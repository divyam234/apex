#![forbid(unsafe_code)]

use apex_auth::apply_authentication;
use apex_domain::{
    CancellationToken, ErrorCategory, ExecutionError, ExecutionEvent, FormField, HeaderEntry,
    HttpMethod, MultipartField, MultipartValue, ProtocolCapabilities, ProtocolId, RequestBody,
    TimingEntry, TimingPhase, TimingValue,
};
use apex_runner::{
    AdapterFuture, ExecutionContext, ExecutionEventSink, ExecutionResult, ProtocolAdapter,
    ProtocolRequest, RedirectHop, ResolvedRequest, ResponseMetadata, StoredBody, ValidationError,
};
use bytes::Bytes;
use cookie_store::CookieStore;
use futures_util::stream::{self, Stream};
use futures_util::{StreamExt as _, TryStreamExt as _};
use http::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE,
    TE, USER_AGENT,
};
use http::{HeaderName, HeaderValue, Method, Request, Uri, Version};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::Frame;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::{Client, Error as ClientError};
use hyper_util::rt::TokioExecutor;
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::error::Error as StdError;
use std::fmt::Write as _;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::time::Instant;
use tokio_util::io::ReaderStream;
use url::Url;

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_USER_AGENT: &str = concat!("ApexAPI/", env!("CARGO_PKG_VERSION"));
const MAXIMUM_REDIRECTS_HARD_LIMIT: u16 = 100;

type BoxError = Box<dyn StdError + Send + Sync>;
type ApexRequestBody = UnsyncBoxBody<Bytes, BoxError>;
type ApexClient = Client<HttpsConnector<HttpConnector>, ApexRequestBody>;
type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, BoxError>> + Send>>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ClientKey {
    connection_timeout_ms: u64,
    idle_timeout_ms: u64,
}

pub struct HttpAdapter {
    clients: Mutex<BTreeMap<ClientKey, ApexClient>>,
    cookies: Mutex<CookieStore>,
}

impl Default for HttpAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpAdapter {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(BTreeMap::new()),
            cookies: Mutex::new(CookieStore::default()),
        }
    }

    fn client_for(
        &self,
        connection_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<ApexClient, ExecutionError> {
        let key = ClientKey {
            connection_timeout_ms: duration_millis_saturated(connection_timeout),
            idle_timeout_ms: duration_millis_saturated(idle_timeout),
        };
        if let Some(client) = self
            .clients
            .lock()
            .map_err(|_| ExecutionError::Internal("HTTP client cache is poisoned".to_owned()))?
            .get(&key)
            .cloned()
        {
            return Ok(client);
        }

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        connector.set_connect_timeout(Some(connection_timeout));
        connector.set_happy_eyeballs_timeout(Some(Duration::from_millis(250)));
        connector.set_nodelay(true);

        let https = HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|error| ExecutionError::CertificateFailure(error.to_string()))?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(connector);
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(idle_timeout)
            .build(https);
        self.clients
            .lock()
            .map_err(|_| ExecutionError::Internal("HTTP client cache is poisoned".to_owned()))?
            .insert(key, client.clone());
        Ok(client)
    }

    pub fn clear_cookies(&self) -> Result<(), ExecutionError> {
        self.cookies
            .lock()
            .map_err(|_| ExecutionError::Internal("cookie jar is poisoned".to_owned()))?
            .clear();
        Ok(())
    }

    fn apply_cookie_header(&self, plan: &mut RequestPlan) -> Result<(), ExecutionError> {
        if plan
            .headers
            .iter()
            .any(|header| header.enabled && header.name.eq_ignore_ascii_case("cookie"))
        {
            return Ok(());
        }
        let cookie = self
            .cookies
            .lock()
            .map_err(|_| ExecutionError::Internal("cookie jar is poisoned".to_owned()))?
            .get_request_values(&plan.url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if !cookie.is_empty() {
            let mut header = HeaderEntry::new(COOKIE.as_str(), cookie)
                .map_err(|error| ExecutionError::Internal(error.to_string()))?;
            header.sensitivity = apex_domain::ValueSensitivity::Secret;
            plan.headers.push(header);
        }
        Ok(())
    }

    fn store_response_cookies(
        &self,
        url: &Url,
        headers: &http::HeaderMap,
    ) -> Result<Vec<String>, ExecutionError> {
        let mut diagnostics = Vec::new();
        let mut store = self
            .cookies
            .lock()
            .map_err(|_| ExecutionError::Internal("cookie jar is poisoned".to_owned()))?;
        for value in headers.get_all(SET_COOKIE) {
            match value.to_str() {
                Ok(value) => {
                    if let Err(error) = store.parse(value, url) {
                        diagnostics.push(format!("ignored invalid Set-Cookie header: {error}"));
                    }
                }
                Err(error) => {
                    diagnostics.push(format!("ignored non-text Set-Cookie header: {error}"))
                }
            }
        }
        Ok(diagnostics)
    }

    async fn execute_http(
        &self,
        mut request: apex_domain::HttpRequest,
        context: ExecutionContext,
        events: Arc<dyn ExecutionEventSink>,
    ) -> Result<ExecutionResult, ExecutionError> {
        apply_authentication(&mut request)?;
        let client = self.client_for(
            request.settings.connection_timeout,
            request.settings.idle_timeout,
        )?;
        let initial_url = build_initial_url(&request)?;
        let mut plan = RequestPlan {
            method: request.method.clone(),
            url: initial_url,
            headers: request.headers.clone(),
            body: request.body.clone(),
            decompress_response: request.settings.decompress_response,
        };
        let mut visited = BTreeSet::new();
        visited.insert(plan.url.as_str().to_owned());
        let mut redirect_chain = Vec::new();
        let mut server_wait = Duration::ZERO;
        let mut diagnostics = Vec::new();

        loop {
            context.cancellation.check()?;
            let mut attempt = plan.clone();
            if request.settings.cookie_jar {
                self.apply_cookie_header(&mut attempt)?;
            }
            let prepared = prepare_request(&attempt, &context, Arc::clone(&events)).await?;
            let request_started = Instant::now();
            let response = client.request(prepared).await.map_err(map_client_error)?;
            server_wait = server_wait.saturating_add(request_started.elapsed());
            let status = response.status();
            if request.settings.cookie_jar {
                diagnostics.extend(self.store_response_cookies(&plan.url, response.headers())?);
            }
            events.emit(ExecutionEvent::ResponseHeaders {
                status: status.as_u16(),
                http_version: version_name(response.version()).to_owned(),
            });

            let redirect = redirect_target(&plan.url, &response)?;
            if request.settings.follow_redirects
                && status.is_redirection()
                && let Some(next_url) = redirect
            {
                if redirect_chain.len() >= usize::from(request.settings.redirect_limit) {
                    return Err(ExecutionError::RedirectLoop);
                }
                if !visited.insert(next_url.as_str().to_owned()) {
                    return Err(ExecutionError::RedirectLoop);
                }
                redirect_chain.push(RedirectHop {
                    status: status.as_u16(),
                    from: plan.url.as_str().to_owned(),
                    to: next_url.as_str().to_owned(),
                });
                apply_redirect(&mut plan, status.as_u16(), next_url);
                continue;
            }

            let download_started = Instant::now();
            let (response, decompression) = collect_response(
                response,
                &context,
                request.settings.idle_timeout,
                request.settings.maximum_wire_response_bytes,
                request.settings.decompress_response,
                Arc::clone(&events),
                redirect_chain,
            )
            .await?;
            let download = download_started.elapsed();
            return Ok(ExecutionResult {
                execution_id: context.execution_id,
                response,
                timing: vec![
                    TimingEntry {
                        phase: TimingPhase::Dns,
                        value: TimingValue::Unavailable,
                    },
                    TimingEntry {
                        phase: TimingPhase::TcpConnect,
                        value: TimingValue::Unavailable,
                    },
                    TimingEntry {
                        phase: TimingPhase::Tls,
                        value: TimingValue::Unavailable,
                    },
                    TimingEntry {
                        phase: TimingPhase::Upload,
                        value: TimingValue::Unavailable,
                    },
                    TimingEntry {
                        phase: TimingPhase::ServerWait,
                        value: TimingValue::Available(server_wait),
                    },
                    TimingEntry {
                        phase: TimingPhase::Download,
                        value: TimingValue::Available(download),
                    },
                    TimingEntry {
                        phase: TimingPhase::Decompression,
                        value: decompression
                            .map(TimingValue::Available)
                            .unwrap_or(TimingValue::Unavailable),
                    },
                ],
                diagnostics: {
                    diagnostics.push(
                        "DNS, TCP, TLS, and exact upload timing are unavailable through the pooled Hyper connector and are not fabricated."
                            .to_owned(),
                    );
                    diagnostics
                },
            });
        }
    }
}

