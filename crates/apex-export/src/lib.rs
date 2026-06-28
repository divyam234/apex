#![forbid(unsafe_code)]

use apex_domain::{
    ApiKeyPlacement, Authentication, HttpRequest, MultipartValue, RequestBody, ValueSensitivity,
};
use base64::Engine as _;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter, Write as _};
use url::Url;

pub const REDACTED: &str = "{{REDACTED}}";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeTarget {
    Curl,
    Httpie,
    RustReqwest,
    PythonRequests,
    GoNetHttp,
}

impl CodeTarget {
    pub fn id(self) -> &'static str {
        match self {
            Self::Curl => "curl",
            Self::Httpie => "httpie",
            Self::RustReqwest => "rust-reqwest",
            Self::PythonRequests => "python-requests",
            Self::GoNetHttp => "go-net-http",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CodegenOptions {
    pub reveal_sensitive_values: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedSnippet {
    pub target: CodeTarget,
    pub code: String,
    pub warnings: Vec<String>,
}

pub fn generate(
    request: &HttpRequest,
    target: CodeTarget,
    options: CodegenOptions,
) -> Result<GeneratedSnippet, ExportError> {
    let prepared = PreparedRequest::new(request, options)?;
    let (code, mut warnings) = match target {
        CodeTarget::Curl => generate_curl(&prepared),
        CodeTarget::Httpie => generate_httpie(&prepared),
        CodeTarget::RustReqwest => generate_rust_reqwest(&prepared),
        CodeTarget::PythonRequests => generate_python_requests(&prepared),
        CodeTarget::GoNetHttp => generate_go_net_http(&prepared),
    }?;
    warnings.extend(prepared.warnings);
    Ok(GeneratedSnippet {
        target,
        code,
        warnings,
    })
}

#[derive(Clone, Debug)]
struct PreparedHeader {
    name: String,
    value: String,
}

#[derive(Clone, Debug)]
struct PreparedRequest {
    method: String,
    url: String,
    headers: Vec<PreparedHeader>,
    body: RequestBody,
    warnings: Vec<String>,
    timeout_seconds: u64,
    follow_redirects: bool,
    redirect_limit: u16,
    verify_certificates: bool,
}

impl PreparedRequest {
    fn new(request: &HttpRequest, options: CodegenOptions) -> Result<Self, ExportError> {
        let mut warnings = Vec::new();
        let mut url = request.url.clone();
        let mut headers = request
            .headers
            .iter()
            .filter(|header| header.enabled)
            .map(|header| PreparedHeader {
                name: header.name.clone(),
                value: visible_value(
                    &header.value,
                    header.sensitivity,
                    options.reveal_sensitive_values,
                ),
            })
            .collect::<Vec<_>>();
        apply_authentication(
            &request.authentication,
            &mut url,
            &mut headers,
            options,
            &mut warnings,
        )?;
        Ok(Self {
            method: request.method.as_str().to_owned(),
            url,
            headers,
            body: redact_body(&request.body, options),
            warnings,
            timeout_seconds: request.settings.timeout.as_secs().max(1),
            follow_redirects: request.settings.follow_redirects,
            redirect_limit: request.settings.redirect_limit,
            verify_certificates: request.settings.verify_certificates,
        })
    }
}

fn visible_value(value: &str, sensitivity: ValueSensitivity, reveal: bool) -> String {
    if reveal || sensitivity == ValueSensitivity::Public {
        value.to_owned()
    } else {
        REDACTED.to_owned()
    }
}

fn apply_authentication(
    authentication: &Authentication,
    url: &mut String,
    headers: &mut Vec<PreparedHeader>,
    options: CodegenOptions,
    warnings: &mut Vec<String>,
) -> Result<(), ExportError> {
    match authentication {
        Authentication::None => {}
        Authentication::Basic { username, password } => {
            let value = if options.reveal_sensitive_values {
                let raw = format!("{username}:{password}");
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(raw)
                )
            } else {
                format!("Basic {REDACTED}")
            };
            headers.push(PreparedHeader {
                name: "Authorization".to_owned(),
                value,
            });
        }
        Authentication::Bearer { token } => {
            headers.push(PreparedHeader {
                name: "Authorization".to_owned(),
                value: format!(
                    "Bearer {}",
                    if options.reveal_sensitive_values {
                        token.as_str()
                    } else {
                        REDACTED
                    }
                ),
            });
        }
        Authentication::ApiKey {
            name,
            value,
            placement,
        } => {
            let value = if options.reveal_sensitive_values {
                value.clone()
            } else {
                REDACTED.to_owned()
            };
            match placement {
                ApiKeyPlacement::Header => headers.push(PreparedHeader {
                    name: name.clone(),
                    value,
                }),
                ApiKeyPlacement::Query => append_query(url, name, &value, warnings)?,
            }
        }
    }
    Ok(())
}

fn append_query(
    url: &mut String,
    name: &str,
    value: &str,
    warnings: &mut Vec<String>,
) -> Result<(), ExportError> {
    match Url::parse(url) {
        Ok(mut parsed) => {
            parsed.query_pairs_mut().append_pair(name, value);
            *url = parsed.into();
        }
        Err(_) if url.contains("{{") => {
            let separator = if url.contains('?') { '&' } else { '?' };
            let encoded_name =
                url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>();
            let encoded_value =
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
            write!(url, "{separator}{encoded_name}={encoded_value}")
                .map_err(|_| ExportError::Generation("failed to append API key".to_owned()))?;
            warnings.push(
                "The URL contains unresolved variables; API-key query encoding was appended without full URL validation."
                    .to_owned(),
            );
        }
        Err(error) => return Err(ExportError::InvalidUrl(error.to_string())),
    }
    Ok(())
}

fn redact_body(body: &RequestBody, options: CodegenOptions) -> RequestBody {
    match body {
        RequestBody::FormUrlEncoded(fields) => RequestBody::FormUrlEncoded(
            fields
                .iter()
                .map(|field| {
                    let mut field = field.clone();
                    field.value = visible_value(
                        &field.value,
                        field.sensitivity,
                        options.reveal_sensitive_values,
                    );
                    field
                })
                .collect(),
        ),
        RequestBody::Multipart(fields) => RequestBody::Multipart(
            fields
                .iter()
                .map(|field| {
                    let mut field = field.clone();
                    if let MultipartValue::Text(value) = &mut field.value {
                        *value = visible_value(
                            value,
                            field.sensitivity,
                            options.reveal_sensitive_values,
                        );
                    }
                    field
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn generate_curl(prepared: &PreparedRequest) -> Result<(String, Vec<String>), ExportError> {
    let mut lines = vec![format!(
        "curl --request {} {}",
        prepared.method,
        shell_quote(&prepared.url)
    )];
    if prepared.follow_redirects {
        lines.push("  --location".to_owned());
    } else {
        lines.push("  --max-redirs 0".to_owned());
    }
    if !prepared.verify_certificates {
        lines.push("  --insecure".to_owned());
    }
    lines.push(format!("  --max-time {}", prepared.timeout_seconds));
    for header in &prepared.headers {
        lines.push(format!(
            "  --header {}",
            shell_quote(&format!("{}: {}", header.name, header.value))
        ));
    }
    append_curl_body(&mut lines, &prepared.body)?;
    Ok((join_shell_lines(lines), Vec::new()))
}

fn append_curl_body(lines: &mut Vec<String>, body: &RequestBody) -> Result<(), ExportError> {
    match body {
        RequestBody::Empty => {}
        RequestBody::Text { text, .. } | RequestBody::Json(text) | RequestBody::Xml(text) => {
            lines.push(format!("  --data-raw {}", shell_quote(text)));
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => {
            let payload = graphql_payload(query, variables_json, operation_name.as_deref())?;
            lines.push(format!("  --data-raw {}", shell_quote(&payload)));
        }
        RequestBody::FormUrlEncoded(fields) => {
            for field in fields.iter().filter(|field| field.enabled) {
                lines.push(format!(
                    "  --data-urlencode {}",
                    shell_quote(&format!("{}={}", field.name, field.value))
                ));
            }
        }
        RequestBody::Multipart(fields) => {
            for field in fields.iter().filter(|field| field.enabled) {
                let value = match &field.value {
                    MultipartValue::Text(value) => format!("{}={value}", field.name),
                    MultipartValue::File { relative_path } => {
                        let mut value = format!("{}=@{relative_path}", field.name);
                        if let Some(content_type) = &field.content_type {
                            write!(value, ";type={content_type}").map_err(|_| {
                                ExportError::Generation("multipart formatting failed".to_owned())
                            })?;
                        }
                        value
                    }
                };
                lines.push(format!("  --form {}", shell_quote(&value)));
            }
        }
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            lines.push(format!(
                "  --data-binary {}",
                shell_quote(&format!("@{relative_path}"))
            ));
        }
    }
    Ok(())
}

fn generate_httpie(prepared: &PreparedRequest) -> Result<(String, Vec<String>), ExportError> {
    let mut arguments = vec![
        "http".to_owned(),
        prepared.method.clone(),
        shell_quote(&prepared.url),
        format!("--timeout={}", prepared.timeout_seconds),
    ];
    if !prepared.follow_redirects {
        arguments.push("--max-redirects=0".to_owned());
    }
    if !prepared.verify_certificates {
        arguments.push("--verify=no".to_owned());
    }
    for header in &prepared.headers {
        arguments.push(shell_quote(&format!("{}:{}", header.name, header.value)));
    }
    let mut warnings = Vec::new();
    match &prepared.body {
        RequestBody::Empty => {}
        RequestBody::Text { text, .. } | RequestBody::Json(text) | RequestBody::Xml(text) => {
            arguments.push(format!("--raw={}", shell_quote(text)));
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => arguments.push(format!(
            "--raw={}",
            shell_quote(&graphql_payload(
                query,
                variables_json,
                operation_name.as_deref()
            )?)
        )),
        RequestBody::FormUrlEncoded(fields) => {
            arguments.push("--form".to_owned());
            for field in fields.iter().filter(|field| field.enabled) {
                arguments.push(shell_quote(&format!("{}={}", field.name, field.value)));
            }
        }
        RequestBody::Multipart(fields) => {
            arguments.push("--form".to_owned());
            for field in fields.iter().filter(|field| field.enabled) {
                match &field.value {
                    MultipartValue::Text(value) => {
                        arguments.push(shell_quote(&format!("{}={value}", field.name)));
                    }
                    MultipartValue::File { relative_path } => {
                        arguments.push(shell_quote(&format!("{}@{relative_path}", field.name)));
                    }
                }
            }
        }
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            arguments.push(format!("< {}", shell_quote(relative_path)));
            warnings.push(
                "HTTPie file redirection is shell syntax; review it before using in non-shell contexts."
                    .to_owned(),
            );
        }
    }
    Ok((arguments.join(" "), warnings))
}

fn generate_rust_reqwest(prepared: &PreparedRequest) -> Result<(String, Vec<String>), ExportError> {
    let mut code = String::new();
    code.push_str("use reqwest::{Client, Method, header::{HeaderMap, HeaderName, HeaderValue}};\n");
    code.push_str("use std::{str::FromStr, time::Duration};\n\n");
    code.push_str("#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {\n");
    writeln!(
        code,
        "    let client = Client::builder().timeout(Duration::from_secs({})).redirect(reqwest::redirect::Policy::limited({})){} .build()?;",
        prepared.timeout_seconds,
        if prepared.follow_redirects {
            prepared.redirect_limit
        } else {
            0
        },
        if prepared.verify_certificates {
            String::new()
        } else {
            ".danger_accept_invalid_certs(true)".to_owned()
        }
    )
    .map_err(|_| ExportError::Generation("Rust snippet formatting failed".to_owned()))?;
    code.push_str("    let mut headers = HeaderMap::new();\n");
    for header in &prepared.headers {
        writeln!(
            code,
            "    headers.append(HeaderName::from_str({})?, HeaderValue::from_str({})?);",
            rust_string(&header.name),
            rust_string(&header.value)
        )
        .map_err(|_| ExportError::Generation("Rust header formatting failed".to_owned()))?;
    }
    writeln!(
        code,
        "    let request = client.request(Method::from_bytes({}.as_bytes())?, {}).headers(headers)",
        rust_string(&prepared.method),
        rust_string(&prepared.url)
    )
    .map_err(|_| ExportError::Generation("Rust request formatting failed".to_owned()))?;
    append_rust_body(&mut code, &prepared.body)?;
    code.push_str("        .send().await?;\n");
    code.push_str("    println!(\"{}\", request.status());\n    Ok(())\n}\n");
    let warnings = if matches!(prepared.body, RequestBody::Multipart(_)) {
        vec![
            "The Rust reqwest snippet marks the multipart assembly point; add a multipart::Form before execution."
                .to_owned(),
        ]
    } else {
        Vec::new()
    };
    Ok((code, warnings))
}

fn append_rust_body(code: &mut String, body: &RequestBody) -> Result<(), ExportError> {
    match body {
        RequestBody::Empty => code.push_str("        "),
        RequestBody::Text { text, .. } | RequestBody::Json(text) | RequestBody::Xml(text) => {
            writeln!(code, "        .body({})", rust_string(text))
                .map_err(|_| ExportError::Generation("Rust body formatting failed".to_owned()))?;
            code.push_str("        ");
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => {
            let payload = graphql_payload(query, variables_json, operation_name.as_deref())?;
            writeln!(code, "        .body({})", rust_string(&payload))
                .map_err(|_| ExportError::Generation("Rust body formatting failed".to_owned()))?;
            code.push_str("        ");
        }
        RequestBody::FormUrlEncoded(fields) => {
            code.push_str("        .form(&[\n");
            for field in fields.iter().filter(|field| field.enabled) {
                writeln!(
                    code,
                    "            ({}, {}),",
                    rust_string(&field.name),
                    rust_string(&field.value)
                )
                .map_err(|_| ExportError::Generation("Rust form formatting failed".to_owned()))?;
            }
            code.push_str("        ])\n        ");
        }
        RequestBody::Multipart(_) => {
            code.push_str("        // Build reqwest::multipart::Form with the fields below before send.\n        ");
        }
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            writeln!(
                code,
                "        .body(tokio::fs::read({}).await?)",
                rust_string(relative_path)
            )
            .map_err(|_| ExportError::Generation("Rust file body formatting failed".to_owned()))?;
            code.push_str("        ");
        }
    }
    Ok(())
}

fn generate_python_requests(
    prepared: &PreparedRequest,
) -> Result<(String, Vec<String>), ExportError> {
    let mut warnings = Vec::new();
    let mut duplicate_counts = BTreeMap::<String, usize>::new();
    for header in &prepared.headers {
        *duplicate_counts
            .entry(header.name.to_ascii_lowercase())
            .or_default() += 1;
    }
    if duplicate_counts.values().any(|count| *count > 1) {
        warnings.push(
            "Python requests uses a mapping for headers and cannot preserve duplicate header names; the last value is generated."
                .to_owned(),
        );
    }
    let mut code = String::from("import requests\n\nheaders = {\n");
    for header in &prepared.headers {
        writeln!(
            code,
            "    {}: {},",
            python_string(&header.name),
            python_string(&header.value)
        )
        .map_err(|_| ExportError::Generation("Python header formatting failed".to_owned()))?;
    }
    code.push_str("}\n");
    let body = python_body(&prepared.body, &mut warnings)?;
    writeln!(
        code,
        "response = requests.request({}, {}, headers=headers, timeout={}, allow_redirects={}{}{}, verify={})",
        python_string(&prepared.method),
        python_string(&prepared.url),
        prepared.timeout_seconds,
        if prepared.follow_redirects { "True" } else { "False" },
        if body.is_empty() { "" } else { ", " },
        body,
        if prepared.verify_certificates { "True" } else { "False" }
    )
    .map_err(|_| ExportError::Generation("Python request formatting failed".to_owned()))?;
    code.push_str("print(response.status_code)\n");
    Ok((code, warnings))
}

fn python_body(body: &RequestBody, warnings: &mut Vec<String>) -> Result<String, ExportError> {
    match body {
        RequestBody::Empty => Ok(String::new()),
        RequestBody::Text { text, .. } | RequestBody::Json(text) | RequestBody::Xml(text) => {
            Ok(format!("data={}", python_string(text)))
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => Ok(format!(
            "data={}",
            python_string(&graphql_payload(
                query,
                variables_json,
                operation_name.as_deref()
            )?)
        )),
        RequestBody::FormUrlEncoded(fields) => {
            let entries = fields
                .iter()
                .filter(|field| field.enabled)
                .map(|field| {
                    format!(
                        "({}, {})",
                        python_string(&field.name),
                        python_string(&field.value)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Ok(format!("data=[{entries}]"))
        }
        RequestBody::Multipart(_) => {
            warnings.push(
                "Multipart generation requires explicit file-handle lifetime management; add a files mapping before execution."
                    .to_owned(),
            );
            Ok("files={}".to_owned())
        }
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            warnings.push(
                "The generated file handle should be managed with a context manager in production code."
                    .to_owned(),
            );
            Ok(format!("data=open({}, 'rb')", python_string(relative_path)))
        }
    }
}

fn generate_go_net_http(prepared: &PreparedRequest) -> Result<(String, Vec<String>), ExportError> {
    let body = go_body(&prepared.body)?;
    let mut code = String::from("package main\n\nimport (\n    \"fmt\"\n    \"net/http\"\n");
    if go_body_uses_strings(&prepared.body) {
        code.push_str("    \"strings\"\n");
    }
    code.push_str("    \"time\"\n)\n\nfunc main() {\n");
    writeln!(
        code,
        "    req, err := http.NewRequest({}, {}, {})",
        go_string(&prepared.method),
        go_string(&prepared.url),
        body
    )
    .map_err(|_| ExportError::Generation("Go request formatting failed".to_owned()))?;
    code.push_str("    if err != nil { panic(err) }\n");
    for header in &prepared.headers {
        writeln!(
            code,
            "    req.Header.Add({}, {})",
            go_string(&header.name),
            go_string(&header.value)
        )
        .map_err(|_| ExportError::Generation("Go header formatting failed".to_owned()))?;
    }
    writeln!(
        code,
        "    client := &http.Client{{Timeout: {} * time.Second}}",
        prepared.timeout_seconds
    )
    .map_err(|_| ExportError::Generation("Go client formatting failed".to_owned()))?;
    if !prepared.follow_redirects {
        code.push_str(
            "    client.CheckRedirect = func(_ *http.Request, _ []*http.Request) error { return http.ErrUseLastResponse }\n",
        );
    }
    if !prepared.verify_certificates {
        code.push_str(
            "    // TLS verification is disabled in ApexAPI; configure a custom Transport explicitly before running.\n",
        );
    }
    code.push_str("    resp, err := client.Do(req)\n    if err != nil { panic(err) }\n    defer resp.Body.Close()\n    fmt.Println(resp.StatusCode)\n}\n");
    let mut warnings = Vec::new();
    if !prepared.verify_certificates {
        warnings.push(
            "Go snippet does not silently install an insecure TLS transport; review the emitted comment."
                .to_owned(),
        );
    }
    if matches!(prepared.body, RequestBody::Multipart(_)) {
        warnings.push(
            "The Go snippet marks the multipart.Writer assembly point; add the multipart body before execution."
                .to_owned(),
        );
    } else if matches!(
        prepared.body,
        RequestBody::BinaryFile { .. } | RequestBody::StreamFile { .. }
    ) {
        warnings.push(
            "The Go snippet marks the file io.Reader assembly point; open and close the file explicitly before execution."
                .to_owned(),
        );
    }
    Ok((code, warnings))
}

fn go_body_uses_strings(body: &RequestBody) -> bool {
    matches!(
        body,
        RequestBody::Text { .. }
            | RequestBody::Json(_)
            | RequestBody::Xml(_)
            | RequestBody::GraphQl { .. }
            | RequestBody::FormUrlEncoded(_)
    )
}

fn go_body(body: &RequestBody) -> Result<String, ExportError> {
    match body {
        RequestBody::Empty => Ok("nil".to_owned()),
        RequestBody::Text { text, .. } | RequestBody::Json(text) | RequestBody::Xml(text) => {
            Ok(format!("strings.NewReader({})", go_string(text)))
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => Ok(format!(
            "strings.NewReader({})",
            go_string(&graphql_payload(
                query,
                variables_json,
                operation_name.as_deref()
            )?)
        )),
        RequestBody::FormUrlEncoded(fields) => {
            let encoded = fields
                .iter()
                .filter(|field| field.enabled)
                .map(|field| {
                    let name = url::form_urlencoded::byte_serialize(field.name.as_bytes())
                        .collect::<String>();
                    let value = url::form_urlencoded::byte_serialize(field.value.as_bytes())
                        .collect::<String>();
                    format!("{name}={value}")
                })
                .collect::<Vec<_>>()
                .join("&");
            Ok(format!("strings.NewReader({})", go_string(&encoded)))
        }
        RequestBody::Multipart(_) => Ok("nil /* build multipart.Writer body */".to_owned()),
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            Ok(format!(
                "nil /* open {} and pass the file as io.Reader */",
                go_string(relative_path)
            ))
        }
    }
}

fn graphql_payload(
    query: &str,
    variables_json: &str,
    operation_name: Option<&str>,
) -> Result<String, ExportError> {
    let variables = serde_json::from_str::<serde_json::Value>(variables_json).map_err(|error| {
        ExportError::InvalidBody(format!("invalid GraphQL variables JSON: {error}"))
    })?;
    let mut object = serde_json::Map::new();
    object.insert(
        "query".to_owned(),
        serde_json::Value::String(query.to_owned()),
    );
    object.insert("variables".to_owned(), variables);
    if let Some(operation_name) = operation_name {
        object.insert(
            "operationName".to_owned(),
            serde_json::Value::String(operation_name.to_owned()),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(object))
        .map_err(|error| ExportError::Generation(error.to_string()))
}

fn join_shell_lines(lines: Vec<String>) -> String {
    lines.join(" \\\n")
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn rust_string(value: &str) -> String {
    format!("{value:?}")
}

fn python_string(value: &str) -> String {
    format!("{value:?}")
}

fn go_string(value: &str) -> String {
    format!("{value:?}")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportError {
    InvalidUrl(String),
    InvalidBody(String),
    Generation(String),
}

impl Display for ExportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl(detail) => write!(formatter, "cannot generate URL: {detail}"),
            Self::InvalidBody(detail) => write!(formatter, "cannot generate body: {detail}"),
            Self::Generation(detail) => write!(formatter, "code generation failed: {detail}"),
        }
    }
}

impl std::error::Error for ExportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{
        Authentication, FormField, HeaderEntry, HttpMethod, RequestSettings, StableId,
    };

    fn request() -> HttpRequest {
        let mut public = HeaderEntry::new("X-Trace", "one").unwrap();
        public.enabled = true;
        let mut duplicate = HeaderEntry::new("X-Trace", "two").unwrap();
        duplicate.enabled = true;
        let mut sensitive = HeaderEntry::new("X-Internal", "private-host").unwrap();
        sensitive.sensitivity = ValueSensitivity::Sensitive;
        HttpRequest {
            id: StableId::parse("codegen-request").unwrap(),
            name: "Codegen".to_owned(),
            method: HttpMethod::Post,
            url: "https://api.example.test/users".to_owned(),
            query: Vec::new(),
            headers: vec![public, duplicate, sensitive],
            authentication: Authentication::Bearer {
                token: "actual-token".to_owned(),
            },
            body: RequestBody::FormUrlEncoded(vec![FormField {
                name: "password".to_owned(),
                value: "actual-password".to_owned(),
                enabled: true,
                sensitivity: ValueSensitivity::Secret,
            }]),
            settings: RequestSettings::default(),
            documentation: String::new(),
        }
    }

    #[test]
    fn every_target_redacts_auth_and_sensitive_fields_by_default() {
        for target in [
            CodeTarget::Curl,
            CodeTarget::Httpie,
            CodeTarget::RustReqwest,
            CodeTarget::PythonRequests,
            CodeTarget::GoNetHttp,
        ] {
            let generated = generate(&request(), target, CodegenOptions::default()).unwrap();
            assert!(generated.code.contains(REDACTED), "target: {target:?}");
            assert!(
                !generated.code.contains("actual-token"),
                "target: {target:?}"
            );
            assert!(
                !generated.code.contains("private-host"),
                "target: {target:?}"
            );
            assert!(
                !generated.code.contains("actual-password"),
                "target: {target:?}"
            );
        }
    }

    #[test]
    fn curl_and_go_preserve_duplicate_headers() {
        let curl = generate(&request(), CodeTarget::Curl, CodegenOptions::default()).unwrap();
        assert_eq!(curl.code.matches("X-Trace:").count(), 2);
        let go = generate(&request(), CodeTarget::GoNetHttp, CodegenOptions::default()).unwrap();
        assert_eq!(go.code.matches("req.Header.Add(\"X-Trace\"").count(), 2);
    }

    #[test]
    fn python_reports_duplicate_header_loss() {
        let generated = generate(
            &request(),
            CodeTarget::PythonRequests,
            CodegenOptions::default(),
        )
        .unwrap();
        assert!(
            generated
                .warnings
                .iter()
                .any(|warning| warning.contains("cannot preserve duplicate"))
        );
    }

    #[test]
    fn explicit_reveal_option_is_required_for_secret_values() {
        let generated = generate(
            &request(),
            CodeTarget::Curl,
            CodegenOptions {
                reveal_sensitive_values: true,
            },
        )
        .unwrap();
        assert!(generated.code.contains("actual-token"));
        assert!(generated.code.contains("actual-password"));
    }

    #[test]
    fn api_key_query_is_encoded_and_redacted() {
        let mut request = request();
        request.authentication = Authentication::ApiKey {
            name: "api key".to_owned(),
            value: "secret key".to_owned(),
            placement: ApiKeyPlacement::Query,
        };
        let generated = generate(&request, CodeTarget::Curl, CodegenOptions::default()).unwrap();
        assert!(generated.code.contains("api+key=%7B%7BREDACTED%7D%7D"));
        assert!(!generated.code.contains("secret key"));
    }

    #[test]
    fn go_empty_body_does_not_emit_an_unused_strings_import() {
        let mut request = request();
        request.body = RequestBody::Empty;
        let generated =
            generate(&request, CodeTarget::GoNetHttp, CodegenOptions::default()).unwrap();
        assert!(!generated.code.contains("\"strings\""));
    }

    #[test]
    fn incomplete_target_specific_body_assembly_is_reported() {
        let mut request = request();
        request.body = RequestBody::Multipart(Vec::new());
        for target in [CodeTarget::RustReqwest, CodeTarget::GoNetHttp] {
            let generated = generate(&request, target, CodegenOptions::default()).unwrap();
            assert!(!generated.warnings.is_empty(), "target: {target:?}");
        }
    }

    #[test]
    fn invalid_graphql_variables_fail_instead_of_generating_broken_code() {
        let mut request = request();
        request.body = RequestBody::GraphQl {
            query: "query Viewer { viewer { id } }".to_owned(),
            variables_json: "not json".to_owned(),
            operation_name: Some("Viewer".to_owned()),
        };
        assert!(matches!(
            generate(&request, CodeTarget::Curl, CodegenOptions::default()),
            Err(ExportError::InvalidBody(_))
        ));
    }
}
