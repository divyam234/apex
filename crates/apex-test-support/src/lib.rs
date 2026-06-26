#![forbid(unsafe_code)]

use apex_domain::{
    ExecutionError, ExecutionEvent, ExecutionId, ProtocolCapabilities, ProtocolId, TimingEntry,
    ValueSensitivity, VariableDefinition, VariableValue,
};
use apex_runner::{
    AdapterFuture, ExecutionContext, ExecutionEventSink, ExecutionResult, ProtocolAdapter,
    ProtocolRequest, ResolvedRequest, ResponseMetadata, StoredBody, ValidationError,
};
use apex_variables::DynamicVariableProvider;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub struct RecordingEventSink {
    events: Mutex<Vec<ExecutionEvent>>,
}

impl RecordingEventSink {
    pub fn events(&self) -> Vec<ExecutionEvent> {
        self.events
            .lock()
            .expect("recording event sink lock is not poisoned")
            .clone()
    }
}

impl ExecutionEventSink for RecordingEventSink {
    fn emit(&self, event: ExecutionEvent) {
        self.events
            .lock()
            .expect("recording event sink lock is not poisoned")
            .push(event);
    }
}

#[derive(Clone, Debug, Default)]
pub struct FixedDynamicVariables {
    values: BTreeMap<String, VariableDefinition>,
}

impl FixedDynamicVariables {
    pub fn with_public(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(
            name.into(),
            VariableDefinition {
                value: VariableValue::String(value.into()),
                sensitivity: ValueSensitivity::Public,
                enabled: true,
                description: Some("test fixture".to_owned()),
            },
        );
        self
    }
}

impl DynamicVariableProvider for FixedDynamicVariables {
    fn resolve(&self, name: &str) -> Option<VariableDefinition> {
        self.values.get(name).cloned()
    }
}

#[derive(Clone, Debug)]
pub struct FixtureAdapter {
    response: Result<ResponseMetadata, ExecutionError>,
}

impl FixtureAdapter {
    pub fn successful(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            response: Ok(ResponseMetadata {
                status: Some(status),
                status_text: Some("OK".to_owned()),
                protocol_version: "HTTP/1.1".to_owned(),
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                trailers: Vec::new(),
                received_bytes: 0,
                wire_bytes: 0,
                declared_content_length: None,
                content_type: Some("application/json".to_owned()),
                content_encoding: None,
                decompressed: false,
                redirect_chain: Vec::new(),
                stored_body: StoredBody::InMemory(body.into()),
            }),
        }
    }

    pub fn failing(error: ExecutionError) -> Self {
        Self {
            response: Err(error),
        }
    }
}

impl ProtocolAdapter for FixtureAdapter {
    fn protocol_id(&self) -> ProtocolId {
        ProtocolId::Http
    }

    fn capabilities(&self) -> ProtocolCapabilities {
        ProtocolCapabilities {
            streaming_input: false,
            streaming_output: false,
            bidirectional: false,
            cancellation: true,
            trailers: true,
        }
    }

    fn validate(&self, request: &ProtocolRequest) -> Result<(), ValidationError> {
        match request {
            ProtocolRequest::Http(request) if request.url.trim().is_empty() => {
                Err(ValidationError {
                    field: "url".to_owned(),
                    message: "URL is empty".to_owned(),
                })
            }
            ProtocolRequest::Http(_) => Ok(()),
        }
    }

    fn execute<'a>(
        &'a self,
        _request: ResolvedRequest,
        context: ExecutionContext,
        events: Arc<dyn ExecutionEventSink>,
    ) -> AdapterFuture<'a> {
        Box::pin(async move {
            events.emit(ExecutionEvent::Started {
                execution_id: context.execution_id,
            });
            context.cancellation.check()?;
            match &self.response {
                Ok(response) => {
                    events.emit(ExecutionEvent::Completed);
                    Ok(ExecutionResult {
                        execution_id: context.execution_id,
                        response: response.clone(),
                        timing: Vec::<TimingEntry>::new(),
                        diagnostics: Vec::new(),
                    })
                }
                Err(error) => Err(error.clone()),
            }
        })
    }
}