impl ProtocolAdapter for HttpAdapter {
    fn protocol_id(&self) -> ProtocolId {
        ProtocolId::Http
    }

    fn capabilities(&self) -> ProtocolCapabilities {
        ProtocolCapabilities {
            streaming_input: true,
            streaming_output: true,
            bidirectional: false,
            cancellation: true,
            trailers: true,
        }
    }

    fn validate(&self, request: &ProtocolRequest) -> Result<(), ValidationError> {
        let ProtocolRequest::Http(request) = request;
        validate_request(request)
    }

    fn execute<'a>(
        &'a self,
        request: ResolvedRequest,
        context: ExecutionContext,
        events: Arc<dyn ExecutionEventSink>,
    ) -> AdapterFuture<'a> {
        Box::pin(async move {
            events.emit(ExecutionEvent::Started {
                execution_id: context.execution_id,
            });
            let redacted_summary = request.redacted_summary.clone();
            if let Err(error) = self.validate(&request.request) {
                let execution_error = validation_to_execution_error(&error);
                events.emit(ExecutionEvent::Failed {
                    category: execution_error.category(),
                    redacted_summary: redacted_failure_summary(
                        &redacted_summary,
                        execution_error.category(),
                    ),
                });
                return Err(execution_error);
            }

            let cancellation = context.cancellation.clone();
            let timeout = context.timeout;
            let ProtocolRequest::Http(http_request) = request.request;
            let execution = self.execute_http(http_request, context, Arc::clone(&events));
            let result = tokio::select! {
                _ = wait_for_cancellation(cancellation) => Err(ExecutionError::Cancelled),
                timed = tokio::time::timeout(timeout, execution) => {
                    match timed {
                        Ok(result) => result,
                        Err(_) => Err(ExecutionError::RequestTimeout),
                    }
                }
            };

            match &result {
                Ok(_) => events.emit(ExecutionEvent::Completed),
                Err(ExecutionError::Cancelled) => events.emit(ExecutionEvent::Cancelled),
                Err(error) => events.emit(ExecutionEvent::Failed {
                    category: error.category(),
                    redacted_summary: redacted_failure_summary(&redacted_summary, error.category()),
                }),
            }
            result
        })
    }
}

fn validate_request(request: &apex_domain::HttpRequest) -> Result<(), ValidationError> {
    if request.settings.timeout.is_zero() {
        return Err(validation(
            "settings.timeout",
            "timeout must be greater than zero",
        ));
    }
    if request.settings.connection_timeout.is_zero() {
        return Err(validation(
            "settings.connection_timeout",
            "connection timeout must be greater than zero",
        ));
    }
    if request.settings.idle_timeout.is_zero() {
        return Err(validation(
            "settings.idle_timeout",
            "idle timeout must be greater than zero",
        ));
    }
    if request.settings.maximum_response_bytes == 0 {
        return Err(validation(
            "settings.maximum_response_bytes",
            "maximum response bytes must be greater than zero",
        ));
    }
    if request.settings.maximum_wire_response_bytes == 0 {
        return Err(validation(
            "settings.maximum_wire_response_bytes",
            "maximum wire response bytes must be greater than zero",
        ));
    }
    if request.settings.redirect_limit > MAXIMUM_REDIRECTS_HARD_LIMIT {
        return Err(validation(
            "settings.redirect_limit",
            format!(
                "redirect limit exceeds the hard safety limit of {MAXIMUM_REDIRECTS_HARD_LIMIT}"
            ),
        ));
    }
    if !request.settings.verify_certificates {
        return Err(validation(
            "settings.verify_certificates",
            "disabling certificate verification is not implemented; ApexAPI will not silently ignore this setting",
        ));
    }

    let url = Url::parse(&request.url)
        .map_err(|error| validation("url", format!("invalid URL: {error}")))?;
    validate_url(&url)?;

    let mut host_count = 0_usize;
    for (index, header) in request.enabled_headers().enumerate() {
        HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|error| validation(format!("headers[{index}].name"), error.to_string()))?;
        HeaderValue::from_str(&header.value)
            .map_err(|error| validation(format!("headers[{index}].value"), error.to_string()))?;
        if header.name.eq_ignore_ascii_case("host") {
            host_count += 1;
        }
    }
    if host_count > 1 {
        return Err(validation(
            "headers",
            "HTTP requests may contain only one Host header",
        ));
    }
    validate_body_paths(&request.body)?;
    Ok(())
}

fn validate_url(url: &Url) -> Result<(), ValidationError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(validation(
            "url",
            "the stable HTTP adapter accepts only http:// and https:// URLs",
        ));
    }
    if url.host_str().is_none() {
        return Err(validation("url", "URL must contain a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(validation(
            "url",
            "URL userinfo is rejected; configure authentication explicitly so credentials can be redacted",
        ));
    }
    if url.fragment().is_some() {
        return Err(validation(
            "url",
            "URL fragments are not transmitted in HTTP requests and must be removed explicitly",
        ));
    }
    Ok(())
}

fn validate_body_paths(body: &RequestBody) -> Result<(), ValidationError> {
    match body {
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            validate_relative_path(relative_path)
                .map_err(|message| validation("body.relative_path", message))?;
        }
        RequestBody::Multipart(fields) => {
            for (index, field) in fields.iter().enumerate() {
                if let MultipartValue::File { relative_path } = &field.value {
                    validate_relative_path(relative_path).map_err(|message| {
                        validation(format!("body.multipart[{index}].relative_path"), message)
                    })?;
                }
            }
        }
        RequestBody::Empty
        | RequestBody::Text { .. }
        | RequestBody::Json(_)
        | RequestBody::Xml(_)
        | RequestBody::GraphQl { .. }
        | RequestBody::FormUrlEncoded(_) => {}
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("resource path must be a non-empty relative path".to_owned());
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("resource path may not escape its workspace root".to_owned());
            }
        }
    }
    Ok(())
}

fn build_initial_url(request: &apex_domain::HttpRequest) -> Result<Url, ExecutionError> {
    let mut url =
        Url::parse(&request.url).map_err(|error| ExecutionError::InvalidUrl(error.to_string()))?;
    {
        let mut query = url.query_pairs_mut();
        for field in request.query.iter().filter(|field| field.enabled) {
            query.append_pair(&field.name, &field.value);
        }
    }
    Ok(url)
}

#[derive(Clone)]
struct RequestPlan {
    method: HttpMethod,
    url: Url,
    headers: Vec<HeaderEntry>,
    body: RequestBody,
    decompress_response: bool,
}

