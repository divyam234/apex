#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod branding {
    pub const PRODUCT_NAME: &str = "ApexAPI";
    pub const EXECUTABLE_NAME: &str = "apex";
    pub const WORKSPACE_FILE: &str = "apex.toml";
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableId(String);

impl StableId {
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if valid {
            Ok(Self(value))
        } else {
            Err(DomainError::InvalidStableId(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for StableId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Trace,
    Connect,
    Custom(String),
}

impl HttpMethod {
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        let normalized = value.trim().to_ascii_uppercase();
        let method = match normalized.as_str() {
            "GET" => Self::Get,
            "HEAD" => Self::Head,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "PATCH" => Self::Patch,
            "DELETE" => Self::Delete,
            "OPTIONS" => Self::Options,
            "TRACE" => Self::Trace,
            "CONNECT" => Self::Connect,
            _ if is_http_token(&normalized) => Self::Custom(normalized),
            _ => return Err(DomainError::InvalidHttpMethod(value.to_owned())),
        };
        Ok(method)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Trace => "TRACE",
            Self::Connect => "CONNECT",
            Self::Custom(value) => value,
        }
    }
}

impl Display for HttpMethod {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueSensitivity {
    Public,
    Sensitive,
    Secret,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
    pub enabled: bool,
    pub sensitivity: ValueSensitivity,
}

impl HeaderEntry {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if !is_http_token(&name) {
            return Err(DomainError::InvalidHeaderName(name));
        }
        Ok(Self {
            name,
            value: value.into(),
            enabled: true,
            sensitivity: ValueSensitivity::Public,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiKeyPlacement {
    Header,
    Query,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Authentication {
    None,
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    ApiKey {
        name: String,
        value: String,
        placement: ApiKeyPlacement,
    },
}

impl Authentication {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Basic { .. } => "basic",
            Self::Bearer { .. } => "bearer",
            Self::ApiKey { .. } => "api_key",
        }
    }

    pub fn is_configured(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormField {
    pub name: String,
    pub value: String,
    pub enabled: bool,
    pub sensitivity: ValueSensitivity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MultipartValue {
    Text(String),
    File { relative_path: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultipartField {
    pub name: String,
    pub value: MultipartValue,
    pub content_type: Option<String>,
    pub enabled: bool,
    pub sensitivity: ValueSensitivity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestBody {
    Empty,
    Text {
        content_type: Option<String>,
        text: String,
    },
    Json(String),
    Xml(String),
    GraphQl {
        query: String,
        variables_json: String,
        operation_name: Option<String>,
    },
    FormUrlEncoded(Vec<FormField>),
    Multipart(Vec<MultipartField>),
    BinaryFile {
        relative_path: String,
    },
    StreamFile {
        relative_path: String,
    },
}

impl RequestBody {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Text { .. } => "text",
            Self::Json(_) => "json",
            Self::Xml(_) => "xml",
            Self::GraphQl { .. } => "graphql",
            Self::FormUrlEncoded(_) => "form_urlencoded",
            Self::Multipart(_) => "multipart",
            Self::BinaryFile { .. } => "binary_file",
            Self::StreamFile { .. } => "stream_file",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestSettings {
    pub timeout: Duration,
    pub connection_timeout: Duration,
    pub idle_timeout: Duration,
    pub maximum_response_bytes: u64,
    pub maximum_wire_response_bytes: u64,
    pub redirect_limit: u16,
    pub follow_redirects: bool,
    pub verify_certificates: bool,
    pub cookie_jar: bool,
    pub decompress_response: bool,
}

impl Default for RequestSettings {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            maximum_response_bytes: 64 * 1024 * 1024,
            maximum_wire_response_bytes: 64 * 1024 * 1024,
            redirect_limit: 10,
            follow_redirects: true,
            verify_certificates: true,
            cookie_jar: true,
            decompress_response: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpRequest {
    pub id: StableId,
    pub name: String,
    pub method: HttpMethod,
    pub url: String,
    pub query: Vec<FormField>,
    pub headers: Vec<HeaderEntry>,
    pub authentication: Authentication,
    pub body: RequestBody,
    pub settings: RequestSettings,
    pub documentation: String,
}

impl HttpRequest {
    pub fn enabled_headers(&self) -> impl Iterator<Item = &HeaderEntry> {
        self.headers.iter().filter(|entry| entry.enabled)
    }

    pub fn header_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.headers
            .iter()
            .filter(move |entry| entry.enabled && entry.name.eq_ignore_ascii_case(name))
            .map(|entry| entry.value.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum VariableValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Object(BTreeMap<String, VariableValue>),
    Array(Vec<VariableValue>),
}

impl VariableValue {
    pub fn display_value(&self) -> String {
        match self {
            Self::Null => "null".to_owned(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Object(_) => "[object]".to_owned(),
            Self::Array(_) => "[array]".to_owned(),
        }
    }

    pub fn get_path<'a>(&'a self, segments: &[&str]) -> Option<&'a Self> {
        let mut current = self;
        for segment in segments {
            match current {
                Self::Object(values) => current = values.get(*segment)?,
                Self::Array(values) => current = values.get(segment.parse::<usize>().ok()?)?,
                _ => return None,
            }
        }
        Some(current)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableDefinition {
    pub value: VariableValue,
    pub sensitivity: ValueSensitivity,
    pub enabled: bool,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProtocolId {
    Http,
    GraphQl,
    WebSocket,
    ServerSentEvents,
    Grpc,
    Http3,
    SocketIo,
    Mqtt,
    RawTcp,
    UnixHttp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolCapabilities {
    pub streaming_input: bool,
    pub streaming_output: bool,
    pub bidirectional: bool,
    pub cancellation: bool,
    pub trailers: bool,
}

static NEXT_EXECUTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExecutionId(u128);

impl ExecutionId {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = u128::from(NEXT_EXECUTION_ID.fetch_add(1, Ordering::Relaxed));
        Self((nanos << 32) ^ sequence)
    }
}

impl Default for ExecutionId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for ExecutionId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:032x}", self.0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), ExecutionError> {
        if self.is_cancelled() {
            Err(ExecutionError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimingPhase {
    VariableResolution,
    PreRequestScript,
    Queueing,
    Dns,
    TcpConnect,
    Tls,
    Upload,
    ServerWait,
    Download,
    Decompression,
    Parsing,
    TestExecution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimingValue {
    Available(Duration),
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimingEntry {
    pub phase: TimingPhase,
    pub value: TimingValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionEvent {
    Started {
        execution_id: ExecutionId,
    },
    PhaseStarted(TimingPhase),
    UploadProgress {
        sent_bytes: u64,
        total_bytes: Option<u64>,
    },
    ResponseHeaders {
        status: u16,
        http_version: String,
    },
    DownloadProgress {
        received_bytes: u64,
        total_bytes: Option<u64>,
    },
    StreamItem {
        sequence: u64,
        kind: String,
        preview: String,
    },
    Completed,
    Cancelled,
    Failed {
        category: ErrorCategory,
        redacted_summary: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    InvalidUrl,
    DnsFailure,
    ConnectionRefused,
    ConnectionTimeout,
    TlsFailure,
    CertificateFailure,
    ProxyFailure,
    AuthenticationFailure,
    RedirectLoop,
    RequestTimeout,
    UploadFailure,
    ResponseTooLarge,
    DecompressionFailure,
    MalformedResponse,
    Http2Protocol,
    WebSocket,
    ServerSentEvents,
    Grpc,
    ScriptTimeout,
    ScriptMemoryLimit,
    AssertionFailure,
    UnresolvedVariable,
    MissingSecret,
    InvalidWorkspace,
    MigrationFailure,
    ImportIncompatibility,
    FilesystemConflict,
    DatabaseCorruption,
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    InvalidUrl(String),
    DnsFailure(String),
    ConnectionRefused(String),
    ConnectionTimeout,
    TlsFailure(String),
    CertificateFailure(String),
    ProxyFailure(String),
    AuthenticationFailure(String),
    RedirectLoop,
    RequestTimeout,
    UploadFailure(String),
    ResponseTooLarge { limit: u64, observed: u64 },
    DecompressionFailure(String),
    MalformedResponse(String),
    Http2Protocol(String),
    WebSocket(String),
    ServerSentEvents(String),
    Grpc { code: i32, message: String },
    ScriptTimeout,
    ScriptMemoryLimit,
    AssertionFailure(String),
    UnresolvedVariable(String),
    MissingSecret(String),
    InvalidWorkspace(String),
    MigrationFailure(String),
    ImportIncompatibility(String),
    FilesystemConflict(String),
    DatabaseCorruption(String),
    Cancelled,
    Internal(String),
}

impl ExecutionError {
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::InvalidUrl(_) => ErrorCategory::InvalidUrl,
            Self::DnsFailure(_) => ErrorCategory::DnsFailure,
            Self::ConnectionRefused(_) => ErrorCategory::ConnectionRefused,
            Self::ConnectionTimeout => ErrorCategory::ConnectionTimeout,
            Self::TlsFailure(_) => ErrorCategory::TlsFailure,
            Self::CertificateFailure(_) => ErrorCategory::CertificateFailure,
            Self::ProxyFailure(_) => ErrorCategory::ProxyFailure,
            Self::AuthenticationFailure(_) => ErrorCategory::AuthenticationFailure,
            Self::RedirectLoop => ErrorCategory::RedirectLoop,
            Self::RequestTimeout => ErrorCategory::RequestTimeout,
            Self::UploadFailure(_) => ErrorCategory::UploadFailure,
            Self::ResponseTooLarge { .. } => ErrorCategory::ResponseTooLarge,
            Self::DecompressionFailure(_) => ErrorCategory::DecompressionFailure,
            Self::MalformedResponse(_) => ErrorCategory::MalformedResponse,
            Self::Http2Protocol(_) => ErrorCategory::Http2Protocol,
            Self::WebSocket(_) => ErrorCategory::WebSocket,
            Self::ServerSentEvents(_) => ErrorCategory::ServerSentEvents,
            Self::Grpc { .. } => ErrorCategory::Grpc,
            Self::ScriptTimeout => ErrorCategory::ScriptTimeout,
            Self::ScriptMemoryLimit => ErrorCategory::ScriptMemoryLimit,
            Self::AssertionFailure(_) => ErrorCategory::AssertionFailure,
            Self::UnresolvedVariable(_) => ErrorCategory::UnresolvedVariable,
            Self::MissingSecret(_) => ErrorCategory::MissingSecret,
            Self::InvalidWorkspace(_) => ErrorCategory::InvalidWorkspace,
            Self::MigrationFailure(_) => ErrorCategory::MigrationFailure,
            Self::ImportIncompatibility(_) => ErrorCategory::ImportIncompatibility,
            Self::FilesystemConflict(_) => ErrorCategory::FilesystemConflict,
            Self::DatabaseCorruption(_) => ErrorCategory::DatabaseCorruption,
            Self::Cancelled => ErrorCategory::Cancelled,
            Self::Internal(_) => ErrorCategory::Internal,
        }
    }
}

impl Display for ExecutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl(detail) => write!(formatter, "invalid URL: {detail}"),
            Self::DnsFailure(detail) => write!(formatter, "DNS lookup failed: {detail}"),
            Self::ConnectionRefused(detail) => write!(formatter, "connection refused: {detail}"),
            Self::ConnectionTimeout => formatter.write_str("connection timed out"),
            Self::TlsFailure(detail) => write!(formatter, "TLS negotiation failed: {detail}"),
            Self::CertificateFailure(detail) => {
                write!(formatter, "certificate validation failed: {detail}")
            }
            Self::ProxyFailure(detail) => write!(formatter, "proxy failed: {detail}"),
            Self::AuthenticationFailure(detail) => {
                write!(formatter, "authentication failed: {detail}")
            }
            Self::RedirectLoop => formatter.write_str("redirect loop detected"),
            Self::RequestTimeout => formatter.write_str("request timed out"),
            Self::UploadFailure(detail) => write!(formatter, "upload failed: {detail}"),
            Self::ResponseTooLarge { limit, observed } => {
                write!(
                    formatter,
                    "response exceeded {limit} bytes after receiving {observed} bytes"
                )
            }
            Self::DecompressionFailure(detail) => {
                write!(formatter, "response decompression failed: {detail}")
            }
            Self::MalformedResponse(detail) => write!(formatter, "malformed response: {detail}"),
            Self::Http2Protocol(detail) => write!(formatter, "HTTP/2 protocol error: {detail}"),
            Self::WebSocket(detail) => write!(formatter, "WebSocket error: {detail}"),
            Self::ServerSentEvents(detail) => write!(formatter, "SSE error: {detail}"),
            Self::Grpc { code, message } => write!(formatter, "gRPC status {code}: {message}"),
            Self::ScriptTimeout => formatter.write_str("script execution timed out"),
            Self::ScriptMemoryLimit => formatter.write_str("script memory limit exceeded"),
            Self::AssertionFailure(detail) => write!(formatter, "assertion failed: {detail}"),
            Self::UnresolvedVariable(name) => write!(formatter, "unresolved variable: {name}"),
            Self::MissingSecret(name) => write!(formatter, "missing secret: {name}"),
            Self::InvalidWorkspace(detail) => write!(formatter, "invalid workspace: {detail}"),
            Self::MigrationFailure(detail) => {
                write!(formatter, "workspace migration failed: {detail}")
            }
            Self::ImportIncompatibility(detail) => {
                write!(formatter, "import is incompatible: {detail}")
            }
            Self::FilesystemConflict(detail) => write!(formatter, "filesystem conflict: {detail}"),
            Self::DatabaseCorruption(detail) => write!(formatter, "database corruption: {detail}"),
            Self::Cancelled => formatter.write_str("operation cancelled"),
            Self::Internal(detail) => write!(formatter, "internal error: {detail}"),
        }
    }
}

impl std::error::Error for ExecutionError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    InvalidStableId(String),
    InvalidHttpMethod(String),
    InvalidHeaderName(String),
}

impl Display for DomainError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidStableId(value) => write!(formatter, "invalid stable identifier: {value}"),
            Self::InvalidHttpMethod(value) => write!(formatter, "invalid HTTP method: {value}"),
            Self::InvalidHeaderName(value) => {
                write!(formatter, "invalid HTTP header name: {value}")
            }
        }
    }
}

impl std::error::Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_headers(headers: Vec<HeaderEntry>) -> HttpRequest {
        HttpRequest {
            id: StableId::parse("request-1").expect("valid id"),
            name: "Duplicate headers".to_owned(),
            method: HttpMethod::Get,
            url: "https://example.test".to_owned(),
            query: Vec::new(),
            headers,
            authentication: Authentication::None,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: String::new(),
        }
    }

    #[test]
    fn preserves_ordered_duplicate_headers() {
        let request = request_with_headers(vec![
            HeaderEntry::new("X-Trace", "first").expect("valid header"),
            HeaderEntry::new("x-trace", "second").expect("valid header"),
        ]);
        assert_eq!(
            request.header_values("X-TRACE").collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn accepts_custom_token_methods() {
        assert_eq!(
            HttpMethod::parse("purge").expect("valid method").as_str(),
            "PURGE"
        );
        assert!(HttpMethod::parse("bad method").is_err());
    }

    #[test]
    fn cancellation_is_shared_between_clones() {
        let token = CancellationToken::default();
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled());
        assert_eq!(clone.check(), Err(ExecutionError::Cancelled));
    }
}
