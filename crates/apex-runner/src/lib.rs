#![forbid(unsafe_code)]

use apex_domain::{
    CancellationToken, ExecutionError, ExecutionEvent, ExecutionId, HttpRequest,
    ProtocolCapabilities, ProtocolId, TimingEntry,
};
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug)]
pub enum ProtocolRequest {
    Http(HttpRequest),
}

#[derive(Clone, Debug)]
pub struct ResolvedRequest {
    pub request: ProtocolRequest,
    pub redacted_summary: String,
}

#[derive(Clone, Debug)]
pub struct ExecutionContext {
    pub execution_id: ExecutionId,
    pub cancellation: CancellationToken,
    pub started_at: SystemTime,
    pub timeout: Duration,
    pub maximum_response_bytes: u64,
    pub download_target: Option<PathBuf>,
    pub resource_root: Option<PathBuf>,
    pub memory_response_threshold: u64,
    pub overwrite_download: bool,
}

impl ExecutionContext {
    pub fn new(timeout: Duration, maximum_response_bytes: u64) -> Self {
        Self {
            execution_id: ExecutionId::new(),
            cancellation: CancellationToken::default(),
            started_at: SystemTime::now(),
            timeout,
            maximum_response_bytes,
            download_target: None,
            resource_root: None,
            memory_response_threshold: 8 * 1024 * 1024,
            overwrite_download: false,
        }
    }
}

pub trait ExecutionEventSink: Send + Sync {
    fn emit(&self, event: ExecutionEvent);
}

#[derive(Clone, Debug)]
pub struct ChannelEventSink {
    sender: mpsc::Sender<ExecutionEvent>,
}

impl ChannelEventSink {
    pub fn new(sender: mpsc::Sender<ExecutionEvent>) -> Self {
        Self { sender }
    }
}

impl ExecutionEventSink for ChannelEventSink {
    fn emit(&self, event: ExecutionEvent) {
        let _ = self.sender.send(event);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseMetadata {
    pub status: Option<u16>,
    pub status_text: Option<String>,
    pub protocol_version: String,
    pub headers: Vec<(String, String)>,
    pub trailers: Vec<(String, String)>,
    pub received_bytes: u64,
    pub wire_bytes: u64,
    pub declared_content_length: Option<u64>,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub decompressed: bool,
    pub redirect_chain: Vec<RedirectHop>,
    pub stored_body: StoredBody,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedirectHop {
    pub status: u16,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredBody {
    Empty,
    InMemory(Vec<u8>),
    File { path: PathBuf, temporary: bool },
    StreamLog(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionResult {
    pub execution_id: ExecutionId,
    pub response: ResponseMetadata,
    pub timing: Vec<TimingEntry>,
    pub diagnostics: Vec<String>,
}

pub type AdapterFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ExecutionResult, ExecutionError>> + Send + 'a>>;

pub trait ProtocolAdapter: Send + Sync {
    fn protocol_id(&self) -> ProtocolId;
    fn capabilities(&self) -> ProtocolCapabilities;
    fn validate(&self, request: &ProtocolRequest) -> Result<(), ValidationError>;
    fn execute<'a>(
        &'a self,
        request: ResolvedRequest,
        context: ExecutionContext,
        events: Arc<dyn ExecutionEventSink>,
    ) -> AdapterFuture<'a>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl Display for ValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConcurrencyPolicy {
    maximum_in_flight: usize,
}

impl ConcurrencyPolicy {
    pub fn bounded(maximum_in_flight: usize) -> Result<Self, RunnerConfigurationError> {
        if maximum_in_flight == 0 {
            Err(RunnerConfigurationError::ZeroConcurrency)
        } else if maximum_in_flight > 1024 {
            Err(RunnerConfigurationError::ConcurrencyTooHigh(
                maximum_in_flight,
            ))
        } else {
            Ok(Self { maximum_in_flight })
        }
    }

    pub fn maximum_in_flight(&self) -> usize {
        self.maximum_in_flight
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunnerConfigurationError {
    ZeroConcurrency,
    ConcurrencyTooHigh(usize),
}

impl Display for RunnerConfigurationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroConcurrency => formatter.write_str("runner concurrency must be at least one"),
            Self::ConcurrencyTooHigh(value) => {
                write!(
                    formatter,
                    "runner concurrency {value} exceeds the safety limit of 1024"
                )
            }
        }
    }
}

impl std::error::Error for RunnerConfigurationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_is_always_bounded() {
        assert!(ConcurrencyPolicy::bounded(0).is_err());
        assert_eq!(
            ConcurrencyPolicy::bounded(8)
                .expect("valid policy")
                .maximum_in_flight(),
            8
        );
        assert!(ConcurrencyPolicy::bounded(2048).is_err());
    }
}