fn plan_supports_decompression(plan: &RequestPlan) -> bool {
    plan.decompress_response
}

async fn prepare_request(
    plan: &RequestPlan,
    context: &ExecutionContext,
    events: Arc<dyn ExecutionEventSink>,
) -> Result<Request<ApexRequestBody>, ExecutionError> {
    let method = Method::from_bytes(plan.method.as_str().as_bytes())
        .map_err(|error| ExecutionError::MalformedResponse(error.to_string()))?;
    let uri: Uri = plan
        .url
        .as_str()
        .parse()
        .map_err(|error: http::uri::InvalidUri| ExecutionError::InvalidUrl(error.to_string()))?;
    let prepared_body = prepare_body(&plan.body, context, events).await?;
    let mut request = Request::new(prepared_body.body);
    *request.method_mut() = method;
    *request.uri_mut() = uri;

    for header in plan.headers.iter().filter(|header| header.enabled) {
        let name = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|error| ExecutionError::MalformedResponse(error.to_string()))?;
        let value = HeaderValue::from_str(&header.value)
            .map_err(|error| ExecutionError::MalformedResponse(error.to_string()))?;
        request.headers_mut().append(name, value);
    }
    if !request.headers().contains_key(USER_AGENT) {
        request
            .headers_mut()
            .insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
    }
    if !request.headers().contains_key(TE) {
        request
            .headers_mut()
            .insert(TE, HeaderValue::from_static("trailers"));
    }
    if plan_supports_decompression(plan) && !request.headers().contains_key(ACCEPT_ENCODING) {
        request
            .headers_mut()
            .insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip, br, zstd"));
    }
    if let Some(content_type) = prepared_body.content_type
        && !request.headers().contains_key(CONTENT_TYPE)
    {
        request.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&content_type)
                .map_err(|error| ExecutionError::MalformedResponse(error.to_string()))?,
        );
    }
    if let Some(content_length) = prepared_body.content_length {
        validate_or_insert_content_length(request.headers_mut(), content_length)?;
    }
    Ok(request)
}

struct PreparedBody {
    body: ApexRequestBody,
    content_type: Option<String>,
    content_length: Option<u64>,
}

async fn prepare_body(
    body: &RequestBody,
    context: &ExecutionContext,
    events: Arc<dyn ExecutionEventSink>,
) -> Result<PreparedBody, ExecutionError> {
    match body {
        RequestBody::Empty => Ok(PreparedBody {
            body: full_body(Bytes::new()),
            content_type: None,
            content_length: None,
        }),
        RequestBody::Text { content_type, text } => Ok(full_prepared_body(
            text.as_bytes(),
            content_type.clone(),
            &events,
        )),
        RequestBody::Json(text) => Ok(full_prepared_body(
            text.as_bytes(),
            Some("application/json".to_owned()),
            &events,
        )),
        RequestBody::Xml(text) => Ok(full_prepared_body(
            text.as_bytes(),
            Some("application/xml".to_owned()),
            &events,
        )),
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => {
            let variables: JsonValue = serde_json::from_str(variables_json).map_err(|error| {
                ExecutionError::UploadFailure(format!("invalid GraphQL variables JSON: {error}"))
            })?;
            let mut object = JsonMap::new();
            object.insert("query".to_owned(), JsonValue::String(query.clone()));
            object.insert("variables".to_owned(), variables);
            if let Some(operation_name) = operation_name {
                object.insert(
                    "operationName".to_owned(),
                    JsonValue::String(operation_name.clone()),
                );
            }
            let encoded = serde_json::to_vec(&JsonValue::Object(object)).map_err(|error| {
                ExecutionError::UploadFailure(format!("failed to encode GraphQL body: {error}"))
            })?;
            Ok(full_prepared_body(
                &encoded,
                Some("application/json".to_owned()),
                &events,
            ))
        }
        RequestBody::FormUrlEncoded(fields) => {
            let encoded = encode_form(fields);
            Ok(full_prepared_body(
                encoded.as_bytes(),
                Some("application/x-www-form-urlencoded".to_owned()),
                &events,
            ))
        }
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            prepare_file_body(relative_path, context, events).await
        }
        RequestBody::Multipart(fields) => prepare_multipart(fields, context, events).await,
    }
}

fn full_prepared_body(
    bytes: &[u8],
    content_type: Option<String>,
    events: &Arc<dyn ExecutionEventSink>,
) -> PreparedBody {
    let bytes = Bytes::copy_from_slice(bytes);
    let length = bytes.len() as u64;
    if length > 0 {
        events.emit(ExecutionEvent::UploadProgress {
            sent_bytes: length,
            total_bytes: Some(length),
        });
    }
    PreparedBody {
        body: full_body(bytes),
        content_type,
        content_length: Some(length),
    }
}

fn full_body(bytes: Bytes) -> ApexRequestBody {
    Full::new(bytes)
        .map_err(infallible_to_box_error)
        .boxed_unsync()
}

fn infallible_to_box_error(error: Infallible) -> BoxError {
    match error {}
}

async fn prepare_file_body(
    relative_path: &str,
    context: &ExecutionContext,
    events: Arc<dyn ExecutionEventSink>,
) -> Result<PreparedBody, ExecutionError> {
    let path = resolve_resource_path(context.resource_root.as_deref(), relative_path).await?;
    let file = File::open(&path)
        .await
        .map_err(|error| ExecutionError::UploadFailure(format!("{}: {error}", path.display())))?;
    let length = file
        .metadata()
        .await
        .map_err(|error| ExecutionError::UploadFailure(format!("{}: {error}", path.display())))?
        .len();
    let stream: ByteStream =
        Box::pin(ReaderStream::new(file).map_err(|error| -> BoxError { Box::new(error) }));
    Ok(PreparedBody {
        body: streaming_body(stream, events, Some(length)),
        content_type: None,
        content_length: Some(length),
    })
}

async fn prepare_multipart(
    fields: &[MultipartField],
    context: &ExecutionContext,
    events: Arc<dyn ExecutionEventSink>,
) -> Result<PreparedBody, ExecutionError> {
    let boundary = format!("apex-{}", context.execution_id);
    let mut streams = Vec::<ByteStream>::new();
    let mut total = 0_u64;

    for field in fields.iter().filter(|field| field.enabled) {
        let mut prefix = String::new();
        write!(&mut prefix, "--{boundary}\r\n").expect("writing to String cannot fail");
        match &field.value {
            MultipartValue::Text(value) => {
                write!(
                    &mut prefix,
                    "Content-Disposition: form-data; name=\"{}\"\r\n",
                    escape_disposition_value(&field.name)?
                )
                .expect("writing to String cannot fail");
                if let Some(content_type) = &field.content_type {
                    write!(&mut prefix, "Content-Type: {content_type}\r\n")
                        .expect("writing to String cannot fail");
                }
                prefix.push_str("\r\n");
                let mut bytes = prefix.into_bytes();
                bytes.extend_from_slice(value.as_bytes());
                bytes.extend_from_slice(b"\r\n");
                total = total.saturating_add(bytes.len() as u64);
                streams.push(single_chunk(Bytes::from(bytes)));
            }
            MultipartValue::File { relative_path } => {
                let path =
                    resolve_resource_path(context.resource_root.as_deref(), relative_path).await?;
                let filename = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        ExecutionError::UploadFailure(format!(
                            "multipart file has no UTF-8 filename: {}",
                            path.display()
                        ))
                    })?;
                write!(
                    &mut prefix,
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                    escape_disposition_value(&field.name)?,
                    escape_disposition_value(filename)?
                )
                .expect("writing to String cannot fail");
                if let Some(content_type) = &field.content_type {
                    write!(&mut prefix, "Content-Type: {content_type}\r\n")
                        .expect("writing to String cannot fail");
                } else {
                    prefix.push_str("Content-Type: application/octet-stream\r\n");
                }
                prefix.push_str("\r\n");
                let file = File::open(&path).await.map_err(|error| {
                    ExecutionError::UploadFailure(format!("{}: {error}", path.display()))
                })?;
                let file_length = file
                    .metadata()
                    .await
                    .map_err(|error| {
                        ExecutionError::UploadFailure(format!("{}: {error}", path.display()))
                    })?
                    .len();
                total = total
                    .saturating_add(prefix.len() as u64)
                    .saturating_add(file_length)
                    .saturating_add(2);
                streams.push(single_chunk(Bytes::from(prefix)));
                streams.push(Box::pin(
                    ReaderStream::new(file).map_err(|error| -> BoxError { Box::new(error) }),
                ));
                streams.push(single_chunk(Bytes::from_static(b"\r\n")));
            }
        }
    }
    let closing = Bytes::from(format!("--{boundary}--\r\n"));
    total = total.saturating_add(closing.len() as u64);
    streams.push(single_chunk(closing));
    let stream: ByteStream = Box::pin(stream::iter(streams).flatten());
    Ok(PreparedBody {
        body: streaming_body(stream, events, Some(total)),
        content_type: Some(format!("multipart/form-data; boundary={boundary}")),
        content_length: Some(total),
    })
}

