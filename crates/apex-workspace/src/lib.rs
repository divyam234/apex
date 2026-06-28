#![forbid(unsafe_code)]

mod collection;
pub use collection::*;
mod environment;
pub use environment::*;
mod reconcile;
pub use reconcile::*;
mod search;
pub use search::*;
mod watch;
pub use watch::*;

use apex_domain::{
    ApiKeyPlacement, Authentication, FormField, HeaderEntry, HttpMethod, HttpRequest,
    MultipartField, MultipartValue, RequestBody, RequestSettings, StableId, ValueSensitivity,
};
use apex_secrets::{LeakFinding, SecretLeakDetector};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceTrust {
    Untrusted,
    Trusted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceManifest {
    pub schema_version: u32,
    pub id: StableId,
    pub name: String,
    pub default_environment: Option<String>,
    pub trust: WorkspaceTrust,
    pub unknown_fields: BTreeMap<String, String>,
}

impl WorkspaceManifest {
    pub fn new(id: StableId, name: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id,
            name: name.into(),
            default_environment: None,
            trust: WorkspaceTrust::Untrusted,
            unknown_fields: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestDocument {
    pub schema_version: u32,
    pub request: HttpRequest,
    pub unknown_fields: BTreeMap<String, String>,
}

impl RequestDocument {
    pub fn new(request: HttpRequest) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            request,
            unknown_fields: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileFingerprint(u64);

impl FileFingerprint {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        Self(hasher.finish())
    }

    pub fn to_hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

#[derive(Clone, Debug)]
pub struct LoadedDocument<T> {
    pub value: T,
    pub path: PathBuf,
    pub fingerprint: FileFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRequestEntry {
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub collection: String,
    pub folders: Vec<String>,
    pub slug: String,
    pub id: StableId,
    pub name: String,
    pub method: HttpMethod,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictMarker {
    pub start_line: usize,
    pub separator_line: Option<usize>,
    pub end_line: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceRepository {
    root: PathBuf,
}

impl WorkspaceRepository {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let root = root.into();
        validate_workspace_path(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn initialize(&self, manifest: &WorkspaceManifest) -> Result<(), WorkspaceError> {
        fs::create_dir_all(self.root.join("environments"))?;
        fs::create_dir_all(self.root.join("collections"))?;
        fs::create_dir_all(self.root.join("schemas"))?;
        fs::create_dir_all(self.root.join("grpc"))?;
        fs::create_dir_all(self.root.join("mocks"))?;
        fs::create_dir_all(self.root.join("profiles"))?;
        self.ensure_local_state_ignored()?;
        self.save_manifest(manifest, None)?;
        Ok(())
    }

    fn ensure_local_state_ignored(&self) -> Result<(), WorkspaceError> {
        let path = self.root.join(".gitignore");
        if path.exists() {
            return Ok(());
        }
        match atomic_write_checked(&path, b".apex/\n", None) {
            Ok(_) => Ok(()),
            Err(WorkspaceError::AlreadyExists(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("apex.toml")
    }

    pub fn load_manifest(&self) -> Result<LoadedDocument<WorkspaceManifest>, WorkspaceError> {
        let path = self.manifest_path();
        let bytes = read_limited(&path, 4 * 1024 * 1024)?;
        let content =
            std::str::from_utf8(&bytes).map_err(|_| WorkspaceError::InvalidUtf8(path.clone()))?;
        detect_conflict_error(&path, content)?;
        let value = parse_manifest(content)?;
        Ok(LoadedDocument {
            value,
            path,
            fingerprint: FileFingerprint::from_bytes(&bytes),
        })
    }

    pub fn save_manifest(
        &self,
        manifest: &WorkspaceManifest,
        expected: Option<FileFingerprint>,
    ) -> Result<FileFingerprint, WorkspaceError> {
        let path = self.manifest_path();
        let content = format_manifest(manifest);
        atomic_write_checked(&path, content.as_bytes(), expected)
    }

    pub fn request_path(
        &self,
        collection_slug: &str,
        request_slug: &str,
    ) -> Result<PathBuf, WorkspaceError> {
        validate_slug(collection_slug)?;
        validate_slug(request_slug)?;
        Ok(self
            .root
            .join("collections")
            .join(collection_slug)
            .join(format!("{request_slug}.request.toml")))
    }

    pub fn load_request(
        &self,
        path: &Path,
    ) -> Result<LoadedDocument<RequestDocument>, WorkspaceError> {
        ensure_inside(&self.root, path)?;
        let bytes = read_limited(path, 32 * 1024 * 1024)?;
        let content = std::str::from_utf8(&bytes)
            .map_err(|_| WorkspaceError::InvalidUtf8(path.to_owned()))?;
        detect_conflict_error(path, content)?;
        let value = parse_request(content)?;
        Ok(LoadedDocument {
            value,
            path: path.to_owned(),
            fingerprint: FileFingerprint::from_bytes(&bytes),
        })
    }

    pub fn save_request(
        &self,
        path: &Path,
        document: &RequestDocument,
        expected: Option<FileFingerprint>,
        leak_detector: &SecretLeakDetector,
    ) -> Result<FileFingerprint, WorkspaceError> {
        ensure_inside(&self.root, path)?;
        let content = format_request(document);
        let findings = leak_detector.scan(&content);
        if !findings.is_empty() {
            return Err(WorkspaceError::SecretLeak(findings));
        }
        atomic_write_checked(path, content.as_bytes(), expected)
    }

    pub fn list_requests(&self) -> Result<Vec<WorkspaceRequestEntry>, WorkspaceError> {
        let collections_root = self.root.join("collections");
        if !collections_root.exists() {
            return Ok(Vec::new());
        }
        let mut requests = Vec::new();
        for path in self.list_request_files_ordered()? {
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(slug) = file_name.strip_suffix(".request.toml").map(str::to_owned) else {
                continue;
            };
            let relative_path = path
                .strip_prefix(&self.root)
                .map_err(|_| WorkspaceError::PathTraversal(path.display().to_string()))?
                .to_owned();
            let collection_relative = path
                .strip_prefix(&collections_root)
                .map_err(|_| WorkspaceError::PathTraversal(path.display().to_string()))?;
            let components = collection_relative
                .parent()
                .into_iter()
                .flat_map(Path::components)
                .filter_map(|component| match component {
                    Component::Normal(value) => value.to_str().map(str::to_owned),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let collection = components
                .first()
                .cloned()
                .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
            let folders = components.into_iter().skip(1).collect();
            let loaded = self.load_request(&path)?;
            requests.push(WorkspaceRequestEntry {
                path,
                relative_path,
                collection,
                folders,
                slug,
                id: loaded.value.request.id,
                name: loaded.value.request.name,
                method: loaded.value.request.method,
                url: loaded.value.request.url,
            });
        }
        Ok(requests)
    }

    pub fn scan_conflicts(&self) -> Result<Vec<(PathBuf, Vec<ConflictMarker>)>, WorkspaceError> {
        let mut conflicts = Vec::new();
        for path in walk_files(&self.root)? {
            if path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                let bytes = read_limited(&path, 32 * 1024 * 1024)?;
                let content = String::from_utf8_lossy(&bytes);
                let markers = detect_conflict_markers(&content);
                if !markers.is_empty() {
                    conflicts.push((path, markers));
                }
            }
        }
        Ok(conflicts)
    }
}

pub fn format_manifest(manifest: &WorkspaceManifest) -> String {
    let mut output = String::new();
    output.push_str(&format!("schema_version = {}\n", manifest.schema_version));
    output.push_str(&format!("workspace_id = {}\n", quote(manifest.id.as_str())));
    output.push_str(&format!("name = {}\n", quote(&manifest.name)));
    if let Some(environment) = &manifest.default_environment {
        output.push_str(&format!("default_environment = {}\n", quote(environment)));
    }
    output.push_str(&format!(
        "trust = {}\n",
        quote(match manifest.trust {
            WorkspaceTrust::Untrusted => "untrusted",
            WorkspaceTrust::Trusted => "trusted",
        })
    ));
    append_unknown_fields(&mut output, &manifest.unknown_fields);
    output
}

pub fn parse_manifest(input: &str) -> Result<WorkspaceManifest, WorkspaceError> {
    let values = parse_flat_document(input)?;
    let schema_version = parse_u32(required(&values, "schema_version")?, "schema_version")?;
    require_supported_version(schema_version)?;
    let id = StableId::parse(parse_string(required(&values, "workspace_id")?)?)
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    let name = parse_string(required(&values, "name")?)?;
    let default_environment = values
        .get("default_environment")
        .map(|value| parse_string(value))
        .transpose()?;
    let trust = match values.get("trust").map(String::as_str) {
        None => WorkspaceTrust::Untrusted,
        Some(value) if parse_string(value)? == "trusted" => WorkspaceTrust::Trusted,
        Some(value) if parse_string(value)? == "untrusted" => WorkspaceTrust::Untrusted,
        Some(value) => {
            return Err(WorkspaceError::InvalidFormat(format!(
                "invalid trust value: {value}"
            )));
        }
    };
    let mut unknown_fields = values;
    for key in [
        "schema_version",
        "workspace_id",
        "name",
        "default_environment",
        "trust",
    ] {
        unknown_fields.remove(key);
    }
    Ok(WorkspaceManifest {
        schema_version,
        id,
        name,
        default_environment,
        trust,
        unknown_fields,
    })
}

pub fn format_request(document: &RequestDocument) -> String {
    let request = &document.request;
    let mut output = String::new();
    output.push_str(&format!("schema_version = {}\n", document.schema_version));
    output.push_str(&format!("id = {}\n", quote(request.id.as_str())));
    output.push_str(&format!("name = {}\n", quote(&request.name)));
    output.push_str(&format!("method = {}\n", quote(request.method.as_str())));
    output.push_str(&format!("url = {}\n", quote(&request.url)));
    output.push_str(&format!(
        "timeout_ms = {}\n",
        request.settings.timeout.as_millis()
    ));
    output.push_str(&format!(
        "connection_timeout_ms = {}\n",
        request.settings.connection_timeout.as_millis()
    ));
    output.push_str(&format!(
        "idle_timeout_ms = {}\n",
        request.settings.idle_timeout.as_millis()
    ));
    output.push_str(&format!(
        "maximum_response_bytes = {}\n",
        request.settings.maximum_response_bytes
    ));
    output.push_str(&format!(
        "maximum_wire_response_bytes = {}\n",
        request.settings.maximum_wire_response_bytes
    ));
    output.push_str(&format!(
        "redirect_limit = {}\n",
        request.settings.redirect_limit
    ));
    output.push_str(&format!(
        "follow_redirects = {}\n",
        request.settings.follow_redirects
    ));
    output.push_str(&format!(
        "verify_certificates = {}\n",
        request.settings.verify_certificates
    ));
    output.push_str(&format!("cookie_jar = {}\n", request.settings.cookie_jar));
    output.push_str(&format!(
        "decompress_response = {}\n",
        request.settings.decompress_response
    ));
    if !request.documentation.is_empty() {
        output.push_str(&format!(
            "documentation = {}\n",
            quote(&request.documentation)
        ));
    }
    append_unknown_fields(&mut output, &document.unknown_fields);
    for query in &request.query {
        output.push_str("\n[[query]]\n");
        output.push_str(&format!("name = {}\n", quote(&query.name)));
        output.push_str(&format!("value = {}\n", quote(&query.value)));
        output.push_str(&format!("enabled = {}\n", query.enabled));
        output.push_str(&format!(
            "sensitivity = {}\n",
            quote(sensitivity_name(query.sensitivity))
        ));
    }
    for header in &request.headers {
        output.push_str("\n[[headers]]\n");
        output.push_str(&format!("name = {}\n", quote(&header.name)));
        output.push_str(&format!("value = {}\n", quote(&header.value)));
        output.push_str(&format!("enabled = {}\n", header.enabled));
        output.push_str(&format!(
            "sensitivity = {}\n",
            quote(sensitivity_name(header.sensitivity))
        ));
    }
    output.push_str("\n[auth]\n");
    output.push_str(&format!(
        "kind = {}\n",
        quote(request.authentication.kind())
    ));
    match &request.authentication {
        Authentication::None => {}
        Authentication::Basic { username, password } => {
            output.push_str(&format!("username = {}\n", quote(username)));
            output.push_str(&format!("password = {}\n", quote(password)));
        }
        Authentication::Bearer { token } => {
            output.push_str(&format!("token = {}\n", quote(token)));
        }
        Authentication::ApiKey {
            name,
            value,
            placement,
        } => {
            output.push_str(&format!("name = {}\n", quote(name)));
            output.push_str(&format!("value = {}\n", quote(value)));
            output.push_str(&format!(
                "placement = {}\n",
                quote(match placement {
                    ApiKeyPlacement::Header => "header",
                    ApiKeyPlacement::Query => "query",
                })
            ));
        }
    }
    output.push_str("\n[body]\n");
    output.push_str(&format!("kind = {}\n", quote(request.body.kind())));
    match &request.body {
        RequestBody::Empty => {}
        RequestBody::Text { content_type, text } => {
            if let Some(content_type) = content_type {
                output.push_str(&format!("content_type = {}\n", quote(content_type)));
            }
            output.push_str(&format!("text = {}\n", quote(text)));
        }
        RequestBody::Json(text) | RequestBody::Xml(text) => {
            output.push_str(&format!("text = {}\n", quote(text)));
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => {
            output.push_str(&format!("query = {}\n", quote(query)));
            output.push_str(&format!("variables_json = {}\n", quote(variables_json)));
            if let Some(operation_name) = operation_name {
                output.push_str(&format!("operation_name = {}\n", quote(operation_name)));
            }
        }
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            output.push_str(&format!("relative_path = {}\n", quote(relative_path)));
        }
        RequestBody::FormUrlEncoded(fields) => {
            output.push_str("encoding_version = 1\n");
            for field in fields {
                output.push_str("\n[[body.fields]]\n");
                output.push_str(&format!("name = {}\n", quote(&field.name)));
                output.push_str(&format!("value = {}\n", quote(&field.value)));
                output.push_str(&format!("enabled = {}\n", field.enabled));
                output.push_str(&format!(
                    "sensitivity = {}\n",
                    quote(sensitivity_name(field.sensitivity))
                ));
            }
        }
        RequestBody::Multipart(fields) => {
            output.push_str("encoding_version = 1\n");
            for field in fields {
                output.push_str("\n[[body.fields]]\n");
                output.push_str(&format!("name = {}\n", quote(&field.name)));
                match &field.value {
                    MultipartValue::Text(value) => {
                        output.push_str("value_kind = \"text\"\n");
                        output.push_str(&format!("value = {}\n", quote(value)));
                    }
                    MultipartValue::File { relative_path } => {
                        output.push_str("value_kind = \"file\"\n");
                        output.push_str(&format!("relative_path = {}\n", quote(relative_path)));
                    }
                }
                if let Some(content_type) = &field.content_type {
                    output.push_str(&format!("content_type = {}\n", quote(content_type)));
                }
                output.push_str(&format!("enabled = {}\n", field.enabled));
                output.push_str(&format!(
                    "sensitivity = {}\n",
                    quote(sensitivity_name(field.sensitivity))
                ));
            }
        }
    }
    output
}

pub fn parse_request(input: &str) -> Result<RequestDocument, WorkspaceError> {
    let mut root_values = BTreeMap::new();
    let mut query = Vec::new();
    let mut headers = Vec::new();
    let mut current_query: Option<BTreeMap<String, String>> = None;
    let mut current_header: Option<BTreeMap<String, String>> = None;
    let mut auth_values = BTreeMap::new();
    let mut body_values = BTreeMap::new();
    let mut body_fields = Vec::new();
    let mut current_body_field: Option<BTreeMap<String, String>> = None;
    let mut section = "root";

    for (line_index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match trimmed {
            "[[query]]" => {
                if let Some(values) = current_body_field.take() {
                    body_fields.push(values);
                }
                if let Some(values) = current_query.take() {
                    query.push(parse_form_field(&values)?);
                }
                if let Some(header) = current_header.take() {
                    headers.push(parse_header(&header)?);
                }
                current_query = Some(BTreeMap::new());
                section = "query";
            }
            "[[headers]]" => {
                if let Some(values) = current_body_field.take() {
                    body_fields.push(values);
                }
                if let Some(values) = current_query.take() {
                    query.push(parse_form_field(&values)?);
                }
                if let Some(header) = current_header.take() {
                    headers.push(parse_header(&header)?);
                }
                current_header = Some(BTreeMap::new());
                section = "header";
            }
            "[auth]" => {
                if let Some(values) = current_body_field.take() {
                    body_fields.push(values);
                }
                if let Some(values) = current_query.take() {
                    query.push(parse_form_field(&values)?);
                }
                if let Some(header) = current_header.take() {
                    headers.push(parse_header(&header)?);
                }
                section = "auth";
            }
            "[body]" => {
                if let Some(values) = current_body_field.take() {
                    body_fields.push(values);
                }
                if let Some(values) = current_query.take() {
                    query.push(parse_form_field(&values)?);
                }
                if let Some(header) = current_header.take() {
                    headers.push(parse_header(&header)?);
                }
                section = "body";
            }
            "[[body.fields]]" => {
                if let Some(values) = current_body_field.take() {
                    body_fields.push(values);
                }
                current_body_field = Some(BTreeMap::new());
                section = "body_field";
            }
            _ => {
                let (key, value) = parse_assignment(trimmed, line_index + 1)?;
                match section {
                    "root" => {
                        root_values.insert(key, value);
                    }
                    "query" => {
                        current_query
                            .as_mut()
                            .ok_or_else(|| {
                                WorkspaceError::InvalidFormat("query section missing".to_owned())
                            })?
                            .insert(key, value);
                    }
                    "header" => {
                        current_header
                            .as_mut()
                            .ok_or_else(|| {
                                WorkspaceError::InvalidFormat("header section missing".to_owned())
                            })?
                            .insert(key, value);
                    }
                    "auth" => {
                        auth_values.insert(key, value);
                    }
                    "body" => {
                        body_values.insert(key, value);
                    }
                    "body_field" => {
                        current_body_field
                            .as_mut()
                            .ok_or_else(|| {
                                WorkspaceError::InvalidFormat(
                                    "body field section missing".to_owned(),
                                )
                            })?
                            .insert(key, value);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
    if let Some(values) = current_body_field.take() {
        body_fields.push(values);
    }
    if let Some(values) = current_query.take() {
        query.push(parse_form_field(&values)?);
    }
    if let Some(header) = current_header.take() {
        headers.push(parse_header(&header)?);
    }

    let schema_version = parse_u32(required(&root_values, "schema_version")?, "schema_version")?;
    require_supported_version(schema_version)?;
    let id = StableId::parse(parse_string(required(&root_values, "id")?)?)
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    let name = parse_string(required(&root_values, "name")?)?;
    let method = HttpMethod::parse(&parse_string(required(&root_values, "method")?)?)
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    let url = parse_string(required(&root_values, "url")?)?;
    let settings = RequestSettings {
        timeout: duration_ms(&root_values, "timeout_ms", 30_000)?,
        connection_timeout: duration_ms(&root_values, "connection_timeout_ms", 10_000)?,
        idle_timeout: duration_ms(&root_values, "idle_timeout_ms", 30_000)?,
        maximum_response_bytes: parse_optional_u64(
            &root_values,
            "maximum_response_bytes",
            64 * 1024 * 1024,
        )?,
        maximum_wire_response_bytes: parse_optional_u64(
            &root_values,
            "maximum_wire_response_bytes",
            64 * 1024 * 1024,
        )?,
        redirect_limit: parse_optional_u16(&root_values, "redirect_limit", 10)?,
        follow_redirects: parse_optional_bool(&root_values, "follow_redirects", true)?,
        verify_certificates: parse_optional_bool(&root_values, "verify_certificates", true)?,
        cookie_jar: parse_optional_bool(&root_values, "cookie_jar", true)?,
        decompress_response: parse_optional_bool(&root_values, "decompress_response", true)?,
    };
    let documentation = root_values
        .get("documentation")
        .map(|value| parse_string(value))
        .transpose()?
        .unwrap_or_default();
    let authentication = parse_authentication(&auth_values)?;
    let body = parse_body(&body_values, &body_fields)?;
    let mut unknown_fields = root_values;
    for key in [
        "schema_version",
        "id",
        "name",
        "method",
        "url",
        "timeout_ms",
        "connection_timeout_ms",
        "idle_timeout_ms",
        "maximum_response_bytes",
        "maximum_wire_response_bytes",
        "redirect_limit",
        "follow_redirects",
        "verify_certificates",
        "cookie_jar",
        "decompress_response",
        "documentation",
    ] {
        unknown_fields.remove(key);
    }
    Ok(RequestDocument {
        schema_version,
        request: HttpRequest {
            id,
            name,
            method,
            url,
            query,
            headers,
            authentication,
            body,
            settings,
            documentation,
        },
        unknown_fields,
    })
}

fn parse_authentication(
    values: &BTreeMap<String, String>,
) -> Result<Authentication, WorkspaceError> {
    let kind = values
        .get("kind")
        .map(|value| parse_string(value))
        .transpose()?
        .unwrap_or_else(|| "none".to_owned());
    match kind.as_str() {
        "none" => Ok(Authentication::None),
        "basic" => Ok(Authentication::Basic {
            username: parse_string(required(values, "username")?)?,
            password: parse_secret_template(values, "password")?,
        }),
        "bearer" => Ok(Authentication::Bearer {
            token: parse_secret_template(values, "token")?,
        }),
        "api_key" => {
            let placement = match parse_string(required(values, "placement")?)?.as_str() {
                "header" => ApiKeyPlacement::Header,
                "query" => ApiKeyPlacement::Query,
                other => {
                    return Err(WorkspaceError::InvalidFormat(format!(
                        "invalid API key placement: {other}"
                    )));
                }
            };
            Ok(Authentication::ApiKey {
                name: parse_string(required(values, "name")?)?,
                value: parse_secret_template(values, "value")?,
                placement,
            })
        }
        other => Err(WorkspaceError::InvalidFormat(format!(
            "unsupported authentication kind: {other}"
        ))),
    }
}

fn parse_secret_template(
    values: &BTreeMap<String, String>,
    key: &str,
) -> Result<String, WorkspaceError> {
    let value = parse_string(required(values, key)?)?;
    if !value.contains("{{") || !value.contains("}}") {
        return Err(WorkspaceError::InvalidFormat(format!(
            "authentication field {key} must reference a variable; plaintext credentials are forbidden"
        )));
    }
    Ok(value)
}

fn parse_form_field(values: &BTreeMap<String, String>) -> Result<FormField, WorkspaceError> {
    Ok(FormField {
        name: parse_string(required(values, "name")?)?,
        value: parse_string(required(values, "value")?)?,
        enabled: parse_optional_bool(values, "enabled", true)?,
        sensitivity: values
            .get("sensitivity")
            .map(|value| parse_sensitivity(&parse_string(value)?))
            .transpose()?
            .unwrap_or(ValueSensitivity::Public),
    })
}

fn parse_header(values: &BTreeMap<String, String>) -> Result<HeaderEntry, WorkspaceError> {
    let mut header = HeaderEntry::new(
        parse_string(required(values, "name")?)?,
        parse_string(required(values, "value")?)?,
    )
    .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    header.enabled = parse_optional_bool(values, "enabled", true)?;
    header.sensitivity = values
        .get("sensitivity")
        .map(|value| parse_sensitivity(&parse_string(value)?))
        .transpose()?
        .unwrap_or(ValueSensitivity::Public);
    Ok(header)
}

fn parse_multipart_field(
    values: &BTreeMap<String, String>,
) -> Result<MultipartField, WorkspaceError> {
    let value_kind = parse_string(required(values, "value_kind")?)?;
    let value = match value_kind.as_str() {
        "text" => MultipartValue::Text(parse_string(required(values, "value")?)?),
        "file" => MultipartValue::File {
            relative_path: validate_relative_resource_path(&parse_string(required(
                values,
                "relative_path",
            )?)?)?,
        },
        other => {
            return Err(WorkspaceError::InvalidFormat(format!(
                "invalid multipart value kind: {other}"
            )));
        }
    };
    Ok(MultipartField {
        name: parse_string(required(values, "name")?)?,
        value,
        content_type: values
            .get("content_type")
            .map(|value| parse_string(value))
            .transpose()?,
        enabled: parse_optional_bool(values, "enabled", true)?,
        sensitivity: values
            .get("sensitivity")
            .map(|value| parse_sensitivity(&parse_string(value)?))
            .transpose()?
            .unwrap_or(ValueSensitivity::Public),
    })
}

fn parse_body(
    values: &BTreeMap<String, String>,
    fields: &[BTreeMap<String, String>],
) -> Result<RequestBody, WorkspaceError> {
    let kind = values
        .get("kind")
        .map(|value| parse_string(value))
        .transpose()?
        .unwrap_or_else(|| "empty".to_owned());
    match kind.as_str() {
        "empty" => Ok(RequestBody::Empty),
        "text" => Ok(RequestBody::Text {
            content_type: values
                .get("content_type")
                .map(|value| parse_string(value))
                .transpose()?,
            text: parse_string(required(values, "text")?)?,
        }),
        "json" => Ok(RequestBody::Json(parse_string(required(values, "text")?)?)),
        "xml" => Ok(RequestBody::Xml(parse_string(required(values, "text")?)?)),
        "graphql" => Ok(RequestBody::GraphQl {
            query: parse_string(required(values, "query")?)?,
            variables_json: values
                .get("variables_json")
                .map(|value| parse_string(value))
                .transpose()?
                .unwrap_or_else(|| "{}".to_owned()),
            operation_name: values
                .get("operation_name")
                .map(|value| parse_string(value))
                .transpose()?,
        }),
        "binary_file" => Ok(RequestBody::BinaryFile {
            relative_path: validate_relative_resource_path(&parse_string(required(
                values,
                "relative_path",
            )?)?)?,
        }),
        "stream_file" => Ok(RequestBody::StreamFile {
            relative_path: validate_relative_resource_path(&parse_string(required(
                values,
                "relative_path",
            )?)?)?,
        }),
        "form_urlencoded" => fields
            .iter()
            .map(parse_form_field)
            .collect::<Result<Vec<_>, _>>()
            .map(RequestBody::FormUrlEncoded),
        "multipart" => fields
            .iter()
            .map(parse_multipart_field)
            .collect::<Result<Vec<_>, _>>()
            .map(RequestBody::Multipart),
        other => Err(WorkspaceError::InvalidFormat(format!(
            "unsupported body kind: {other}"
        ))),
    }
}

fn append_unknown_fields(output: &mut String, fields: &BTreeMap<String, String>) {
    if !fields.is_empty() {
        output.push_str("\n# Forward-compatible fields preserved by ApexAPI.\n");
        for (key, value) in fields {
            output.push_str(&format!("{key} = {value}\n"));
        }
    }
}

fn sensitivity_name(value: ValueSensitivity) -> &'static str {
    match value {
        ValueSensitivity::Public => "public",
        ValueSensitivity::Sensitive => "sensitive",
        ValueSensitivity::Secret => "secret",
    }
}

fn parse_sensitivity(value: &str) -> Result<ValueSensitivity, WorkspaceError> {
    match value {
        "public" => Ok(ValueSensitivity::Public),
        "sensitive" => Ok(ValueSensitivity::Sensitive),
        "secret" => Ok(ValueSensitivity::Secret),
        _ => Err(WorkspaceError::InvalidFormat(format!(
            "invalid sensitivity: {value}"
        ))),
    }
}

fn parse_flat_document(input: &str) -> Result<BTreeMap<String, String>, WorkspaceError> {
    let mut values = BTreeMap::new();
    for (line_index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            return Err(WorkspaceError::InvalidFormat(format!(
                "unexpected section on line {}",
                line_index + 1
            )));
        }
        let (key, value) = parse_assignment(trimmed, line_index + 1)?;
        values.insert(key, value);
    }
    Ok(values)
}

fn parse_assignment(line: &str, line_number: usize) -> Result<(String, String), WorkspaceError> {
    let Some((key, value)) = line.split_once('=') else {
        return Err(WorkspaceError::InvalidFormat(format!(
            "expected key/value on line {line_number}"
        )));
    };
    let key = key.trim();
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(WorkspaceError::InvalidFormat(format!(
            "invalid key on line {line_number}"
        )));
    }
    Ok((key.to_owned(), value.trim().to_owned()))
}

fn required<'a>(
    values: &'a BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, WorkspaceError> {
    values
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| WorkspaceError::InvalidFormat(format!("missing required field: {key}")))
}

fn parse_string(value: &str) -> Result<String, WorkspaceError> {
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Err(WorkspaceError::InvalidFormat(format!(
            "expected quoted string: {value}"
        )));
    }
    let mut output = String::new();
    let mut escaped = false;
    for character in value[1..value.len() - 1].chars() {
        if escaped {
            match character {
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                '\\' => output.push('\\'),
                '"' => output.push('"'),
                other => {
                    return Err(WorkspaceError::InvalidFormat(format!(
                        "unsupported string escape: \\{other}"
                    )));
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
    }
    if escaped {
        return Err(WorkspaceError::InvalidFormat(
            "trailing string escape".to_owned(),
        ));
    }
    Ok(output)
}

fn quote(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

fn parse_u32(value: &str, key: &str) -> Result<u32, WorkspaceError> {
    value
        .parse()
        .map_err(|_| WorkspaceError::InvalidFormat(format!("{key} must be an unsigned integer")))
}

fn parse_optional_u64(
    values: &BTreeMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64, WorkspaceError> {
    values.get(key).map_or(Ok(default), |value| {
        value.parse().map_err(|_| {
            WorkspaceError::InvalidFormat(format!("{key} must be an unsigned integer"))
        })
    })
}

fn parse_optional_u16(
    values: &BTreeMap<String, String>,
    key: &str,
    default: u16,
) -> Result<u16, WorkspaceError> {
    values.get(key).map_or(Ok(default), |value| {
        value.parse().map_err(|_| {
            WorkspaceError::InvalidFormat(format!("{key} must be an unsigned integer"))
        })
    })
}

fn parse_optional_bool(
    values: &BTreeMap<String, String>,
    key: &str,
    default: bool,
) -> Result<bool, WorkspaceError> {
    values
        .get(key)
        .map_or(Ok(default), |value| match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(WorkspaceError::InvalidFormat(format!(
                "{key} must be true or false"
            ))),
        })
}

fn duration_ms(
    values: &BTreeMap<String, String>,
    key: &str,
    default: u64,
) -> Result<std::time::Duration, WorkspaceError> {
    Ok(std::time::Duration::from_millis(parse_optional_u64(
        values, key, default,
    )?))
}

fn require_supported_version(version: u32) -> Result<(), WorkspaceError> {
    if version == CURRENT_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(WorkspaceError::UnsupportedSchemaVersion {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        })
    }
}

fn validate_workspace_path(path: &Path) -> Result<(), WorkspaceError> {
    if path.as_os_str().is_empty() {
        Err(WorkspaceError::InvalidPath(
            "workspace path is empty".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_slug(slug: &str) -> Result<(), WorkspaceError> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(WorkspaceError::InvalidPath(format!(
            "invalid workspace slug: {slug}"
        )))
    } else {
        Ok(())
    }
}

fn validate_relative_resource_path(value: &str) -> Result<String, WorkspaceError> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        Err(WorkspaceError::PathTraversal(value.to_owned()))
    } else {
        Ok(value.to_owned())
    }
}

fn ensure_inside(root: &Path, path: &Path) -> Result<(), WorkspaceError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| WorkspaceError::PathTraversal(path.display().to_string()))?;
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        Err(WorkspaceError::PathTraversal(path.display().to_string()))
    } else {
        Ok(())
    }
}

fn read_limited(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, WorkspaceError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > maximum_bytes {
        return Err(WorkspaceError::FileTooLarge {
            path: path.to_owned(),
            maximum_bytes,
            observed_bytes: metadata.len(),
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn atomic_write_checked(
    path: &Path,
    bytes: &[u8],
    expected: Option<FileFingerprint>,
) -> Result<FileFingerprint, WorkspaceError> {
    if let Some(expected) = expected {
        let current = fs::read(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                WorkspaceError::ExternalChange(path.to_owned())
            } else {
                WorkspaceError::Io(error)
            }
        })?;
        if FileFingerprint::from_bytes(&current) != expected {
            return Err(WorkspaceError::ExternalChange(path.to_owned()));
        }
    } else if path.exists() {
        return Err(WorkspaceError::AlreadyExists(path.to_owned()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
    let temporary = parent.join(format!(".{file_name}.{nonce}.tmp"));
    let result = (|| -> Result<(), WorkspaceError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = File::open(parent) {
            directory.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(FileFingerprint::from_bytes(bytes))
}

pub fn detect_conflict_markers(content: &str) -> Vec<ConflictMarker> {
    let mut markers = Vec::new();
    let mut current: Option<ConflictMarker> = None;
    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        if line.starts_with("<<<<<<< ") {
            if let Some(marker) = current.take() {
                markers.push(marker);
            }
            current = Some(ConflictMarker {
                start_line: line_number,
                separator_line: None,
                end_line: None,
            });
        } else if line == "=======" {
            if let Some(marker) = &mut current {
                marker.separator_line = Some(line_number);
            }
        } else if line.starts_with(">>>>>>> ")
            && let Some(mut marker) = current.take()
        {
            marker.end_line = Some(line_number);
            markers.push(marker);
        }
    }
    if let Some(marker) = current {
        markers.push(marker);
    }
    markers
}

fn detect_conflict_error(path: &Path, content: &str) -> Result<(), WorkspaceError> {
    let markers = detect_conflict_markers(content);
    if markers.is_empty() {
        Ok(())
    } else {
        Err(WorkspaceError::MergeConflict {
            path: path.to_owned(),
            markers,
        })
    }
}

fn walk_files(root: &Path) -> Result<Vec<PathBuf>, WorkspaceError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[derive(Debug)]
pub enum WorkspaceError {
    Io(std::io::Error),
    InvalidUtf8(PathBuf),
    InvalidFormat(String),
    InvalidPath(String),
    PathTraversal(String),
    SymbolicLink(PathBuf),
    AlreadyExists(PathBuf),
    ExternalChange(PathBuf),
    MergeConflict {
        path: PathBuf,
        markers: Vec<ConflictMarker>,
    },
    UnsupportedSchemaVersion {
        found: u32,
        supported: u32,
    },
    FileTooLarge {
        path: PathBuf,
        maximum_bytes: u64,
        observed_bytes: u64,
    },
    SecretLeak(Vec<LeakFinding>),
    SecretResolution(String),
}

impl Display for WorkspaceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "workspace I/O failed: {error}"),
            Self::InvalidUtf8(path) => {
                write!(formatter, "workspace file is not UTF-8: {}", path.display())
            }
            Self::InvalidFormat(detail) => write!(formatter, "invalid workspace format: {detail}"),
            Self::InvalidPath(detail) => write!(formatter, "invalid workspace path: {detail}"),
            Self::PathTraversal(detail) => {
                write!(formatter, "workspace path traversal rejected: {detail}")
            }
            Self::SymbolicLink(path) => write!(
                formatter,
                "symbolic links are not allowed in workspace mutations: {}",
                path.display()
            ),
            Self::AlreadyExists(path) => write!(
                formatter,
                "refusing to overwrite existing workspace resource: {}",
                path.display()
            ),
            Self::ExternalChange(path) => write!(
                formatter,
                "file changed outside ApexAPI: {}",
                path.display()
            ),
            Self::MergeConflict { path, markers } => write!(
                formatter,
                "merge conflict markers found in {} at {} location(s)",
                path.display(),
                markers.len()
            ),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                formatter,
                "workspace schema version {found} is unsupported; current version is {supported}"
            ),
            Self::FileTooLarge {
                path,
                maximum_bytes,
                observed_bytes,
            } => write!(
                formatter,
                "workspace file {} is {observed_bytes} bytes, exceeding {maximum_bytes} bytes",
                path.display()
            ),
            Self::SecretLeak(findings) => write!(
                formatter,
                "refusing to save workspace file with {} potential secret leak(s)",
                findings.len()
            ),
            Self::SecretResolution(detail) => {
                write!(formatter, "secret resolution failed: {detail}")
            }
        }
    }
}

impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for WorkspaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("apex-{name}-{nonce}"))
    }

    fn sample_request() -> HttpRequest {
        HttpRequest {
            id: StableId::parse("get-user").expect("valid id"),
            name: "Get user".to_owned(),
            method: HttpMethod::Get,
            url: "https://{{host}}/users/{{user_id}}".to_owned(),
            query: vec![
                FormField {
                    name: "tag".to_owned(),
                    value: "one".to_owned(),
                    enabled: true,
                    sensitivity: ValueSensitivity::Public,
                },
                FormField {
                    name: "tag".to_owned(),
                    value: "two".to_owned(),
                    enabled: true,
                    sensitivity: ValueSensitivity::Sensitive,
                },
            ],
            headers: vec![
                HeaderEntry::new("Accept", "application/json").expect("valid header"),
                HeaderEntry::new("X-Trace", "one").expect("valid header"),
                HeaderEntry::new("X-Trace", "two").expect("valid header"),
            ],
            authentication: Authentication::None,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: "Fetch one user.".to_owned(),
        }
    }

    #[test]
    fn manifest_round_trip_is_stable() {
        let mut manifest =
            WorkspaceManifest::new(StableId::parse("workspace-1").expect("valid id"), "Demo");
        manifest.default_environment = Some("development".to_owned());
        manifest
            .unknown_fields
            .insert("future_option".to_owned(), "\"preserved\"".to_owned());
        let first = format_manifest(&manifest);
        let parsed = parse_manifest(&first).expect("parses");
        assert_eq!(format_manifest(&parsed), first);
    }

    #[test]
    fn request_round_trip_preserves_duplicate_query_and_headers() {
        let document = RequestDocument::new(sample_request());
        let formatted = format_request(&document);
        let parsed = parse_request(&formatted).expect("parses");
        assert_eq!(
            parsed
                .request
                .query
                .iter()
                .filter(|field| field.name == "tag")
                .map(|field| field.value.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(
            parsed.request.query[1].sensitivity,
            ValueSensitivity::Sensitive
        );
        assert_eq!(
            parsed.request.header_values("x-trace").collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(format_request(&parsed), formatted);
    }

    #[test]
    fn request_round_trip_preserves_secret_references_for_authentication() {
        let mut request = sample_request();
        request.authentication = Authentication::Bearer {
            token: "{{access_token}}".to_owned(),
        };
        let document = RequestDocument::new(request);
        let formatted = format_request(&document);
        let parsed = parse_request(&formatted).expect("auth request parses");
        assert_eq!(parsed, document);
        assert_eq!(format_request(&parsed), formatted);
    }

    #[test]
    fn rejects_plaintext_authentication_credentials() {
        let input = r#"
schema_version = 1
id = "plaintext-auth"
name = "Plaintext auth"
method = "GET"
url = "https://example.test"

[auth]
kind = "bearer"
token = "literal-secret"

[body]
kind = "empty"
"#;
        let error = parse_request(input).expect_err("plaintext auth must be rejected");
        assert!(matches!(error, WorkspaceError::InvalidFormat(_)));
        assert!(
            error
                .to_string()
                .contains("plaintext credentials are forbidden")
        );
    }

    #[test]
    fn initialization_creates_local_state_ignore_without_overwriting_existing_file() {
        let root = temporary_directory("gitignore");
        let repository = WorkspaceRepository::new(&root).expect("valid repository");
        let manifest =
            WorkspaceManifest::new(StableId::parse("workspace-1").expect("valid id"), "Demo");
        repository.initialize(&manifest).expect("initializes");
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).expect("gitignore"),
            ".apex/\n"
        );
        fs::remove_dir_all(&root).expect("cleanup");

        let existing_root = temporary_directory("existing-gitignore");
        fs::create_dir_all(&existing_root).expect("root");
        fs::write(existing_root.join(".gitignore"), "target/\n").expect("existing ignore");
        let repository = WorkspaceRepository::new(&existing_root).expect("valid repository");
        repository.initialize(&manifest).expect("initializes");
        assert_eq!(
            fs::read_to_string(existing_root.join(".gitignore")).expect("gitignore"),
            "target/\n"
        );
        fs::remove_dir_all(existing_root).expect("cleanup");
    }

    #[test]
    fn request_round_trip_preserves_form_fields() {
        let mut request = sample_request();
        request.body = RequestBody::FormUrlEncoded(vec![
            FormField {
                name: "tag".to_owned(),
                value: "one".to_owned(),
                enabled: true,
                sensitivity: ValueSensitivity::Public,
            },
            FormField {
                name: "tag".to_owned(),
                value: "two".to_owned(),
                enabled: false,
                sensitivity: ValueSensitivity::Sensitive,
            },
        ]);
        let document = RequestDocument::new(request);
        let formatted = format_request(&document);
        let parsed = parse_request(&formatted).expect("form request parses");
        assert_eq!(parsed, document);
        assert_eq!(format_request(&parsed), formatted);
    }

    #[test]
    fn request_round_trip_preserves_multipart_file_fields() {
        let mut request = sample_request();
        request.body = RequestBody::Multipart(vec![
            MultipartField {
                name: "metadata".to_owned(),
                value: MultipartValue::Text("{\"kind\":\"avatar\"}".to_owned()),
                content_type: Some("application/json".to_owned()),
                enabled: true,
                sensitivity: ValueSensitivity::Public,
            },
            MultipartField {
                name: "file".to_owned(),
                value: MultipartValue::File {
                    relative_path: "fixtures/avatar.png".to_owned(),
                },
                content_type: Some("image/png".to_owned()),
                enabled: true,
                sensitivity: ValueSensitivity::Sensitive,
            },
        ]);
        let document = RequestDocument::new(request);
        let formatted = format_request(&document);
        let parsed = parse_request(&formatted).expect("multipart request parses");
        assert_eq!(parsed, document);
        assert_eq!(format_request(&parsed), formatted);
    }

    #[test]
    fn request_index_lists_nested_collections_in_stable_order() {
        let root = temporary_directory("request-index");
        let repository = WorkspaceRepository::new(&root).expect("valid repository");
        let manifest =
            WorkspaceManifest::new(StableId::parse("workspace-1").expect("valid id"), "Demo");
        repository.initialize(&manifest).expect("initializes");

        let first_path = root
            .join("collections")
            .join("users")
            .join("admin")
            .join("get-user.request.toml");
        repository
            .save_request(
                &first_path,
                &RequestDocument::new(sample_request()),
                None,
                &SecretLeakDetector::default(),
            )
            .expect("saves nested request");

        let mut second = sample_request();
        second.id = StableId::parse("create-user").expect("valid id");
        second.name = "Create user".to_owned();
        second.method = HttpMethod::Post;
        let second_path = repository
            .request_path("users", "create-user")
            .expect("valid path");
        repository
            .save_request(
                &second_path,
                &RequestDocument::new(second),
                None,
                &SecretLeakDetector::default(),
            )
            .expect("saves request");

        let entries = repository.list_requests().expect("indexes requests");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].slug, "get-user");
        assert_eq!(entries[0].collection, "users");
        assert_eq!(entries[0].folders, ["admin"]);
        assert_eq!(entries[1].slug, "create-user");
        assert_eq!(entries[1].method, HttpMethod::Post);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn atomic_save_detects_external_edits() {
        let root = temporary_directory("conflict");
        let repository = WorkspaceRepository::new(&root).expect("valid repository");
        let manifest =
            WorkspaceManifest::new(StableId::parse("workspace-1").expect("valid id"), "Demo");
        repository.initialize(&manifest).expect("initializes");
        let loaded = repository.load_manifest().expect("loads");
        fs::write(repository.manifest_path(), "external = true\n").expect("external write");
        let error = repository
            .save_manifest(&loaded.value, Some(loaded.fingerprint))
            .expect_err("must detect conflict");
        assert!(matches!(error, WorkspaceError::ExternalChange(_)));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn refuses_plaintext_secrets() {
        let root = temporary_directory("secret");
        let repository = WorkspaceRepository::new(&root).expect("valid repository");
        let manifest =
            WorkspaceManifest::new(StableId::parse("workspace-1").expect("valid id"), "Demo");
        repository.initialize(&manifest).expect("initializes");
        let path = repository
            .request_path("users", "get-user")
            .expect("valid path");
        let mut document = RequestDocument::new(sample_request());
        document
            .unknown_fields
            .insert("api_key".to_owned(), "\"plaintext-secret\"".to_owned());
        let error = repository
            .save_request(&path, &document, None, &SecretLeakDetector::default())
            .expect_err("must reject secret");
        assert!(matches!(error, WorkspaceError::SecretLeak(_)));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn detects_merge_conflicts() {
        let markers =
            detect_conflict_markers("<<<<<<< ours\na = 1\n=======\na = 2\n>>>>>>> theirs\n");
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].start_line, 1);
        assert_eq!(markers[0].separator_line, Some(3));
        assert_eq!(markers[0].end_line, Some(5));
    }

    #[test]
    fn rejects_resource_path_traversal() {
        let input = r#"
schema_version = 1
id = "upload"
name = "Upload"
method = "POST"
url = "https://example.test"

[body]
kind = "binary_file"
relative_path = "../../secret"
"#;
        assert!(matches!(
            parse_request(input),
            Err(WorkspaceError::PathTraversal(_))
        ));
    }
}
