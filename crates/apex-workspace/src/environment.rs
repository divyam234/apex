use super::{
    CURRENT_SCHEMA_VERSION, FileFingerprint, LoadedDocument, WorkspaceError, WorkspaceRepository,
    append_unknown_fields, atomic_write_checked, detect_conflict_error, parse_assignment,
    parse_sensitivity, parse_string, parse_u32, quote, read_limited, sensitivity_name,
};
use apex_domain::{StableId, ValueSensitivity, VariableDefinition, VariableValue};
use apex_secrets::{SecretRef, SecretStoreChain};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAXIMUM_VARIABLE_SET_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum StoredVariableSource {
    Literal(VariableValue),
    Secret(SecretRef),
    ProcessEnvironment { name: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredVariable {
    pub name: String,
    pub source: StoredVariableSource,
    pub sensitivity: ValueSensitivity,
    pub enabled: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableSetDocument {
    pub schema_version: u32,
    pub id: StableId,
    pub name: String,
    pub variables: Vec<StoredVariable>,
    pub unknown_fields: BTreeMap<String, String>,
}

impl VariableSetDocument {
    pub fn new(id: StableId, name: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id,
            name: name.into(),
            variables: Vec::new(),
            unknown_fields: BTreeMap::new(),
        }
    }

    pub fn resolve(
        &self,
        secret_stores: Option<&SecretStoreChain>,
    ) -> Result<BTreeMap<String, VariableDefinition>, WorkspaceError> {
        let mut resolved = BTreeMap::new();
        for variable in &self.variables {
            validate_variable_name(&variable.name)?;
            let (value, source_description, source_sensitivity) = match &variable.source {
                StoredVariableSource::Literal(value) => {
                    if variable.sensitivity == ValueSensitivity::Secret {
                        return Err(WorkspaceError::SecretResolution(format!(
                            "variable '{}' is marked secret but contains a literal value",
                            variable.name
                        )));
                    }
                    (
                        value.clone(),
                        "workspace variable file".to_owned(),
                        variable.sensitivity,
                    )
                }
                StoredVariableSource::Secret(reference) => {
                    let stores = secret_stores.ok_or_else(|| {
                        WorkspaceError::SecretResolution(format!(
                            "secret store is unavailable for {}",
                            reference.display_name()
                        ))
                    })?;
                    let secret = stores
                        .resolve(reference)
                        .map_err(|error| WorkspaceError::SecretResolution(error.to_string()))?;
                    let value = secret
                        .value
                        .expose()
                        .map_err(|error| WorkspaceError::SecretResolution(error.to_string()))?;
                    (
                        VariableValue::String(value.to_owned()),
                        format!(
                            "secret {} from {}",
                            reference.display_name(),
                            secret.source_store
                        ),
                        ValueSensitivity::Secret,
                    )
                }
                StoredVariableSource::ProcessEnvironment { name } => {
                    validate_process_environment_name(name)?;
                    let value = env::var(name).map_err(|_| {
                        WorkspaceError::SecretResolution(format!(
                            "process environment variable {name} is unavailable"
                        ))
                    })?;
                    (
                        VariableValue::String(value),
                        format!("process environment {name}"),
                        variable.sensitivity,
                    )
                }
            };
            let description = variable
                .description
                .as_ref()
                .map_or(source_description.clone(), |description| {
                    format!("{description} ({source_description})")
                });
            if resolved
                .insert(
                    variable.name.clone(),
                    VariableDefinition {
                        value,
                        sensitivity: strongest_sensitivity(
                            variable.sensitivity,
                            source_sensitivity,
                        ),
                        enabled: variable.enabled,
                        description: Some(description),
                    },
                )
                .is_some()
            {
                return Err(WorkspaceError::InvalidFormat(format!(
                    "duplicate variable name: {}",
                    variable.name
                )));
            }
        }
        Ok(resolved)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentSummary {
    pub id: StableId,
    pub name: String,
    pub path: PathBuf,
    pub variable_count: usize,
    pub has_local_override: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentDeletionReceipt {
    pub id: StableId,
    pub cleanup_pending: Option<PathBuf>,
}

impl WorkspaceRepository {
    pub fn workspace_variables_path(&self) -> PathBuf {
        self.root.join("variables.toml")
    }

    pub fn environment_path(&self, id: &StableId) -> PathBuf {
        self.root
            .join("environments")
            .join(format!("{}.toml", id.as_str()))
    }

    pub fn local_environment_override_path(&self, id: &StableId) -> PathBuf {
        self.root
            .join(".apex")
            .join("environments")
            .join(format!("{}.local.toml", id.as_str()))
    }

    pub fn create_environment(
        &self,
        document: &VariableSetDocument,
    ) -> Result<FileFingerprint, WorkspaceError> {
        validate_variable_set_identity(document)?;
        self.save_variable_set(&self.environment_path(&document.id), document, None)
    }

    pub fn update_environment(
        &self,
        document: &VariableSetDocument,
        expected: FileFingerprint,
    ) -> Result<FileFingerprint, WorkspaceError> {
        validate_variable_set_identity(document)?;
        self.save_variable_set(
            &self.environment_path(&document.id),
            document,
            Some(expected),
        )
    }

    pub fn rename_environment(
        &self,
        id: &StableId,
        name: impl Into<String>,
        expected: FileFingerprint,
    ) -> Result<FileFingerprint, WorkspaceError> {
        let path = self.environment_path(id);
        ensure_file_snapshot(&path, expected)?;
        let loaded = self.load_environment(id)?;
        let mut document = loaded.value;
        document.name = name.into();
        validate_variable_set_identity(&document)?;
        self.save_variable_set(&loaded.path, &document, Some(expected))
    }

    pub fn save_local_environment_override(
        &self,
        environment_id: &StableId,
        document: &VariableSetDocument,
        expected: Option<FileFingerprint>,
    ) -> Result<FileFingerprint, WorkspaceError> {
        if &document.id != environment_id {
            return Err(WorkspaceError::InvalidFormat(format!(
                "local override id '{}' must match environment id '{}'",
                document.id, environment_id
            )));
        }
        self.load_environment(environment_id)?;
        validate_variable_set_identity(document)?;
        self.save_variable_set(
            &self.local_environment_override_path(environment_id),
            document,
            expected,
        )
    }

    pub fn delete_local_environment_override(
        &self,
        environment_id: &StableId,
        expected: FileFingerprint,
    ) -> Result<(), WorkspaceError> {
        delete_file_checked(
            &self.local_environment_override_path(environment_id),
            expected,
        )
    }

    pub fn set_default_environment(
        &self,
        environment_id: Option<&StableId>,
        expected_manifest: FileFingerprint,
    ) -> Result<FileFingerprint, WorkspaceError> {
        if let Some(id) = environment_id {
            self.load_environment(id)?;
        }
        let loaded = self.load_manifest()?;
        if loaded.fingerprint != expected_manifest {
            return Err(WorkspaceError::ExternalChange(loaded.path));
        }
        let mut manifest = loaded.value;
        manifest.default_environment = environment_id.map(|id| id.as_str().to_owned());
        self.save_manifest(&manifest, Some(expected_manifest))
    }

    pub fn delete_environment(
        &self,
        id: &StableId,
        expected: FileFingerprint,
    ) -> Result<EnvironmentDeletionReceipt, WorkspaceError> {
        let manifest = self.load_manifest()?;
        if manifest.value.default_environment.as_deref() == Some(id.as_str()) {
            return Err(WorkspaceError::InvalidFormat(format!(
                "environment '{id}' is the workspace default; select another default before deletion"
            )));
        }
        let environment_path = self.environment_path(id);
        ensure_file_snapshot(&environment_path, expected)?;
        let environment = self.load_environment(id)?;
        let local = self.load_local_environment_override(id)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let trash = self
            .root
            .join(".apex")
            .join("trash")
            .join("environments")
            .join(format!("{}-{nonce}", id.as_str()));
        fs::create_dir_all(&trash)?;
        let environment_trash = trash.join("environment.toml");
        fs::rename(&environment.path, &environment_trash)?;
        let local_trash = local.as_ref().map(|_| trash.join("local.toml"));
        if let (Some(local), Some(local_trash)) = (&local, &local_trash)
            && let Err(error) = fs::rename(&local.path, local_trash)
        {
            let restore = fs::rename(&environment_trash, &environment.path);
            let _ = fs::remove_dir_all(&trash);
            let _ = sync_parent(&environment.path);
            return match restore {
                Ok(()) => Err(WorkspaceError::Io(error)),
                Err(restore_error) => Err(WorkspaceError::InvalidFormat(format!(
                    "local override deletion failed ({error}) and environment rollback failed ({restore_error}); recover {}",
                    trash.display()
                ))),
            };
        }
        let durability = (|| -> Result<(), WorkspaceError> {
            sync_parent(&environment.path)?;
            if let Some(local) = &local {
                sync_parent(&local.path)?;
            }
            sync_directory(&trash)
        })();
        if let Err(error) = durability {
            let local_restore = match (&local, &local_trash) {
                (Some(local), Some(local_trash)) => fs::rename(local_trash, &local.path),
                _ => Ok(()),
            };
            let environment_restore = fs::rename(&environment_trash, &environment.path);
            let _ = fs::remove_dir_all(&trash);
            let _ = sync_parent(&environment.path);
            if let Some(local) = &local {
                let _ = sync_parent(&local.path);
            }
            return match (local_restore, environment_restore) {
                (Ok(()), Ok(())) => Err(error),
                (local_result, environment_result) => Err(WorkspaceError::InvalidFormat(format!(
                    "environment deletion durability failed ({error}); rollback results: local={local_result:?}, environment={environment_result:?}; recover {}",
                    trash.display()
                ))),
            };
        }
        let cleanup_pending = match fs::remove_dir_all(&trash) {
            Ok(()) => None,
            Err(_) => Some(trash),
        };
        Ok(EnvironmentDeletionReceipt {
            id: id.clone(),
            cleanup_pending,
        })
    }

    pub fn load_workspace_variables(
        &self,
    ) -> Result<Option<LoadedDocument<VariableSetDocument>>, WorkspaceError> {
        let path = self.workspace_variables_path();
        if !path.exists() {
            return Ok(None);
        }
        self.load_variable_set(&path).map(Some)
    }

    pub fn load_environment(
        &self,
        id: &StableId,
    ) -> Result<LoadedDocument<VariableSetDocument>, WorkspaceError> {
        self.load_variable_set(&self.environment_path(id))
    }

    pub fn load_local_environment_override(
        &self,
        id: &StableId,
    ) -> Result<Option<LoadedDocument<VariableSetDocument>>, WorkspaceError> {
        let path = self.local_environment_override_path(id);
        if !path.exists() {
            return Ok(None);
        }
        self.load_variable_set(&path).map(Some)
    }

    pub fn load_variable_set(
        &self,
        path: &Path,
    ) -> Result<LoadedDocument<VariableSetDocument>, WorkspaceError> {
        let bytes = read_limited(path, MAXIMUM_VARIABLE_SET_BYTES)?;
        let content = std::str::from_utf8(&bytes)
            .map_err(|_| WorkspaceError::InvalidUtf8(path.to_owned()))?;
        detect_conflict_error(path, content)?;
        let value = parse_variable_set(content)?;
        Ok(LoadedDocument {
            value,
            path: path.to_owned(),
            fingerprint: FileFingerprint::from_bytes(&bytes),
        })
    }

    pub fn save_variable_set(
        &self,
        path: &Path,
        document: &VariableSetDocument,
        expected: Option<FileFingerprint>,
    ) -> Result<FileFingerprint, WorkspaceError> {
        let content = format_variable_set(document)?;
        atomic_write_checked(path, content.as_bytes(), expected)
    }

    pub fn list_environments(&self) -> Result<Vec<EnvironmentSummary>, WorkspaceError> {
        let directory = self.root.join("environments");
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut summaries = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("toml")
            {
                continue;
            }
            let loaded = self.load_variable_set(&path)?;
            let has_local_override = self
                .local_environment_override_path(&loaded.value.id)
                .exists();
            summaries.push(EnvironmentSummary {
                id: loaded.value.id,
                name: loaded.value.name,
                path,
                variable_count: loaded.value.variables.len(),
                has_local_override,
            });
        }
        summaries.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(summaries)
    }
}

pub fn format_variable_set(document: &VariableSetDocument) -> Result<String, WorkspaceError> {
    validate_variable_set_identity(document)?;
    if document.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(WorkspaceError::UnsupportedSchemaVersion {
            found: document.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    let mut names = BTreeSet::new();
    let mut output = String::new();
    output.push_str(&format!("schema_version = {}\n", document.schema_version));
    output.push_str(&format!("id = {}\n", quote(document.id.as_str())));
    output.push_str(&format!("name = {}\n", quote(&document.name)));
    append_unknown_fields(&mut output, &document.unknown_fields);

    for variable in &document.variables {
        validate_variable_name(&variable.name)?;
        if !names.insert(variable.name.clone()) {
            return Err(WorkspaceError::InvalidFormat(format!(
                "duplicate variable name: {}",
                variable.name
            )));
        }
        if matches!(variable.source, StoredVariableSource::Literal(_))
            && variable.sensitivity == ValueSensitivity::Secret
        {
            return Err(WorkspaceError::SecretResolution(format!(
                "variable '{}' is marked secret but contains a literal value",
                variable.name
            )));
        }
        if matches!(variable.source, StoredVariableSource::Secret(_))
            && variable.sensitivity != ValueSensitivity::Secret
        {
            return Err(WorkspaceError::InvalidFormat(format!(
                "variable '{}' uses a secret source and must use secret sensitivity",
                variable.name
            )));
        }
        output.push_str("\n[[variables]]\n");
        output.push_str(&format!("name = {}\n", quote(&variable.name)));
        output.push_str(&format!("enabled = {}\n", variable.enabled));
        output.push_str(&format!(
            "sensitivity = {}\n",
            quote(sensitivity_name(variable.sensitivity))
        ));
        if let Some(description) = &variable.description {
            output.push_str(&format!("description = {}\n", quote(description)));
        }
        match &variable.source {
            StoredVariableSource::Literal(value) => {
                output.push_str("source = \"literal\"\n");
                format_literal_value(&mut output, value)?;
            }
            StoredVariableSource::Secret(reference) => {
                output.push_str("source = \"secret\"\n");
                output.push_str(&format!(
                    "secret_namespace = {}\n",
                    quote(&reference.namespace)
                ));
                output.push_str(&format!("secret_name = {}\n", quote(&reference.name)));
            }
            StoredVariableSource::ProcessEnvironment { name } => {
                validate_process_environment_name(name)?;
                output.push_str("source = \"process_environment\"\n");
                output.push_str(&format!("environment_name = {}\n", quote(name)));
            }
        }
    }
    Ok(output)
}

pub fn parse_variable_set(input: &str) -> Result<VariableSetDocument, WorkspaceError> {
    let mut root = BTreeMap::new();
    let mut raw_variables = Vec::new();
    let mut current: Option<BTreeMap<String, String>> = None;
    for (line_index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[[variables]]" {
            if let Some(values) = current.take() {
                raw_variables.push(values);
            }
            current = Some(BTreeMap::new());
            continue;
        }
        if trimmed.starts_with('[') {
            return Err(WorkspaceError::InvalidFormat(format!(
                "unsupported variable-set section on line {}",
                line_index + 1
            )));
        }
        let (key, value) = parse_assignment(trimmed, line_index + 1)?;
        if let Some(current) = current.as_mut() {
            current.insert(key, value);
        } else {
            root.insert(key, value);
        }
    }
    if let Some(values) = current {
        raw_variables.push(values);
    }

    let schema_version = parse_u32(required(&root, "schema_version")?, "schema_version")?;
    if schema_version != CURRENT_SCHEMA_VERSION {
        return Err(WorkspaceError::UnsupportedSchemaVersion {
            found: schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    let id = StableId::parse(parse_string(required(&root, "id")?)?)
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    let name = parse_string(required(&root, "name")?)?;
    validate_variable_set_name(&name)?;
    let unknown_fields = root
        .into_iter()
        .filter(|(key, _)| !matches!(key.as_str(), "schema_version" | "id" | "name"))
        .collect();
    let mut variables = Vec::with_capacity(raw_variables.len());
    let mut names = BTreeSet::new();
    for values in raw_variables {
        let variable = parse_stored_variable(&values)?;
        if !names.insert(variable.name.clone()) {
            return Err(WorkspaceError::InvalidFormat(format!(
                "duplicate variable name: {}",
                variable.name
            )));
        }
        variables.push(variable);
    }
    Ok(VariableSetDocument {
        schema_version,
        id,
        name,
        variables,
        unknown_fields,
    })
}

fn parse_stored_variable(
    values: &BTreeMap<String, String>,
) -> Result<StoredVariable, WorkspaceError> {
    let name = parse_string(required(values, "name")?)?;
    validate_variable_name(&name)?;
    let enabled = values
        .get("enabled")
        .map_or(Ok(true), |value| parse_bool(value, "enabled"))?;
    let sensitivity = values
        .get("sensitivity")
        .map_or(Ok(ValueSensitivity::Public), |value| {
            parse_string(value).and_then(|value| parse_sensitivity(&value))
        })?;
    let description = values
        .get("description")
        .map(|value| parse_string(value))
        .transpose()?;
    let source = values
        .get("source")
        .map(|value| parse_string(value))
        .transpose()?
        .unwrap_or_else(|| "literal".to_owned());
    let source = match source.as_str() {
        "literal" => {
            if sensitivity == ValueSensitivity::Secret {
                return Err(WorkspaceError::SecretResolution(format!(
                    "variable '{name}' is marked secret but contains a literal value"
                )));
            }
            StoredVariableSource::Literal(parse_literal_value(values)?)
        }
        "secret" => {
            if sensitivity != ValueSensitivity::Secret {
                return Err(WorkspaceError::InvalidFormat(format!(
                    "variable '{name}' uses a secret source and must use secret sensitivity"
                )));
            }
            let namespace = parse_string(required(values, "secret_namespace")?)?;
            let secret_name = parse_string(required(values, "secret_name")?)?;
            StoredVariableSource::Secret(
                SecretRef::new(namespace, secret_name)
                    .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?,
            )
        }
        "process_environment" => {
            let environment_name = parse_string(required(values, "environment_name")?)?;
            validate_process_environment_name(&environment_name)?;
            StoredVariableSource::ProcessEnvironment {
                name: environment_name,
            }
        }
        other => {
            return Err(WorkspaceError::InvalidFormat(format!(
                "unsupported variable source: {other}"
            )));
        }
    };
    Ok(StoredVariable {
        name,
        source,
        sensitivity,
        enabled,
        description,
    })
}

fn format_literal_value(output: &mut String, value: &VariableValue) -> Result<(), WorkspaceError> {
    match value {
        VariableValue::Null => output.push_str("value_kind = \"null\"\n"),
        VariableValue::Bool(value) => {
            output.push_str("value_kind = \"bool\"\n");
            output.push_str(&format!("value = {value}\n"));
        }
        VariableValue::Number(value) => {
            if !value.is_finite() {
                return Err(WorkspaceError::InvalidFormat(
                    "variable numbers must be finite".to_owned(),
                ));
            }
            output.push_str("value_kind = \"number\"\n");
            output.push_str(&format!("value = {value}\n"));
        }
        VariableValue::String(value) => {
            output.push_str("value_kind = \"string\"\n");
            output.push_str(&format!("value = {}\n", quote(value)));
        }
        VariableValue::Object(_) | VariableValue::Array(_) => {
            output.push_str("value_kind = \"json\"\n");
            let json = serde_json::to_string(&variable_value_to_json(value)?)
                .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
            output.push_str(&format!("value = {}\n", quote(&json)));
        }
    }
    Ok(())
}

fn parse_literal_value(values: &BTreeMap<String, String>) -> Result<VariableValue, WorkspaceError> {
    let kind = values
        .get("value_kind")
        .map(|value| parse_string(value))
        .transpose()?
        .unwrap_or_else(|| "string".to_owned());
    match kind.as_str() {
        "null" => Ok(VariableValue::Null),
        "bool" => Ok(VariableValue::Bool(parse_bool(
            required(values, "value")?,
            "value",
        )?)),
        "number" => required(values, "value")?.parse::<f64>().map_or_else(
            |_| {
                Err(WorkspaceError::InvalidFormat(
                    "variable number is invalid".to_owned(),
                ))
            },
            |value| {
                if value.is_finite() {
                    Ok(VariableValue::Number(value))
                } else {
                    Err(WorkspaceError::InvalidFormat(
                        "variable numbers must be finite".to_owned(),
                    ))
                }
            },
        ),
        "string" => Ok(VariableValue::String(parse_string(required(
            values, "value",
        )?)?)),
        "json" => {
            let json = parse_string(required(values, "value")?)?;
            let value: serde_json::Value = serde_json::from_str(&json)
                .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
            json_to_variable_value(value)
        }
        other => Err(WorkspaceError::InvalidFormat(format!(
            "unsupported variable value kind: {other}"
        ))),
    }
}

fn variable_value_to_json(value: &VariableValue) -> Result<serde_json::Value, WorkspaceError> {
    match value {
        VariableValue::Null => Ok(serde_json::Value::Null),
        VariableValue::Bool(value) => Ok(serde_json::Value::Bool(*value)),
        VariableValue::Number(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| {
                WorkspaceError::InvalidFormat("variable numbers must be finite".to_owned())
            }),
        VariableValue::String(value) => Ok(serde_json::Value::String(value.clone())),
        VariableValue::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), variable_value_to_json(value)?)))
            .collect::<Result<serde_json::Map<_, _>, WorkspaceError>>()
            .map(serde_json::Value::Object),
        VariableValue::Array(values) => values
            .iter()
            .map(variable_value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
    }
}

fn json_to_variable_value(value: serde_json::Value) -> Result<VariableValue, WorkspaceError> {
    match value {
        serde_json::Value::Null => Ok(VariableValue::Null),
        serde_json::Value::Bool(value) => Ok(VariableValue::Bool(value)),
        serde_json::Value::Number(value) => {
            value.as_f64().map(VariableValue::Number).ok_or_else(|| {
                WorkspaceError::InvalidFormat("JSON number cannot be represented".to_owned())
            })
        }
        serde_json::Value::String(value) => Ok(VariableValue::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_to_variable_value)
            .collect::<Result<Vec<_>, _>>()
            .map(VariableValue::Array),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, json_to_variable_value(value)?)))
            .collect::<Result<BTreeMap<_, _>, WorkspaceError>>()
            .map(VariableValue::Object),
    }
}

fn parse_bool(value: &str, key: &str) -> Result<bool, WorkspaceError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(WorkspaceError::InvalidFormat(format!(
            "{key} must be true or false"
        ))),
    }
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

fn validate_variable_name(name: &str) -> Result<(), WorkspaceError> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidFormat(format!(
            "invalid variable name: {name}"
        )))
    }
}