fn single_chunk(bytes: Bytes) -> ByteStream {
    Box::pin(stream::once(async move { Ok(bytes) }))
}

fn streaming_body(
    stream: ByteStream,
    events: Arc<dyn ExecutionEventSink>,
    total_bytes: Option<u64>,
) -> ApexRequestBody {
    let sent = Arc::new(AtomicU64::new(0));
    let frames = stream.map_ok({
        let sent = Arc::clone(&sent);
        move |bytes| {
            let current = sent.fetch_add(bytes.len() as u64, Ordering::AcqRel) + bytes.len() as u64;
            events.emit(ExecutionEvent::UploadProgress {
                sent_bytes: current,
                total_bytes,
            });
            Frame::data(bytes)
        }
    });
    StreamBody::new(frames).boxed_unsync()
}

fn encode_form(fields: &[FormField]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for field in fields.iter().filter(|field| field.enabled) {
        serializer.append_pair(&field.name, &field.value);
    }
    serializer.finish()
}

fn escape_disposition_value(value: &str) -> Result<String, ExecutionError> {
    if value.contains(['\r', '\n']) {
        return Err(ExecutionError::UploadFailure(
            "multipart field names and filenames may not contain line breaks".to_owned(),
        ));
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

async fn resolve_resource_path(
    root: Option<&Path>,
    relative_path: &str,
) -> Result<PathBuf, ExecutionError> {
    validate_relative_path(relative_path).map_err(ExecutionError::UploadFailure)?;
    let root = root.ok_or_else(|| {
        ExecutionError::UploadFailure(
            "request uses a relative file body but no workspace resource root was provided"
                .to_owned(),
        )
    })?;
    let canonical_root = fs::canonicalize(root)
        .await
        .map_err(|error| ExecutionError::UploadFailure(format!("{}: {error}", root.display())))?;
    let candidate = canonical_root.join(relative_path);
    let canonical_candidate = fs::canonicalize(&candidate).await.map_err(|error| {
        ExecutionError::UploadFailure(format!("{}: {error}", candidate.display()))
    })?;
    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(ExecutionError::UploadFailure(
            "resource path resolves outside the workspace root".to_owned(),
        ));
    }
    Ok(canonical_candidate)
}

fn validate_or_insert_content_length(
    headers: &mut http::HeaderMap,
    expected: u64,
) -> Result<(), ExecutionError> {
    let existing = headers.get_all(CONTENT_LENGTH);
    let values = existing.iter().collect::<Vec<_>>();
    if values.is_empty() {
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&expected.to_string())
                .map_err(|error| ExecutionError::UploadFailure(error.to_string()))?,
        );
        return Ok(());
    }
    for value in values {
        let parsed = value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok());
        if parsed != Some(expected) {
            return Err(ExecutionError::UploadFailure(format!(
                "manual Content-Length does not match the prepared body length of {expected} bytes"
            )));
        }
    }
    Ok(())
}

fn redirect_target<B>(
    current_url: &Url,
    response: &hyper::Response<B>,
) -> Result<Option<Url>, ExecutionError> {
    if !response.status().is_redirection() {
        return Ok(None);
    }
    let Some(location) = response.headers().get(LOCATION) else {
        return Ok(None);
    };
    let location = location.to_str().map_err(|error| {
        ExecutionError::MalformedResponse(format!("redirect Location is not valid text: {error}"))
    })?;
    let next = current_url
        .join(location)
        .map_err(|error| ExecutionError::InvalidUrl(error.to_string()))?;
    validate_url(&next).map_err(|error| ExecutionError::InvalidUrl(error.to_string()))?;
    Ok(Some(next))
}

