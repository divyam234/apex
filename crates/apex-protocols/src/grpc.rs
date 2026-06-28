use apex_domain::CancellationToken;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum GrpcInteractionMode {
    Unary,
    ServerStreaming,
    ClientStreaming,
    BidirectionalStreaming,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct GrpcMethodDescriptor {
    pub service: String,
    pub method: String,
    pub input_type: String,
    pub output_type: String,
    pub mode: GrpcInteractionMode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GrpcRequest {
    pub endpoint: String,
    pub method: GrpcMethodDescriptor,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub messages: Vec<Value>,
    pub tls: bool,
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GrpcEvent {
    pub sequence: u64,
    pub timestamp_ms: u128,
    pub direction: GrpcEventDirection,
    pub message: Option<Value>,
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum GrpcEventDirection {
    Sent,
    Received,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GrpcCallResult {
    pub status_code: i32,
    pub status_message: String,
    pub initial_metadata: BTreeMap<String, String>,
    pub trailers: BTreeMap<String, String>,
    pub events: Vec<GrpcEvent>,
    pub dropped_events: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct GrpcTransportResult {
    pub status_code: i32,
    pub status_message: String,
    pub initial_metadata: BTreeMap<String, String>,
    pub trailers: BTreeMap<String, String>,
}

pub trait GrpcTransport: Send + Sync {
    fn invoke(
        &self,
        request: &GrpcRequest,
        cancellation: &CancellationToken,
        events: &mut GrpcEventLog,
    ) -> Result<GrpcTransportResult, String>;
}

#[derive(Clone, Debug)]
pub struct GrpcEventLog {
    capacity: usize,
    maximum_message_bytes: usize,
    next_sequence: u64,
    dropped: u64,
    events: VecDeque<GrpcEvent>,
}

impl GrpcEventLog {
    pub fn new(capacity: usize, maximum_message_bytes: usize) -> Result<Self, String> {
        if capacity == 0 || capacity > 1_000_000 {
            return Err("gRPC event capacity must be between 1 and 1000000".to_owned());
        }
        if maximum_message_bytes == 0 || maximum_message_bytes > 64 * 1024 * 1024 {
            return Err("gRPC message byte limit must be between 1 and 64 MiB".to_owned());
        }
        Ok(Self {
            capacity,
            maximum_message_bytes,
            next_sequence: 1,
            dropped: 0,
            events: VecDeque::with_capacity(capacity.min(4096)),
        })
    }

    pub fn push(
        &mut self,
        direction: GrpcEventDirection,
        message: Option<Value>,
        note: Option<String>,
    ) -> Result<(), String> {
        if let Some(message) = &message {
            let bytes = serde_json::to_vec(message)
                .map_err(|error| format!("could not serialize gRPC message: {error}"))?;
            if bytes.len() > self.maximum_message_bytes {
                self.dropped = self.dropped.saturating_add(1);
                return Err("gRPC message exceeds configured byte limit".to_owned());
            }
        }
        if self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
            .as_millis();
        self.events.push_back(GrpcEvent {
            sequence: self.next_sequence,
            timestamp_ms,
            direction,
            message,
            note,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(())
    }

    fn into_parts(self) -> (Vec<GrpcEvent>, u64) {
        (self.events.into_iter().collect(), self.dropped)
    }
}

pub fn validate_grpc_request(request: &GrpcRequest) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    let valid_scheme =
        request.endpoint.starts_with("http://") || request.endpoint.starts_with("https://");
    if !valid_scheme {
        errors.push("gRPC endpoint must use http or https".to_owned());
    }
    if request.tls && !request.endpoint.starts_with("https://") {
        errors.push("TLS-enabled gRPC requests require an https endpoint".to_owned());
    }
    if request.deadline_ms == 0 || request.deadline_ms > 24 * 60 * 60 * 1000 {
        errors.push("gRPC deadline must be between 1 ms and 24 hours".to_owned());
    }
    if request.method.service.trim().is_empty() || request.method.method.trim().is_empty() {
        errors.push("gRPC service and method names must not be empty".to_owned());
    }
    match request.method.mode {
        GrpcInteractionMode::Unary | GrpcInteractionMode::ServerStreaming => {
            if request.messages.len() != 1 {
                errors.push(
                    "unary and server-streaming calls require exactly one request message"
                        .to_owned(),
                );
            }
        }
        GrpcInteractionMode::ClientStreaming | GrpcInteractionMode::BidirectionalStreaming => {
            if request.messages.is_empty() {
                errors.push(
                    "client and bidirectional streaming calls require at least one message"
                        .to_owned(),
                );
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn execute_grpc_call(
    request: &GrpcRequest,
    cancellation: CancellationToken,
    capacity: usize,
    maximum_message_bytes: usize,
    transport: &dyn GrpcTransport,
) -> Result<GrpcCallResult, String> {
    validate_grpc_request(request).map_err(|errors| errors.join("; "))?;
    if cancellation.is_cancelled() {
        return Err("gRPC call was cancelled".to_owned());
    }
    let mut events = GrpcEventLog::new(capacity, maximum_message_bytes)?;
    let deadline = Duration::from_millis(request.deadline_ms);
    let started = std::time::Instant::now();
    for message in &request.messages {
        events.push(GrpcEventDirection::Sent, Some(message.clone()), None)?;
    }
    let transport_result = transport.invoke(request, &cancellation, &mut events)?;
    if cancellation.is_cancelled() {
        return Err("gRPC call was cancelled".to_owned());
    }
    if started.elapsed() > deadline {
        return Err("gRPC call exceeded its deadline".to_owned());
    }
    events.push(
        GrpcEventDirection::System,
        None,
        Some(format!(
            "status {}: {}",
            transport_result.status_code, transport_result.status_message
        )),
    )?;
    let (events, dropped_events) = events.into_parts();
    Ok(GrpcCallResult {
        status_code: transport_result.status_code,
        status_message: transport_result.status_message,
        initial_metadata: transport_result.initial_metadata,
        trailers: transport_result.trailers,
        events,
        dropped_events,
    })
}

#[derive(Clone, Debug, Default)]
pub struct FixtureGrpcTransport {
    pub responses: Vec<Value>,
    pub status_code: i32,
    pub status_message: String,
    pub initial_metadata: BTreeMap<String, String>,
    pub trailers: BTreeMap<String, String>,
}

impl GrpcTransport for FixtureGrpcTransport {
    fn invoke(
        &self,
        request: &GrpcRequest,
        cancellation: &CancellationToken,
        events: &mut GrpcEventLog,
    ) -> Result<GrpcTransportResult, String> {
        for response in &self.responses {
            if cancellation.is_cancelled() {
                return Err("gRPC fixture transport was cancelled".to_owned());
            }
            events.push(GrpcEventDirection::Received, Some(response.clone()), None)?;
            if request.method.mode == GrpcInteractionMode::Unary {
                break;
            }
        }
        Ok(GrpcTransportResult {
            status_code: self.status_code,
            status_message: self.status_message.clone(),
            initial_metadata: self.initial_metadata.clone(),
            trailers: self.trailers.clone(),
        })
    }
}

pub fn load_descriptor_set(bytes: &[u8], maximum_bytes: usize) -> Result<Vec<u8>, String> {
    if bytes.is_empty() {
        return Err("descriptor set is empty".to_owned());
    }
    if bytes.len() > maximum_bytes {
        return Err("descriptor set exceeds configured byte limit".to_owned());
    }
    Ok(bytes.to_vec())
}

pub fn reflection_request(service: &str) -> Result<Value, String> {
    if service.is_empty() || service.len() > 1024 {
        return Err("reflection service name must be between 1 and 1024 bytes".to_owned());
    }
    Ok(serde_json::json!({"file_containing_symbol": service}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(mode: GrpcInteractionMode, messages: Vec<Value>) -> GrpcRequest {
        GrpcRequest {
            endpoint: "https://localhost:50051".to_owned(),
            method: GrpcMethodDescriptor {
                service: "example.Greeter".to_owned(),
                method: "Hello".to_owned(),
                input_type: "HelloRequest".to_owned(),
                output_type: "HelloReply".to_owned(),
                mode,
            },
            metadata: BTreeMap::from([("authorization".to_owned(), "[REDACTED]".to_owned())]),
            messages,
            tls: true,
            deadline_ms: 1000,
        }
    }

    #[test]
    fn all_interaction_modes_execute_against_fixture() {
        let fixture = FixtureGrpcTransport {
            responses: vec![json!({"message":"one"}), json!({"message":"two"})],
            status_code: 0,
            status_message: "OK".to_owned(),
            trailers: BTreeMap::from([("checksum".to_owned(), "abc".to_owned())]),
            ..FixtureGrpcTransport::default()
        };
        for (mode, messages) in [
            (GrpcInteractionMode::Unary, vec![json!({"name":"a"})]),
            (
                GrpcInteractionMode::ServerStreaming,
                vec![json!({"name":"a"})],
            ),
            (
                GrpcInteractionMode::ClientStreaming,
                vec![json!({"name":"a"}), json!({"name":"b"})],
            ),
            (
                GrpcInteractionMode::BidirectionalStreaming,
                vec![json!({"name":"a"}), json!({"name":"b"})],
            ),
        ] {
            let result = execute_grpc_call(
                &request(mode, messages),
                CancellationToken::default(),
                16,
                1024,
                &fixture,
            )
            .expect("fixture call succeeds");
            assert_eq!(result.status_code, 0);
            assert_eq!(result.trailers["checksum"], "abc");
            assert!(
                result
                    .events
                    .iter()
                    .any(|event| event.direction == GrpcEventDirection::Received)
            );
        }
    }

    #[test]
    fn bounded_event_log_tracks_drops_and_cancellation() {
        let mut log = GrpcEventLog::new(1, 16).expect("log");
        log.push(GrpcEventDirection::Received, Some(json!({"a":1})), None)
            .expect("first");
        log.push(GrpcEventDirection::Received, Some(json!({"b":2})), None)
            .expect("second");
        let (_, dropped) = log.into_parts();
        assert_eq!(dropped, 1);
        let token = CancellationToken::default();
        token.cancel();
        let error = execute_grpc_call(
            &request(GrpcInteractionMode::Unary, vec![json!({"a":1})]),
            token,
            4,
            1024,
            &FixtureGrpcTransport::default(),
        )
        .expect_err("cancelled call fails");
        assert!(error.contains("cancelled"));
    }
}