fn validate_process_environment_name(name: &str) -> Result<(), WorkspaceError> {
    let valid = !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidFormat(format!(
            "invalid process environment variable name: {name}"
        )))
    }
}

fn validate_variable_set_identity(document: &VariableSetDocument) -> Result<(), WorkspaceError> {
    validate_variable_set_name(&document.name)
}

fn validate_variable_set_name(name: &str) -> Result<(), WorkspaceError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        Err(WorkspaceError::InvalidFormat(
            "environment names must be non-empty and contain no control characters".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn ensure_file_snapshot(path: &Path, expected: FileFingerprint) -> Result<(), WorkspaceError> {
    let bytes = fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            WorkspaceError::ExternalChange(path.to_owned())
        } else {
            WorkspaceError::Io(error)
        }
    })?;
    if FileFingerprint::from_bytes(&bytes) == expected {
        Ok(())
    } else {
        Err(WorkspaceError::ExternalChange(path.to_owned()))
    }
}

fn delete_file_checked(path: &Path, expected: FileFingerprint) -> Result<(), WorkspaceError> {
    ensure_file_snapshot(path, expected)?;
    let parent = path
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
    let tombstone = parent.join(format!(".{name}.delete.{nonce}.tmp"));
    fs::rename(path, &tombstone)?;
    sync_directory(parent)?;
    if let Err(error) = fs::remove_file(&tombstone) {
        let restore = fs::rename(&tombstone, path);
        let _ = sync_directory(parent);
        return match restore {
            Ok(()) => Err(WorkspaceError::Io(error)),
            Err(restore_error) => Err(WorkspaceError::InvalidFormat(format!(
                "environment deletion failed ({error}) and rollback failed ({restore_error}); recover {}",
                tombstone.display()
            ))),
        };
    }
    sync_directory(parent)
}