fn apply_redirect(plan: &mut RequestPlan, status: u16, next_url: Url) {
    let same_origin = same_origin(&plan.url, &next_url);
    plan.url = next_url;
    if !same_origin {
        plan.headers.retain(|header| {
            !header.name.eq_ignore_ascii_case("authorization")
                && !header.name.eq_ignore_ascii_case("proxy-authorization")
                && !header.name.eq_ignore_ascii_case("cookie")
        });
    }
    let rewrite_to_get = status == 303
        || ((status == 301 || status == 302) && matches!(plan.method, HttpMethod::Post));
    if rewrite_to_get && !matches!(plan.method, HttpMethod::Head) {
        plan.method = HttpMethod::Get;
        plan.body = RequestBody::Empty;
        plan.headers.retain(|header| {
            !header.name.eq_ignore_ascii_case("content-length")
                && !header.name.eq_ignore_ascii_case("content-type")
                && !header.name.eq_ignore_ascii_case("transfer-encoding")
        });
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

async fn collect_response(
    response: hyper::Response<hyper::body::Incoming>,
    context: &ExecutionContext,
    idle_timeout: Duration,
    maximum_wire_response_bytes: u64,
    decompress_response: bool,
    events: Arc<dyn ExecutionEventSink>,
    redirect_chain: Vec<RedirectHop>,
) -> Result<(ResponseMetadata, Option<Duration>), ExecutionError> {
    let (parts, mut body) = response.into_parts();
    let headers = header_pairs(&parts.headers);
    let declared_content_length = content_length(&parts.headers);
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let content_encoding = parts
        .headers
        .get(CONTENT_ENCODING)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let encoding = if decompress_response {
        parse_content_encoding(content_encoding.as_deref())?
    } else {
        None
    };
    let mut trailers = Vec::new();
    let mut wire_bytes = 0_u64;

    if let Some(encoding) = encoding {
        let wire_path =
            std::env::temp_dir().join(format!("apex-wire-{}.bin", context.execution_id));
        let mut guard = FileGuard::new(wire_path.clone());
        let mut wire_file = create_new_or_replace_temporary(&wire_path).await?;
        loop {
            context.cancellation.check()?;
            let frame = tokio::time::timeout(idle_timeout, body.frame())
                .await
                .map_err(|_| ExecutionError::RequestTimeout)?;
            let Some(frame) = frame else {
                break;
            };
            let frame =
                frame.map_err(|error| ExecutionError::MalformedResponse(error.to_string()))?;
            match frame.into_data() {
                Ok(data) => {
                    wire_bytes = wire_bytes.saturating_add(data.len() as u64);
                    if wire_bytes > maximum_wire_response_bytes {
                        return Err(ExecutionError::ResponseTooLarge {
                            limit: maximum_wire_response_bytes,
                            observed: wire_bytes,
                        });
                    }
                    wire_file.write_all(&data).await.map_err(|error| {
                        ExecutionError::FilesystemConflict(format!(
                            "failed to spool compressed response: {error}"
                        ))
                    })?;
                    events.emit(ExecutionEvent::DownloadProgress {
                        received_bytes: wire_bytes,
                        total_bytes: declared_content_length,
                    });
                }
                Err(frame) => {
                    if let Ok(frame_trailers) = frame.into_trailers() {
                        trailers.extend(header_pairs(&frame_trailers));
                    }
                }
            }
        }
        wire_file.flush().await.map_err(|error| {
            ExecutionError::FilesystemConflict(format!(
                "failed to flush compressed response: {error}"
            ))
        })?;
        drop(wire_file);

        let decode_started = Instant::now();
        let file = File::open(&wire_path).await.map_err(|error| {
            ExecutionError::DecompressionFailure(format!(
                "failed to reopen compressed response: {error}"
            ))
        })?;
        let mut reader = decoder_for(encoding, file);
        let mut collector = ResponseCollector::new(context).await?;
        let mut decoded_bytes = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            context.cancellation.check()?;
            let read = reader
                .read(&mut buffer)
                .await
                .map_err(|error| ExecutionError::DecompressionFailure(error.to_string()))?;
            if read == 0 {
                break;
            }
            decoded_bytes = decoded_bytes.saturating_add(read as u64);
            if decoded_bytes > context.maximum_response_bytes {
                return Err(ExecutionError::ResponseTooLarge {
                    limit: context.maximum_response_bytes,
                    observed: decoded_bytes,
                });
            }
            collector.push(&buffer[..read]).await?;
        }
        let decompression = decode_started.elapsed();
        let stored_body = collector.finish().await?;
        guard.commit();
        fs::remove_file(&wire_path).await.map_err(|error| {
            ExecutionError::FilesystemConflict(format!(
                "failed to remove compressed response spool {}: {error}",
                wire_path.display()
            ))
        })?;
        Ok((
            ResponseMetadata {
                status: Some(parts.status.as_u16()),
                status_text: parts.status.canonical_reason().map(str::to_owned),
                protocol_version: version_name(parts.version).to_owned(),
                headers,
                trailers,
                received_bytes: decoded_bytes,
                wire_bytes,
                declared_content_length,
                content_type,
                content_encoding,
                decompressed: true,
                redirect_chain,
                stored_body,
            },
            Some(decompression),
        ))
    } else {
        let mut collector = ResponseCollector::new(context).await?;
        loop {
            context.cancellation.check()?;
            let frame = tokio::time::timeout(idle_timeout, body.frame())
                .await
                .map_err(|_| ExecutionError::RequestTimeout)?;
            let Some(frame) = frame else {
                break;
            };
            let frame =
                frame.map_err(|error| ExecutionError::MalformedResponse(error.to_string()))?;
            match frame.into_data() {
                Ok(data) => {
                    wire_bytes = wire_bytes.saturating_add(data.len() as u64);
                    let limit = context
                        .maximum_response_bytes
                        .min(maximum_wire_response_bytes);
                    if wire_bytes > limit {
                        return Err(ExecutionError::ResponseTooLarge {
                            limit,
                            observed: wire_bytes,
                        });
                    }
                    collector.push(&data).await?;
                    events.emit(ExecutionEvent::DownloadProgress {
                        received_bytes: wire_bytes,
                        total_bytes: declared_content_length,
                    });
                }
                Err(frame) => {
                    if let Ok(frame_trailers) = frame.into_trailers() {
                        trailers.extend(header_pairs(&frame_trailers));
                    }
                }
            }
        }
        let stored_body = collector.finish().await?;
        Ok((
            ResponseMetadata {
                status: Some(parts.status.as_u16()),
                status_text: parts.status.canonical_reason().map(str::to_owned),
                protocol_version: version_name(parts.version).to_owned(),
                headers,
                trailers,
                received_bytes: wire_bytes,
                wire_bytes,
                declared_content_length,
                content_type,
                content_encoding,
                decompressed: false,
                redirect_chain,
                stored_body,
            },
            None,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
enum ResponseEncoding {
    Gzip,
    Brotli,
    Zstd,
}

fn parse_content_encoding(value: Option<&str>) -> Result<Option<ResponseEncoding>, ExecutionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "" | "identity" => Ok(None),
        "gzip" | "x-gzip" => Ok(Some(ResponseEncoding::Gzip)),
        "br" => Ok(Some(ResponseEncoding::Brotli)),
        "zstd" => Ok(Some(ResponseEncoding::Zstd)),
        _ if value.contains(',') => Err(ExecutionError::DecompressionFailure(
            "stacked Content-Encoding values are not supported yet".to_owned(),
        )),
        _ => Err(ExecutionError::DecompressionFailure(format!(
            "unsupported Content-Encoding: {value}"
        ))),
    }
}

fn decoder_for(encoding: ResponseEncoding, file: File) -> Pin<Box<dyn AsyncRead + Send>> {
    let reader = BufReader::new(file);
    match encoding {
        ResponseEncoding::Gzip => {
            Box::pin(async_compression::tokio::bufread::GzipDecoder::new(reader))
        }
        ResponseEncoding::Brotli => Box::pin(
            async_compression::tokio::bufread::BrotliDecoder::new(reader),
        ),
        ResponseEncoding::Zstd => {
            Box::pin(async_compression::tokio::bufread::ZstdDecoder::new(reader))
        }
    }
}

fn header_pairs(headers: &http::HeaderMap) -> Vec<(String, String)> {
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

fn content_length(headers: &http::HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

struct ResponseCollector {
    mode: CollectorMode,
    execution_id: apex_domain::ExecutionId,
    threshold: u64,
}

enum CollectorMode {
    Memory(Vec<u8>),
    File {
        file: File,
        guard: FileGuard,
        final_path: PathBuf,
        temporary: bool,
        rename_on_finish: bool,
    },
}

impl ResponseCollector {
    async fn new(context: &ExecutionContext) -> Result<Self, ExecutionError> {
        let mode = if let Some(target) = &context.download_target {
            if target.exists() && !context.overwrite_download {
                return Err(ExecutionError::FilesystemConflict(format!(
                    "download target already exists: {}",
                    target.display()
                )));
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).await.map_err(|error| {
                    ExecutionError::FilesystemConflict(format!("{}: {error}", parent.display()))
                })?;
            }
            let partial = partial_path(target, context.execution_id);
            let file = create_new_file(&partial).await?;
            CollectorMode::File {
                file,
                guard: FileGuard::new(partial),
                final_path: target.clone(),
                temporary: false,
                rename_on_finish: true,
            }
        } else {
            CollectorMode::Memory(Vec::new())
        };
        Ok(Self {
            mode,
            execution_id: context.execution_id,
            threshold: context.memory_response_threshold,
        })
    }

    async fn push(&mut self, bytes: &[u8]) -> Result<(), ExecutionError> {
        let should_spill = matches!(
            &self.mode,
            CollectorMode::Memory(buffer)
                if (buffer.len() as u64).saturating_add(bytes.len() as u64) > self.threshold
        );
        if should_spill {
            let temporary_path =
                std::env::temp_dir().join(format!("apex-response-{}.bin", self.execution_id));
            let file = create_new_or_replace_temporary(&temporary_path).await?;
            let previous =
                match std::mem::replace(&mut self.mode, CollectorMode::Memory(Vec::new())) {
                    CollectorMode::Memory(previous) => previous,
                    CollectorMode::File { .. } => unreachable!(),
                };
            self.mode = CollectorMode::File {
                file,
                guard: FileGuard::new(temporary_path.clone()),
                final_path: temporary_path,
                temporary: true,
                rename_on_finish: false,
            };
            if !previous.is_empty() {
                self.write_file_bytes(&previous).await?;
            }
        }
        match &mut self.mode {
            CollectorMode::Memory(buffer) => buffer.extend_from_slice(bytes),
            CollectorMode::File { file, .. } => file.write_all(bytes).await.map_err(|error| {
                ExecutionError::FilesystemConflict(format!("failed to write response: {error}"))
            })?,
        }
        Ok(())
    }

    async fn write_file_bytes(&mut self, bytes: &[u8]) -> Result<(), ExecutionError> {
        match &mut self.mode {
            CollectorMode::File { file, .. } => file.write_all(bytes).await.map_err(|error| {
                ExecutionError::FilesystemConflict(format!("failed to write response: {error}"))
            }),
            CollectorMode::Memory(_) => Err(ExecutionError::Internal(
                "response collector did not enter file mode".to_owned(),
            )),
        }
    }

    async fn finish(self) -> Result<StoredBody, ExecutionError> {
        match self.mode {
            CollectorMode::Memory(buffer) if buffer.is_empty() => Ok(StoredBody::Empty),
            CollectorMode::Memory(buffer) => Ok(StoredBody::InMemory(buffer)),
            CollectorMode::File {
                mut file,
                mut guard,
                final_path,
                temporary,
                rename_on_finish,
            } => {
                file.flush().await.map_err(|error| {
                    ExecutionError::FilesystemConflict(format!(
                        "failed to flush response file: {error}"
                    ))
                })?;
                file.sync_data().await.map_err(|error| {
                    ExecutionError::FilesystemConflict(format!(
                        "failed to synchronize response file: {error}"
                    ))
                })?;
                drop(file);
                if rename_on_finish {
                    fs::rename(guard.path(), &final_path)
                        .await
                        .map_err(|error| {
                            ExecutionError::FilesystemConflict(format!(
                                "failed to commit download to {}: {error}",
                                final_path.display()
                            ))
                        })?;
                }
                guard.commit();
                Ok(StoredBody::File {
                    path: final_path,
                    temporary,
                })
            }
        }
    }
}

struct FileGuard {
    path: PathBuf,
    committed: bool,
}

impl FileGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn create_new_file(path: &Path) -> Result<File, ExecutionError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| ExecutionError::FilesystemConflict(format!("{}: {error}", path.display())))
}

async fn create_new_or_replace_temporary(path: &Path) -> Result<File, ExecutionError> {
    match fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ExecutionError::FilesystemConflict(format!(
                "{}: {error}",
                path.display()
            )));
        }
    }
    create_new_file(path).await
}