pub fn deterministic_execution_id() -> ExecutionId {
    ExecutionId::new()
}

pub mod http_fixture {
    use bytes::Bytes;
    use futures_util::stream;
    use http::header::{CONTENT_ENCODING, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE, TRAILER};
    use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
    use http_body_util::combinators::BoxBody;
    use http_body_util::{BodyExt as _, Full, StreamBody};
    use hyper::body::{Frame, Incoming};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio::time::Duration;

    type FixtureBody = BoxBody<Bytes, Infallible>;

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct CapturedRequest {
        pub method: String,
        pub path_and_query: String,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    pub struct HttpFixtureServer {
        address: SocketAddr,
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        shutdown: Option<oneshot::Sender<()>>,
        task: JoinHandle<()>,
    }

    impl HttpFixtureServer {
        pub async fn start() -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("fixture listener binds");
            let address = listener.local_addr().expect("fixture address available");
            let captured = Arc::new(Mutex::new(Vec::new()));
            let task_captured = Arc::clone(&captured);
            let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = &mut shutdown_receiver => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _peer)) = accepted else {
                                break;
                            };
                            let captured = Arc::clone(&task_captured);
                            tokio::spawn(async move {
                                let service = service_fn(move |request| {
                                    handle_request(request, Arc::clone(&captured))
                                });
                                let _ = hyper::server::conn::http1::Builder::new()
                                    .serve_connection(TokioIo::new(stream), service)
                                    .await;
                            });
                        }
                    }
                }
            });
            Self {
                address,
                captured,
                shutdown: Some(shutdown_sender),
                task,
            }
        }

        pub fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.address, path)
        }

        pub fn captured(&self) -> Vec<CapturedRequest> {
            self.captured
                .lock()
                .expect("fixture capture lock is not poisoned")
                .clone()
        }
    }

    impl Drop for HttpFixtureServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            self.task.abort();
        }
    }

    async fn handle_request(
        request: Request<Incoming>,
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
    ) -> Result<Response<FixtureBody>, Infallible> {
        let (parts, body) = request.into_parts();
        let body = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) => {
                return Ok(response(
                    StatusCode::BAD_REQUEST,
                    Bytes::from(format!("failed to read request body: {error}")),
                ));
            }
        };
        let request_cookie = parts
            .headers
            .get(COOKIE)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
            .unwrap_or_default();
        let captured_request = CapturedRequest {
            method: parts.method.to_string(),
            path_and_query: parts
                .uri
                .path_and_query()
                .map_or_else(|| "/".to_owned(), ToString::to_string),
            headers: header_pairs(&parts.headers),
            body: body.to_vec(),
        };
        captured
            .lock()
            .expect("fixture capture lock is not poisoned")
            .push(captured_request);

        let path = parts.uri.path();
        let response = match path {
            "/echo" | "/upload" => response(StatusCode::OK, body),
            "/duplicate" => {
                let mut response = response(StatusCode::OK, Bytes::from_static(b"duplicate"));
                response
                    .headers_mut()
                    .append("x-apex-value", HeaderValue::from_static("one"));
                response
                    .headers_mut()
                    .append("x-apex-value", HeaderValue::from_static("two"));
                response
            }
            "/redirect" => {
                let mut response = response(StatusCode::FOUND, Bytes::new());
                response
                    .headers_mut()
                    .insert(LOCATION, HeaderValue::from_static("/final"));
                response
            }
            "/final" => response(StatusCode::OK, Bytes::from_static(b"redirected")),
            "/cookie/set" => {
                let mut response = response(StatusCode::OK, Bytes::from_static(b"cookie set"));
                response.headers_mut().append(
                    SET_COOKIE,
                    HeaderValue::from_static("session=apex-cookie; Path=/; HttpOnly; SameSite=Lax"),
                );
                response
            }
            "/cookie/redirect" => {
                let mut response = response(StatusCode::FOUND, Bytes::new());
                response
                    .headers_mut()
                    .insert(LOCATION, HeaderValue::from_static("/cookie/echo"));
                response.headers_mut().append(
                    SET_COOKIE,
                    HeaderValue::from_static("redirect_cookie=stored; Path=/; HttpOnly"),
                );
                response
            }
            "/cookie/echo" => response(StatusCode::OK, Bytes::from(request_cookie)),
            "/compressed/gzip" => {
                compressed_response("gzip", Bytes::from(vec![b'g'; 128 * 1024])).await
            }
            "/compressed/br" => {
                compressed_response("br", Bytes::from(vec![b'b'; 128 * 1024])).await
            }
            "/compressed/zstd" => {
                compressed_response("zstd", Bytes::from(vec![b'z'; 128 * 1024])).await
            }
            "/trailers" => trailer_response(),
            "/slow" => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                response(StatusCode::OK, Bytes::from_static(b"slow"))
            }
            "/large" => response(StatusCode::OK, Bytes::from(vec![b'x'; 2 * 1024 * 1024])),
            "/status/204" => response(StatusCode::NO_CONTENT, Bytes::new()),
            _ => response(StatusCode::NOT_FOUND, Bytes::from_static(b"not found")),
        };
        Ok(response)
    }

    async fn compressed_response(encoding: &str, body: Bytes) -> Response<FixtureBody> {
        let (writer, mut reader) = tokio::io::duplex(body.len().saturating_mul(2).max(4096));
        let body_for_task = body.clone();
        let encoding_for_task = encoding.to_owned();
        let task = tokio::spawn(async move {
            match encoding_for_task.as_str() {
                "gzip" => {
                    let mut encoder = async_compression::tokio::write::GzipEncoder::new(writer);
                    encoder.write_all(&body_for_task).await.expect("gzip write");
                    encoder.shutdown().await.expect("gzip finish");
                }
                "br" => {
                    let mut encoder = async_compression::tokio::write::BrotliEncoder::new(writer);
                    encoder
                        .write_all(&body_for_task)
                        .await
                        .expect("brotli write");
                    encoder.shutdown().await.expect("brotli finish");
                }
                "zstd" => {
                    let mut encoder = async_compression::tokio::write::ZstdEncoder::new(writer);
                    encoder.write_all(&body_for_task).await.expect("zstd write");
                    encoder.shutdown().await.expect("zstd finish");
                }
                _ => unreachable!(),
            }
        });
        let mut encoded = Vec::new();
        reader
            .read_to_end(&mut encoded)
            .await
            .expect("compressed read");
        task.await.expect("compression task");
        let mut response = response(StatusCode::OK, Bytes::from(encoded));
        response.headers_mut().insert(
            CONTENT_ENCODING,
            HeaderValue::from_str(encoding).expect("static encoding"),
        );
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        response
    }

    fn response(status: StatusCode, body: Bytes) -> Response<FixtureBody> {
        let mut response = Response::new(Full::new(body).boxed());
        *response.status_mut() = status;
        response
    }

    fn trailer_response() -> Response<FixtureBody> {
        let frames = stream::unfold(0_u8, |state| async move {
            let frame = match state {
                0 => Frame::data(Bytes::from_static(b"hello ")),
                1 => Frame::data(Bytes::from_static(b"trailers")),
                2 => {
                    let mut trailers = HeaderMap::new();
                    trailers.insert("x-apex-checksum", HeaderValue::from_static("verified"));
                    Frame::trailers(trailers)
                }
                _ => return None,
            };
            Some((Ok::<_, Infallible>(frame), state + 1))
        });
        let mut response = Response::new(StreamBody::new(frames).boxed());
        response
            .headers_mut()
            .insert(TRAILER, HeaderValue::from_static("x-apex-checksum"));
        response
    }

    fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{HttpMethod, HttpRequest, RequestBody, RequestSettings, StableId};
    use apex_runner::ProtocolRequest;

    #[test]
    fn fixture_adapter_is_explicitly_test_only_and_cancellable() {
        let adapter = FixtureAdapter::successful(200, b"{}".to_vec());
        let request = ProtocolRequest::Http(HttpRequest {
            id: StableId::parse("fixture").expect("valid id"),
            name: "Fixture".to_owned(),
            method: HttpMethod::Get,
            url: "https://example.test".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication: apex_domain::Authentication::None,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: String::new(),
        });
        assert!(adapter.validate(&request).is_ok());
    }
}