fn sync_parent(path: &Path) -> Result<(), WorkspaceError> {
    let parent = path
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidPath(path.display().to_string()))?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    if let Ok(directory) = File::open(path) {
        directory.sync_all()?;
    }
    Ok(())
}

fn strongest_sensitivity(left: ValueSensitivity, right: ValueSensitivity) -> ValueSensitivity {
    match (left, right) {
        (ValueSensitivity::Secret, _) | (_, ValueSensitivity::Secret) => ValueSensitivity::Secret,
        (ValueSensitivity::Sensitive, _) | (_, ValueSensitivity::Sensitive) => {
            ValueSensitivity::Sensitive
        }
        _ => ValueSensitivity::Public,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_secrets::{SecretStore, SecretValue, SessionSecretStore};
    use std::sync::Arc;

    fn variable_set() -> VariableSetDocument {
        let mut nested = BTreeMap::new();
        nested.insert(
            "host".to_owned(),
            VariableValue::String("api.example.test".to_owned()),
        );
        VariableSetDocument {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: StableId::parse("development").expect("valid id"),
            name: "Development".to_owned(),
            variables: vec![
                StoredVariable {
                    name: "service".to_owned(),
                    source: StoredVariableSource::Literal(VariableValue::Object(nested)),
                    sensitivity: ValueSensitivity::Public,
                    enabled: true,
                    description: Some("Service configuration".to_owned()),
                },
                StoredVariable {
                    name: "token".to_owned(),
                    source: StoredVariableSource::Secret(
                        SecretRef::new("development", "token").expect("valid secret ref"),
                    ),
                    sensitivity: ValueSensitivity::Secret,
                    enabled: true,
                    description: None,
                },
            ],
            unknown_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn variable_set_round_trip_preserves_nested_values_and_secret_references() {
        let original = variable_set();
        let formatted = format_variable_set(&original).expect("formats");
        assert!(!formatted.contains("top-secret"));
        assert_eq!(parse_variable_set(&formatted).expect("parses"), original);
    }

    #[test]
    fn resolves_secret_references_without_persisting_values() {
        let store = Arc::new(SessionSecretStore::default());
        let reference = SecretRef::new("development", "token").expect("valid ref");
        store
            .put(&reference, SecretValue::new(b"top-secret".to_vec()))
            .expect("stores secret");
        let mut chain = SecretStoreChain::default();
        chain.push(store);
        let resolved = variable_set().resolve(Some(&chain)).expect("resolves");
        assert_eq!(
            resolved["token"].value,
            VariableValue::String("top-secret".to_owned())
        );
        assert_eq!(resolved["token"].sensitivity, ValueSensitivity::Secret);
    }

    #[test]
    fn rejects_secret_source_without_secret_sensitivity() {
        let input = r#"schema_version = 1
id = "development"
name = "Development"

[[variables]]
name = "token"
enabled = true
sensitivity = "public"
source = "secret"
secret_namespace = "development"
secret_name = "token"
"#;
        let error = parse_variable_set(input).expect_err("secret source must remain secret");
        assert!(matches!(error, WorkspaceError::InvalidFormat(_)));
    }

    fn temporary_repository(name: &str) -> (WorkspaceRepository, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "apex-environment-{name}-{}-{nonce}",
            std::process::id()
        ));
        let repository = WorkspaceRepository::new(&root).expect("repository");
        repository
            .initialize(&crate::WorkspaceManifest::new(
                StableId::parse("workspace").expect("workspace id"),
                "Environment fixture",
            ))
            .expect("initialize");
        (repository, root)
    }

    fn public_variable(name: &str, value: &str) -> StoredVariable {
        StoredVariable {
            name: name.to_owned(),
            source: StoredVariableSource::Literal(VariableValue::String(value.to_owned())),
            sensitivity: ValueSensitivity::Public,
            enabled: true,
            description: None,
        }
    }

    #[test]
    fn environment_crud_and_local_override_are_listed_without_secret_values() {
        let (repository, root) = temporary_repository("crud");
        let id = StableId::parse("development").expect("id");
        let mut environment = VariableSetDocument::new(id.clone(), "Development");
        environment
            .variables
            .push(public_variable("host", "development.test"));
        let fingerprint = repository
            .create_environment(&environment)
            .expect("create environment");
        let fingerprint = repository
            .rename_environment(&id, "Developer machine", fingerprint)
            .expect("rename environment");
        let mut local = VariableSetDocument::new(id.clone(), "Development local");
        local.variables.push(public_variable("host", "127.0.0.1"));
        repository
            .save_local_environment_override(&id, &local, None)
            .expect("local override");

        let environments = repository.list_environments().expect("list environments");
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].name, "Developer machine");
        assert!(environments[0].has_local_override);
        assert_eq!(
            repository
                .load_environment(&id)
                .expect("updated environment")
                .fingerprint,
            fingerprint
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn stale_environment_update_is_rejected() {
        let (repository, root) = temporary_repository("stale");
        let id = StableId::parse("development").expect("id");
        let document = VariableSetDocument::new(id.clone(), "Development");
        let fingerprint = repository
            .create_environment(&document)
            .expect("create environment");
        fs::write(repository.environment_path(&id), "external edit").expect("external edit");
        assert!(matches!(
            repository.rename_environment(&id, "Changed", fingerprint),
            Err(WorkspaceError::ExternalChange(_))
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn local_override_id_must_match_environment() {
        let (repository, root) = temporary_repository("override-id");
        let id = StableId::parse("development").expect("id");
        repository
            .create_environment(&VariableSetDocument::new(id.clone(), "Development"))
            .expect("create environment");
        let wrong = VariableSetDocument::new(
            StableId::parse("staging").expect("wrong id"),
            "Wrong local override",
        );
        assert!(matches!(
            repository.save_local_environment_override(&id, &wrong, None),
            Err(WorkspaceError::InvalidFormat(_))
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn deleting_default_environment_requires_explicit_default_change() {
        let (repository, root) = temporary_repository("default-delete");
        let id = StableId::parse("development").expect("id");
        let fingerprint = repository
            .create_environment(&VariableSetDocument::new(id.clone(), "Development"))
            .expect("create environment");
        let manifest = repository.load_manifest().expect("manifest");
        repository
            .set_default_environment(Some(&id), manifest.fingerprint)
            .expect("set default");
        assert!(matches!(
            repository.delete_environment(&id, fingerprint),
            Err(WorkspaceError::InvalidFormat(_))
        ));
        assert!(repository.environment_path(&id).exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn deleting_environment_also_removes_ignored_local_override() {
        let (repository, root) = temporary_repository("delete");
        let id = StableId::parse("development").expect("id");
        let fingerprint = repository
            .create_environment(&VariableSetDocument::new(id.clone(), "Development"))
            .expect("create environment");
        repository
            .save_local_environment_override(
                &id,
                &VariableSetDocument::new(id.clone(), "Development local"),
                None,
            )
            .expect("local override");
        let receipt = repository
            .delete_environment(&id, fingerprint)
            .expect("delete environment");
        assert_eq!(receipt.id, id);
        assert_eq!(receipt.cleanup_pending, None);
        assert!(!repository.environment_path(&receipt.id).exists());
        assert!(
            !repository
                .local_environment_override_path(&receipt.id)
                .exists()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn local_override_delete_is_fingerprint_guarded() {
        let (repository, root) = temporary_repository("override-delete");
        let id = StableId::parse("development").expect("id");
        repository
            .create_environment(&VariableSetDocument::new(id.clone(), "Development"))
            .expect("create environment");
        let fingerprint = repository
            .save_local_environment_override(
                &id,
                &VariableSetDocument::new(id.clone(), "Development local"),
                None,
            )
            .expect("local override");
        fs::write(
            repository.local_environment_override_path(&id),
            "external edit",
        )
        .expect("external edit");
        assert!(matches!(
            repository.delete_local_environment_override(&id, fingerprint),
            Err(WorkspaceError::ExternalChange(_))
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn empty_environment_names_are_rejected() {
        let mut document = variable_set();
        document.name = "  ".to_owned();
        assert!(matches!(
            format_variable_set(&document),
            Err(WorkspaceError::InvalidFormat(_))
        ));
    }

    #[test]
    fn rejects_plaintext_secret_literals() {
        let input = r#"schema_version = 1
id = "development"
name = "Development"

[[variables]]
name = "token"
enabled = true
sensitivity = "secret"
source = "literal"
value_kind = "string"
value = "do-not-store"
"#;
        let error = parse_variable_set(input).expect_err("must reject plaintext secret");
        assert!(matches!(error, WorkspaceError::SecretResolution(_)));
    }
}