fn partial_path(target: &Path, execution_id: apex_domain::ExecutionId) -> PathBuf {
    let filename = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    target.with_file_name(format!(".{filename}.apex-part-{execution_id}"))
}

async fn wait_for_cancellation(token: CancellationToken) {
    loop {
        if token.is_cancelled() {
            return;
        }
        tokio::time::sleep(CANCELLATION_POLL_INTERVAL).await;
    }
}

fn map_client_error(error: ClientError) -> ExecutionError {
    if let Some(io_error) = find_io_error(&error) {
        return match io_error.kind() {
            io::ErrorKind::TimedOut => ExecutionError::ConnectionTimeout,
            io::ErrorKind::ConnectionRefused => {
                ExecutionError::ConnectionRefused(io_error.to_string())
            }
            io::ErrorKind::NotFound
            | io::ErrorKind::AddrNotAvailable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkUnreachable => ExecutionError::DnsFailure(io_error.to_string()),
            _ if error.is_connect() => ExecutionError::ConnectionRefused(io_error.to_string()),
            _ => ExecutionError::MalformedResponse(io_error.to_string()),
        };
    }
    let detail = error_source_chain(&error);
    let lowercase = detail.to_ascii_lowercase();
    if lowercase.contains("certificate") || lowercase.contains("unknownissuer") {
        ExecutionError::CertificateFailure(detail)
    } else if lowercase.contains("tls") || lowercase.contains("handshake") {
        ExecutionError::TlsFailure(detail)
    } else if lowercase.contains("dns") || lowercase.contains("name or service not known") {
        ExecutionError::DnsFailure(detail)
    } else if error.is_connect() {
        ExecutionError::ConnectionRefused(detail)
    } else {
        ExecutionError::MalformedResponse(detail)
    }
}

fn find_io_error<'a>(mut error: &'a (dyn StdError + 'static)) -> Option<&'a io::Error> {
    loop {
        if let Some(io_error) = error.downcast_ref::<io::Error>() {
            return Some(io_error);
        }
        error = error.source()?;
    }
}

fn error_source_chain(error: &(dyn StdError + 'static)) -> String {
    let mut messages = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        messages.push(error.to_string());
        source = error.source();
    }
    messages.join(": ")
}

fn validation(field: impl Into<String>, message: impl Into<String>) -> ValidationError {
    ValidationError {
        field: field.into(),
        message: message.into(),
    }
}

fn validation_to_execution_error(error: &ValidationError) -> ExecutionError {
    if error.field == "url" {
        ExecutionError::InvalidUrl(error.message.clone())
    } else if error.field.starts_with("body") {
        ExecutionError::UploadFailure(error.message.clone())
    } else {
        ExecutionError::Internal(error.to_string())
    }
}

fn redacted_failure_summary(summary: &str, category: ErrorCategory) -> String {
    format!("{summary}; category={category:?}")
}

fn version_name(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/unknown",
    }
}

fn duration_millis_saturated(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{HttpRequest, RequestSettings, StableId, ValueSensitivity};
    use apex_test_support::{RecordingEventSink, http_fixture::HttpFixtureServer};

    fn request(url: &str) -> HttpRequest {
        HttpRequest {
            id: StableId::parse("http-test").expect("valid id"),
            name: "HTTP test".to_owned(),
            method: HttpMethod::Get,
            url: url.to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication: apex_domain::Authentication::None,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: String::new(),
        }
    }

    #[test]
    fn rejects_credentials_in_urls() {
        let adapter = HttpAdapter::new();
        let error = adapter
            .validate(&ProtocolRequest::Http(request(
                "https://user:password@example.test/",
            )))
            .expect_err("userinfo must be rejected");
        assert_eq!(error.field, "url");
    }

    #[test]
    fn query_fields_preserve_order_and_duplicates() {
        let mut request = request("http://example.test/path?existing=yes");
        request.query = vec![
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
                sensitivity: ValueSensitivity::Public,
            },
        ];
        let url = build_initial_url(&request).expect("URL builds");
        assert_eq!(url.query(), Some("existing=yes&tag=one&tag=two"));
    }

    #[test]
    fn cross_origin_redirect_strips_credentials() {
        let mut plan = RequestPlan {
            method: HttpMethod::Get,
            url: Url::parse("https://one.test/path").expect("valid URL"),
            headers: vec![
                HeaderEntry::new("Authorization", "Bearer secret").expect("valid header"),
                HeaderEntry::new("X-Trace", "safe").expect("valid header"),
            ],
            body: RequestBody::Empty,
            decompress_response: true,
        };
        apply_redirect(
            &mut plan,
            302,
            Url::parse("https://two.test/next").expect("valid URL"),
        );
        assert!(
            plan.headers
                .iter()
                .all(|header| !header.name.eq_ignore_ascii_case("authorization"))
        );
        assert_eq!(plan.headers.len(), 1);
    }

    #[test]
    fn post_303_redirect_becomes_get_without_body_headers() {
        let mut plan = RequestPlan {
            method: HttpMethod::Post,
            url: Url::parse("https://one.test/path").expect("valid URL"),
            headers: vec![
                HeaderEntry::new("Content-Type", "application/json").expect("valid header"),
            ],
            body: RequestBody::Json("{}".to_owned()),
            decompress_response: true,
        };
        apply_redirect(
            &mut plan,
            303,
            Url::parse("https://one.test/next").expect("valid URL"),
        );
        assert_eq!(plan.method, HttpMethod::Get);
        assert_eq!(plan.body, RequestBody::Empty);
        assert!(plan.headers.is_empty());
    }

    async fn execute_request(
        request: HttpRequest,
        context: ExecutionContext,
    ) -> Result<(ExecutionResult, Vec<ExecutionEvent>), ExecutionError> {
        execute_with_adapter(&HttpAdapter::new(), request, context).await
    }

    async fn execute_with_adapter(
        adapter: &HttpAdapter,
        request: HttpRequest,
        context: ExecutionContext,
    ) -> Result<(ExecutionResult, Vec<ExecutionEvent>), ExecutionError> {
        let sink = Arc::new(RecordingEventSink::default());
        let event_sink: Arc<dyn ExecutionEventSink> = sink.clone();
        let result = adapter
            .execute(
                ResolvedRequest {
                    redacted_summary: format!("{} test-request", request.method),
                    request: ProtocolRequest::Http(request),
                },
                context,
                event_sink,
            )
            .await?;
        Ok((result, sink.events()))
    }

    async fn stored_body_bytes(body: &StoredBody) -> Vec<u8> {
        match body {
            StoredBody::Empty => Vec::new(),
            StoredBody::InMemory(bytes) => bytes.clone(),
            StoredBody::File { path, .. } | StoredBody::StreamLog(path) => {
                fs::read(path).await.expect("stored body can be read")
            }
        }
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "apex-http-test-{label}-{}",
            apex_domain::ExecutionId::new()
        ));
        std::fs::create_dir_all(&path).expect("temporary test directory is created");
        path
    }

    #[tokio::test]
    async fn executes_post_and_preserves_duplicate_request_headers() {
        let server = HttpFixtureServer::start().await;
        let mut request = request(&server.url("/echo"));
        request.method = HttpMethod::Post;
        request.body = RequestBody::Text {
            content_type: Some("text/plain".to_owned()),
            text: "payload".to_owned(),
        };
        request.headers = vec![
            HeaderEntry::new("X-Apex-Duplicate", "one").expect("valid header"),
            HeaderEntry::new("X-Apex-Duplicate", "two").expect("valid header"),
        ];

        let (result, events) = execute_request(
            request,
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("request succeeds");
        assert_eq!(result.response.status, Some(200));
        assert_eq!(
            stored_body_bytes(&result.response.stored_body).await,
            b"payload"
        );
        let captured = server.captured();
        let duplicate_values = captured[0]
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-apex-duplicate"))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(duplicate_values, ["one", "two"]);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ExecutionEvent::Completed))
        );
    }

    #[tokio::test]
    async fn preserves_duplicate_response_headers() {
        let server = HttpFixtureServer::start().await;
        let (result, _) = execute_request(
            request(&server.url("/duplicate")),
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("request succeeds");
        let values = result
            .response
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-apex-value"))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(values, ["one", "two"]);
    }

    #[tokio::test]
    async fn follows_relative_redirect_and_records_chain() {
        let server = HttpFixtureServer::start().await;
        let (result, _) = execute_request(
            request(&server.url("/redirect")),
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("redirect succeeds");
        assert_eq!(result.response.status, Some(200));
        assert_eq!(result.response.redirect_chain.len(), 1);
        assert_eq!(result.response.redirect_chain[0].status, 302);
        assert_eq!(
            stored_body_bytes(&result.response.stored_body).await,
            b"redirected"
        );
    }

    #[tokio::test]
    async fn captures_response_trailers() {
        let server = HttpFixtureServer::start().await;
        let (result, _) = execute_request(
            request(&server.url("/trailers")),
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("trailer response succeeds");
        assert_eq!(
            stored_body_bytes(&result.response.stored_body).await,
            b"hello trailers"
        );
        assert!(result.response.trailers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("x-apex-checksum") && value == "verified"
        }));
    }

    #[tokio::test]
    async fn spills_large_responses_to_a_temporary_file() {
        let server = HttpFixtureServer::start().await;
        let request = request(&server.url("/large"));
        let mut context = ExecutionContext::new(Duration::from_secs(5), 3 * 1024 * 1024);
        context.memory_response_threshold = 1024;
        let (result, _) = execute_request(request, context)
            .await
            .expect("large response succeeds");
        let path = match &result.response.stored_body {
            StoredBody::File {
                path,
                temporary: true,
            } => path.clone(),
            other => panic!("expected temporary file, got {other:?}"),
        };
        assert_eq!(
            fs::metadata(&path).await.expect("file metadata").len(),
            2 * 1024 * 1024
        );
        fs::remove_file(path)
            .await
            .expect("temporary response removed");
    }

    #[tokio::test]
    async fn rejects_responses_above_the_hard_limit() {
        let server = HttpFixtureServer::start().await;
        let mut request = request(&server.url("/large"));
        request.settings.maximum_response_bytes = 1024;
        let error = execute_request(request, ExecutionContext::new(Duration::from_secs(5), 1024))
            .await
            .expect_err("oversized response must fail");
        assert!(matches!(
            error,
            ExecutionError::ResponseTooLarge {
                limit: 1024,
                observed
            } if observed > 1024
        ));
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_request() {
        let server = HttpFixtureServer::start().await;
        let adapter = Arc::new(HttpAdapter::new());
        let request = request(&server.url("/slow"));
        let context = ExecutionContext::new(Duration::from_secs(5), 1024 * 1024);
        let cancellation = context.cancellation.clone();
        let adapter_task = Arc::clone(&adapter);
        let task = tokio::spawn(async move {
            adapter_task
                .execute(
                    ResolvedRequest {
                        request: ProtocolRequest::Http(request),
                        redacted_summary: "GET slow fixture".to_owned(),
                    },
                    context,
                    Arc::new(RecordingEventSink::default()),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
        let error = task
            .await
            .expect("execution task joins")
            .expect_err("request must be cancelled");
        assert_eq!(error, ExecutionError::Cancelled);
    }

    #[tokio::test]
    async fn total_timeout_interrupts_an_in_flight_request() {
        let server = HttpFixtureServer::start().await;
        let mut request = request(&server.url("/slow"));
        request.settings.timeout = Duration::from_millis(50);
        let error = execute_request(
            request,
            ExecutionContext::new(Duration::from_millis(50), 1024 * 1024),
        )
        .await
        .expect_err("request must time out");
        assert_eq!(error, ExecutionError::RequestTimeout);
    }

    #[tokio::test]
    async fn streams_file_uploads_from_the_workspace_root() {
        let server = HttpFixtureServer::start().await;
        let root = temporary_directory("stream-upload");
        fs::write(root.join("payload.bin"), b"streamed-file-body")
            .await
            .expect("fixture file written");
        let mut request = request(&server.url("/upload"));
        request.method = HttpMethod::Put;
        request.body = RequestBody::StreamFile {
            relative_path: "payload.bin".to_owned(),
        };
        let mut context = ExecutionContext::new(Duration::from_secs(5), 1024 * 1024);
        context.resource_root = Some(root.clone());
        let (result, events) = execute_request(request, context)
            .await
            .expect("stream upload succeeds");
        assert_eq!(
            stored_body_bytes(&result.response.stored_body).await,
            b"streamed-file-body"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            ExecutionEvent::UploadProgress {
                sent_bytes: 18,
                total_bytes: Some(18)
            }
        )));
        fs::remove_dir_all(root)
            .await
            .expect("fixture directory removed");
    }

    #[tokio::test]
    async fn streams_multipart_files_without_flattening_them_into_request_memory() {
        let server = HttpFixtureServer::start().await;
        let root = temporary_directory("multipart-upload");
        fs::write(root.join("part.txt"), b"multipart-file")
            .await
            .expect("fixture file written");
        let mut request = request(&server.url("/upload"));
        request.method = HttpMethod::Post;
        request.body = RequestBody::Multipart(vec![
            MultipartField {
                name: "description".to_owned(),
                value: MultipartValue::Text("hello".to_owned()),
                content_type: Some("text/plain".to_owned()),
                enabled: true,
                sensitivity: ValueSensitivity::Public,
            },
            MultipartField {
                name: "attachment".to_owned(),
                value: MultipartValue::File {
                    relative_path: "part.txt".to_owned(),
                },
                content_type: Some("text/plain".to_owned()),
                enabled: true,
                sensitivity: ValueSensitivity::Public,
            },
        ]);
        let mut context = ExecutionContext::new(Duration::from_secs(5), 1024 * 1024);
        context.resource_root = Some(root.clone());
        let (result, _) = execute_request(request, context)
            .await
            .expect("multipart upload succeeds");
        let body = stored_body_bytes(&result.response.stored_body).await;
        let body = String::from_utf8(body).expect("multipart fixture is UTF-8");
        assert!(body.contains("name=\"description\""));
        assert!(body.contains("hello"));
        assert!(body.contains("filename=\"part.txt\""));
        assert!(body.contains("multipart-file"));
        fs::remove_dir_all(root)
            .await
            .expect("fixture directory removed");
    }

    #[tokio::test]
    async fn commits_explicit_downloads_only_after_success() {
        let server = HttpFixtureServer::start().await;
        let root = temporary_directory("download");
        let target = root.join("response.bin");
        let mut context = ExecutionContext::new(Duration::from_secs(5), 1024 * 1024);
        context.download_target = Some(target.clone());
        let (result, _) = execute_request(request(&server.url("/final")), context)
            .await
            .expect("download succeeds");
        assert_eq!(
            result.response.stored_body,
            StoredBody::File {
                path: target.clone(),
                temporary: false,
            }
        );
        assert_eq!(
            fs::read(&target).await.expect("download readable"),
            b"redirected"
        );
        let entries = std::fs::read_dir(&root)
            .expect("download directory readable")
            .map(|entry| entry.expect("entry readable").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [target.file_name().expect("filename").to_owned()]);
        fs::remove_dir_all(root)
            .await
            .expect("fixture directory removed");
    }

    #[tokio::test]
    async fn applies_basic_authentication_inside_the_shared_adapter() {
        let server = HttpFixtureServer::start().await;
        let mut request = request(&server.url("/echo"));
        request.authentication = apex_domain::Authentication::Basic {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        };
        execute_request(
            request,
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("authenticated request succeeds");
        let captured = server.captured();
        assert!(captured[0].headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization") && value == "Basic dXNlcjpwYXNz"
        }));
    }

    #[tokio::test]
    async fn applies_api_key_authentication_to_the_query() {
        let server = HttpFixtureServer::start().await;
        let mut request = request(&server.url("/echo"));
        request.authentication = apex_domain::Authentication::ApiKey {
            name: "api_key".to_owned(),
            value: "secret".to_owned(),
            placement: apex_domain::ApiKeyPlacement::Query,
        };
        execute_request(
            request,
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("authenticated request succeeds");
        assert_eq!(server.captured()[0].path_and_query, "/echo?api_key=secret");
    }

    #[tokio::test]
    async fn cookie_jar_replays_matching_cookies() {
        let server = HttpFixtureServer::start().await;
        let adapter = HttpAdapter::new();
        execute_with_adapter(
            &adapter,
            request(&server.url("/cookie/set")),
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("cookie response succeeds");
        let (result, _) = execute_with_adapter(
            &adapter,
            request(&server.url("/cookie/echo")),
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("cookie replay succeeds");
        assert_eq!(
            stored_body_bytes(&result.response.stored_body).await,
            b"session=apex-cookie"
        );
    }

    #[tokio::test]
    async fn redirect_response_cookies_are_available_to_the_next_hop() {
        let server = HttpFixtureServer::start().await;
        let adapter = HttpAdapter::new();
        let (result, _) = execute_with_adapter(
            &adapter,
            request(&server.url("/cookie/redirect")),
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("cookie redirect succeeds");
        assert_eq!(
            stored_body_bytes(&result.response.stored_body).await,
            b"redirect_cookie=stored"
        );
    }

    #[tokio::test]
    async fn disabled_cookie_jar_does_not_replay_cookies() {
        let server = HttpFixtureServer::start().await;
        let adapter = HttpAdapter::new();
        execute_with_adapter(
            &adapter,
            request(&server.url("/cookie/set")),
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("cookie response succeeds");
        let mut next = request(&server.url("/cookie/echo"));
        next.settings.cookie_jar = false;
        let (result, _) = execute_with_adapter(
            &adapter,
            next,
            ExecutionContext::new(Duration::from_secs(5), 1024 * 1024),
        )
        .await
        .expect("request succeeds");
        assert!(
            stored_body_bytes(&result.response.stored_body)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn decompresses_gzip_brotli_and_zstd_without_buffering_wire_bytes_in_memory() {
        let server = HttpFixtureServer::start().await;
        for (path, expected) in [
            ("/compressed/gzip", b'g'),
            ("/compressed/br", b'b'),
            ("/compressed/zstd", b'z'),
        ] {
            let (result, _) = execute_request(
                request(&server.url(path)),
                ExecutionContext::new(Duration::from_secs(5), 256 * 1024),
            )
            .await
            .expect("compressed response succeeds");
            let bytes = stored_body_bytes(&result.response.stored_body).await;
            assert_eq!(bytes.len(), 128 * 1024);
            assert!(bytes.iter().all(|byte| *byte == expected));
            assert!(result.response.decompressed);
            assert!(result.response.wire_bytes < result.response.received_bytes);
        }
    }

    #[tokio::test]
    async fn decompression_limit_blocks_compression_bombs() {
        let server = HttpFixtureServer::start().await;
        let error = execute_request(
            request(&server.url("/compressed/gzip")),
            ExecutionContext::new(Duration::from_secs(5), 1024),
        )
        .await
        .expect_err("decoded response exceeds limit");
        assert!(matches!(
            error,
            ExecutionError::ResponseTooLarge {
                limit: 1024,
                observed: _
            }
        ));
    }

    #[tokio::test]
    async fn decompression_can_be_disabled_explicitly() {
        let server = HttpFixtureServer::start().await;
        let mut request = request(&server.url("/compressed/gzip"));
        request.settings.decompress_response = false;
        let (result, _) = execute_request(
            request,
            ExecutionContext::new(Duration::from_secs(5), 256 * 1024),
        )
        .await
        .expect("raw compressed response succeeds");
        assert!(!result.response.decompressed);
        assert_eq!(result.response.received_bytes, result.response.wire_bytes);
        assert_eq!(result.response.content_encoding.as_deref(), Some("gzip"));
    }
}
