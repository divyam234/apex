#![forbid(unsafe_code)]

use apex_domain::{
    ExecutionError, ExecutionEvent, StableId, ValueSensitivity, VariableDefinition, VariableValue,
    branding,
};
use apex_history::{HistoryDatabase, HistoryPolicy, HistoryRecord};
use apex_http::HttpAdapter;
use apex_import::parse_curl;
use apex_runner::{
    ExecutionContext, ExecutionEventSink, ProtocolAdapter, ProtocolRequest, ResolvedRequest,
    StoredBody,
};
use apex_secrets::{EnvironmentSecretStore, SecretStoreChain};
use apex_variables::{
    ResolverOptions, SystemDynamicVariables, VariableContext, VariableResolver, VariableScope,
    WorkspaceVariableSelection, load_workspace_variables,
    resolve_http_request as resolve_shared_http_request,
};
use apex_workspace::{
    StoredVariableSource, VariableSetDocument, WorkspaceManifest, WorkspaceRepository,
    format_request,
};
use serde_json::json;
use std::env;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

const EXIT_SUCCESS: u8 = 0;
const EXIT_USAGE: u8 = 2;
const EXIT_VALIDATION: u8 = 3;
const EXIT_IO: u8 = 4;
const EXIT_IMPORT: u8 = 5;
const EXIT_NETWORK: u8 = 6;
const EXIT_CANCELLED: u8 = 130;
const MAXIMUM_STDOUT_BODY_BYTES: usize = 1024 * 1024;

#[tokio::main]
async fn main() -> ExitCode {
    match run(env::args().skip(1).collect()).await {
        Ok(()) => ExitCode::from(EXIT_SUCCESS),
        Err(error) => {
            eprintln!("error: {}", error.message);
            ExitCode::from(error.exit_code)
        }
    }
}

async fn run(arguments: Vec<String>) -> Result<(), CliError> {
    let Some(command) = arguments.first().map(String::as_str) else {
        print_help();
        return Err(CliError::usage("missing command"));
    };
    match command {
        "doctor" => doctor(),
        "init" => init_workspace(&arguments[1..]),
        "validate" => validate_workspace(&arguments[1..]),
        "resolve" => resolve_template(&arguments[1..]),
        "import-curl" => import_curl(&arguments[1..]),
        "send" => send_request(&arguments[1..]).await,
        "history" => history_command(&arguments[1..]),
        "env" => environment_command(&arguments[1..]),
        "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        "--version" | "-V" => {
            println!("{} {}", branding::PRODUCT_NAME, env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => Err(CliError::usage(format!("unknown command: {other}"))),
    }
}

fn doctor() -> Result<(), CliError> {
    println!("product: {}", branding::PRODUCT_NAME);
    println!("version: {}", env!("CARGO_PKG_VERSION"));
    println!(
        "workspace schema: {}",
        apex_workspace::CURRENT_SCHEMA_VERSION
    );
    println!("rustc: compile-time toolchain is Rust 1.96 compatible");
    println!("core status: domain/workspace/variables/secrets/import contracts available");
    println!("network engine: Hyper HTTP/1.1 + HTTP/2 with Rustls native-root TLS");
    println!("authentication: Basic, Bearer, and API key through resolved secret references");
    println!("cookies: RFC-aware session jar with per-request opt-out");
    println!("decompression: bounded gzip, Brotli, and zstd decoding");
    println!("history: local SQLite metadata database with redacted query values");
    println!("streaming: request files, multipart files, response spill-to-disk, downloads");
    println!("gpui shell: native workspace member using gpui 0.2.2 and gpui-component 0.5.1");
    Ok(())
}

fn init_workspace(arguments: &[String]) -> Result<(), CliError> {
    let path = arguments
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage("usage: apex init <directory> [name]"))?;
    let name = arguments.get(1).cloned().unwrap_or_else(|| {
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Apex workspace")
            .to_owned()
    });
    let slug = stable_slug(&name);
    let manifest = WorkspaceManifest::new(
        StableId::parse(slug).map_err(|error| CliError::validation(error.to_string()))?,
        name,
    );
    WorkspaceRepository::new(&path)
        .and_then(|repository| repository.initialize(&manifest))
        .map_err(|error| CliError::io(error.to_string()))?;
    println!(
        "initialized {} workspace at {}",
        branding::PRODUCT_NAME,
        path.display()
    );
    Ok(())
}

fn validate_workspace(arguments: &[String]) -> Result<(), CliError> {
    let path = arguments
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage("usage: apex validate <workspace-directory>"))?;
    let repository =
        WorkspaceRepository::new(&path).map_err(|error| CliError::io(error.to_string()))?;
    let manifest = repository
        .load_manifest()
        .map_err(|error| CliError::validation(error.to_string()))?;
    let conflicts = repository
        .scan_conflicts()
        .map_err(|error| CliError::validation(error.to_string()))?;
    if !conflicts.is_empty() {
        return Err(CliError::validation(format!(
            "{} file(s) contain merge conflict markers",
            conflicts.len()
        )));
    }
    let workspace_variables = repository
        .load_workspace_variables()
        .map_err(|error| CliError::validation(error.to_string()))?;
    let environments = repository
        .list_environments()
        .map_err(|error| CliError::validation(error.to_string()))?;
    if let Some(default_environment) = &manifest.value.default_environment
        && !environments
            .iter()
            .any(|environment| environment.id.as_str() == default_environment)
    {
        return Err(CliError::validation(format!(
            "default environment '{default_environment}' does not exist"
        )));
    }
    println!(
        "valid workspace: {} ({}, schema {}), {} environment(s), {} workspace variable(s)",
        manifest.value.name,
        manifest.value.id,
        manifest.value.schema_version,
        environments.len(),
        workspace_variables
            .as_ref()
            .map_or(0, |document| document.value.variables.len())
    );
    Ok(())
}

fn resolve_template(arguments: &[String]) -> Result<(), CliError> {
    let template = arguments
        .first()
        .ok_or_else(|| {
            CliError::usage(
                "usage: apex resolve <template> [--workspace path] [--environment id] [--set name=value]...",
            )
        })?;
    let mut overrides = VariableContext::default();
    let mut workspace = None;
    let mut environment = None;
    let mut include_local_environment = true;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--set" => {
                let assignment = arguments
                    .get(index + 1)
                    .ok_or_else(|| CliError::usage("--set requires name=value"))?;
                insert_assignment(
                    &mut overrides,
                    assignment,
                    ValueSensitivity::Public,
                    "CLI override",
                )?;
                index += 2;
            }
            "--secret-env" => {
                let mapping = arguments.get(index + 1).ok_or_else(|| {
                    CliError::usage("--secret-env requires name=ENVIRONMENT_NAME")
                })?;
                let (name, environment_name) = mapping.split_once('=').ok_or_else(|| {
                    CliError::usage("--secret-env requires name=ENVIRONMENT_NAME")
                })?;
                let value = env::var(environment_name).map_err(|_| {
                    CliError::validation(format!(
                        "environment variable {environment_name} is not available"
                    ))
                })?;
                insert_variable(
                    &mut overrides,
                    name,
                    value,
                    ValueSensitivity::Secret,
                    format!("process environment {environment_name}"),
                )?;
                index += 2;
            }
            "--workspace" => {
                workspace =
                    Some(PathBuf::from(arguments.get(index + 1).ok_or_else(
                        || CliError::usage("--workspace requires a path"),
                    )?));
                index += 2;
            }
            "--environment" | "-e" => {
                environment = Some(
                    arguments
                        .get(index + 1)
                        .ok_or_else(|| CliError::usage("--environment requires an id"))?
                        .clone(),
                );
                index += 2;
            }
            "--no-local-environment" => {
                include_local_environment = false;
                index += 1;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unexpected resolve argument: {other}"
                )));
            }
        }
    }
    let mut context = if let Some(workspace) = workspace {
        let repository =
            WorkspaceRepository::new(workspace).map_err(|error| CliError::io(error.to_string()))?;
        let mut stores = SecretStoreChain::default();
        stores.push(Arc::new(EnvironmentSecretStore));
        load_workspace_variables(
            &repository,
            &WorkspaceVariableSelection {
                environment,
                include_local_override: include_local_environment,
            },
            Some(&stores),
        )
        .map_err(|error| CliError::validation(error.to_string()))?
        .context
    } else {
        VariableContext::default()
    };
    context.merge(overrides);
    let resolved = VariableResolver::new(
        &context,
        &SystemDynamicVariables,
        ResolverOptions::default(),
    )
    .resolve(template)
    .map_err(|error| CliError::validation(error.to_string()))?;
    println!("{}", resolved.value);
    for trace in resolved.traces {
        let source = trace
            .selected_scope
            .map(VariableScope::label)
            .unwrap_or("unresolved");
        eprintln!("trace: {} <- {source}", trace.expression);
    }
    Ok(())
}

fn import_curl(arguments: &[String]) -> Result<(), CliError> {
    if arguments.is_empty() {
        return Err(CliError::usage("usage: apex import-curl <curl command>"));
    }
    let command = arguments.join(" ");
    let preview = parse_curl(&command).map_err(|error| CliError::import(error.to_string()))?;
    let mut report = String::new();
    writeln!(&mut report, "source_format = {}", preview.source_format)
        .map_err(|error| CliError::import(error.to_string()))?;
    writeln!(&mut report, "requests = {}", preview.requests.len())
        .map_err(|error| CliError::import(error.to_string()))?;
    for diagnostic in &preview.diagnostics {
        writeln!(
            &mut report,
            "diagnostic = {:?}: {} ({})",
            diagnostic.severity, diagnostic.message, diagnostic.code
        )
        .map_err(|error| CliError::import(error.to_string()))?;
    }
    print!("{report}");
    for request in &preview.requests {
        println!("--- request preview ---");
        print!("{}", format_request(request));
    }
    Ok(())
}

#[derive(Debug)]
struct SendOptions {
    request_path: PathBuf,
    download_target: Option<PathBuf>,
    overwrite: bool,
    json: bool,
    quiet: bool,
    maximum_response_bytes: Option<u64>,
    memory_threshold: Option<u64>,
    no_history: bool,
    history_db: Option<PathBuf>,
    environment: Option<String>,
    include_local_environment: bool,
    variables: VariableContext,
}

async fn send_request(arguments: &[String]) -> Result<(), CliError> {
    let options = parse_send_options(arguments)?;
    let request_path = std::fs::canonicalize(&options.request_path)
        .map_err(|error| CliError::io(format!("{}: {error}", options.request_path.display())))?;
    let workspace_root = find_workspace_root(&request_path).unwrap_or_else(|| {
        request_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_owned()
    });
    let repository = WorkspaceRepository::new(&workspace_root)
        .map_err(|error| CliError::io(error.to_string()))?;
    let loaded = repository
        .load_request(&request_path)
        .map_err(|error| CliError::validation(error.to_string()))?;
    let mut variable_context = if workspace_root.join(branding::WORKSPACE_FILE).is_file() {
        let mut secret_stores = SecretStoreChain::default();
        secret_stores.push(Arc::new(EnvironmentSecretStore));
        load_workspace_variables(
            &repository,
            &WorkspaceVariableSelection {
                environment: options.environment.clone(),
                include_local_override: options.include_local_environment,
            },
            Some(&secret_stores),
        )
        .map_err(|error| CliError::validation(error.to_string()))?
        .context
    } else {
        VariableContext::default()
    };
    variable_context.merge(options.variables.clone());
    let request = resolve_shared_http_request(
        &loaded.value.request,
        &variable_context,
        &SystemDynamicVariables,
        ResolverOptions::default(),
    )
    .map_err(|error| CliError::validation(error.to_string()))?
    .request;

    let mut execution = ExecutionContext::new(
        request.settings.timeout,
        options
            .maximum_response_bytes
            .unwrap_or(request.settings.maximum_response_bytes),
    );
    execution.resource_root = Some(workspace_root.clone());
    execution.download_target = options.download_target.clone();
    execution.overwrite_download = options.overwrite;
    if let Some(threshold) = options.memory_threshold {
        execution.memory_response_threshold = threshold;
    }

    let cancellation = execution.cancellation.clone();
    let cancellation_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancellation.cancel();
        }
    });
    let sink: Arc<dyn ExecutionEventSink> = Arc::new(CliEventSink {
        quiet: options.quiet || options.json,
    });
    let adapter = HttpAdapter::new();
    let summary = format!("{} {}", request.method, request.name);
    let history_request = request.clone();
    let execution_id = execution.execution_id.to_string();
    let started = Instant::now();
    let result = adapter
        .execute(
            ResolvedRequest {
                request: ProtocolRequest::Http(request),
                redacted_summary: summary,
            },
            execution,
            sink,
        )
        .await;
    cancellation_task.abort();
    let elapsed = started.elapsed();
    let history_path = options
        .history_db
        .clone()
        .unwrap_or_else(|| workspace_root.join(".apex").join("history.sqlite"));
    let policy = HistoryPolicy {
        enabled: !options.no_history,
        ..HistoryPolicy::default()
    };

    match result {
        Ok(mut result) => {
            let record = HistoryRecord::success(
                execution_id,
                &history_request,
                elapsed,
                result.response.status,
                result.response.received_bytes,
                &policy,
            );
            if let Err(error) = record_history(&history_path, &record, &policy) {
                result
                    .diagnostics
                    .push(format!("history was not recorded: {error}"));
            }
            if options.json {
                print_json_result(&result)?;
            } else if !options.quiet {
                print_human_result(&result)?;
                for diagnostic in &result.diagnostics {
                    eprintln!("diagnostic: {diagnostic}");
                }
            }
            Ok(())
        }
        Err(error) => {
            let record = HistoryRecord::failure(
                execution_id,
                &history_request,
                elapsed,
                error.category(),
                &policy,
            );
            if let Err(history_error) = record_history(&history_path, &record, &policy)
                && !options.quiet
            {
                eprintln!("warning: history was not recorded: {history_error}");
            }
            Err(CliError::execution(error))
        }
    }
}

fn parse_send_options(arguments: &[String]) -> Result<SendOptions, CliError> {
    let request_path = arguments
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage(send_usage()))?;
    let mut options = SendOptions {
        request_path,
        download_target: None,
        overwrite: false,
        json: false,
        quiet: false,
        maximum_response_bytes: None,
        memory_threshold: None,
        no_history: false,
        history_db: None,
        environment: None,
        include_local_environment: true,
        variables: VariableContext::default(),
    };
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--download" => {
                options.download_target = Some(PathBuf::from(next_option_value(
                    arguments,
                    &mut index,
                    "--download",
                )?));
            }
            "--overwrite" => options.overwrite = true,
            "--json" => options.json = true,
            "--quiet" | "-q" => options.quiet = true,
            "--max-response-bytes" => {
                options.maximum_response_bytes = Some(parse_positive_u64(
                    next_option_value(arguments, &mut index, "--max-response-bytes")?,
                    "--max-response-bytes",
                )?);
            }
            "--memory-threshold" => {
                options.memory_threshold = Some(parse_positive_u64(
                    next_option_value(arguments, &mut index, "--memory-threshold")?,
                    "--memory-threshold",
                )?);
            }
            "--no-history" => options.no_history = true,
            "--history-db" => {
                options.history_db = Some(PathBuf::from(next_option_value(
                    arguments,
                    &mut index,
                    "--history-db",
                )?));
            }
            "--environment" | "-e" => {
                options.environment =
                    Some(next_option_value(arguments, &mut index, "--environment")?.to_owned());
            }
            "--no-local-environment" => options.include_local_environment = false,
            "--set" => {
                let assignment = next_option_value(arguments, &mut index, "--set")?;
                insert_assignment(
                    &mut options.variables,
                    assignment,
                    ValueSensitivity::Public,
                    "CLI send override",
                )?;
            }
            "--secret-env" => {
                let mapping = next_option_value(arguments, &mut index, "--secret-env")?;
                let (name, environment_name) = mapping.split_once('=').ok_or_else(|| {
                    CliError::usage("--secret-env requires variable=ENVIRONMENT_NAME")
                })?;
                let value = env::var(environment_name).map_err(|_| {
                    CliError::validation(format!(
                        "environment variable {environment_name} is not available"
                    ))
                })?;
                insert_variable(
                    &mut options.variables,
                    name,
                    value,
                    ValueSensitivity::Secret,
                    format!("process environment {environment_name}"),
                )?;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unexpected send argument: {other}"
                )));
            }
        }
        index += 1;
    }
    if options.json && options.quiet {
        return Err(CliError::usage("--json and --quiet cannot be combined"));
    }
    Ok(options)
}

fn next_option_value<'a>(
    arguments: &'a [String],
    index: &mut usize,
    option: &str,
) -> Result<&'a str, CliError> {
    *index += 1;
    arguments
        .get(*index)
        .map(String::as_str)
        .ok_or_else(|| CliError::usage(format!("{option} requires a value")))
}

fn parse_positive_u64(value: &str, option: &str) -> Result<u64, CliError> {
    let value = value
        .parse::<u64>()
        .map_err(|_| CliError::usage(format!("{option} requires a positive integer")))?;
    if value == 0 {
        Err(CliError::usage(format!(
            "{option} requires a positive integer"
        )))
    } else {
        Ok(value)
    }
}

fn insert_assignment(
    context: &mut VariableContext,
    assignment: &str,
    sensitivity: ValueSensitivity,
    source: &str,
) -> Result<(), CliError> {
    let (name, value) = assignment
        .split_once('=')
        .ok_or_else(|| CliError::usage("variable assignment requires name=value"))?;
    insert_variable(
        context,
        name,
        value.to_owned(),
        sensitivity,
        source.to_owned(),
    )
}

fn insert_variable(
    context: &mut VariableContext,
    name: &str,
    value: String,
    sensitivity: ValueSensitivity,
    source: String,
) -> Result<(), CliError> {
    if name.trim().is_empty() {
        return Err(CliError::usage("variable name may not be empty"));
    }
    context.layer_mut(VariableScope::Request).insert(
        name,
        VariableDefinition {
            value: VariableValue::String(value),
            sensitivity,
            enabled: true,
            description: Some(source),
        },
    );
    Ok(())
}

fn find_workspace_root(request_path: &Path) -> Option<PathBuf> {
    request_path
        .parent()?
        .ancestors()
        .find(|path| path.join(branding::WORKSPACE_FILE).is_file())
        .map(Path::to_owned)
}

#[derive(Debug)]
struct CliEventSink {
    quiet: bool,
}

impl ExecutionEventSink for CliEventSink {
    fn emit(&self, event: ExecutionEvent) {
        if self.quiet {
            return;
        }
        match event {
            ExecutionEvent::Started { execution_id } => {
                eprintln!("execution {execution_id} started");
            }
            ExecutionEvent::ResponseHeaders {
                status,
                http_version,
            } => eprintln!("received {http_version} {status}"),
            ExecutionEvent::Completed => eprintln!("execution completed"),
            ExecutionEvent::Cancelled => eprintln!("execution cancelled"),
            ExecutionEvent::Failed { category, .. } => {
                eprintln!("execution failed: {category:?}");
            }
            ExecutionEvent::PhaseStarted(_)
            | ExecutionEvent::UploadProgress { .. }
            | ExecutionEvent::DownloadProgress { .. }
            | ExecutionEvent::StreamItem { .. } => {}
        }
    }
}

fn print_human_result(result: &apex_runner::ExecutionResult) -> Result<(), CliError> {
    let response = &result.response;
    println!(
        "{} {}{}",
        response.protocol_version,
        response.status.unwrap_or_default(),
        response
            .status_text
            .as_deref()
            .map(|text| format!(" {text}"))
            .unwrap_or_default()
    );
    println!("received: {} bytes", response.received_bytes);
    if response.decompressed {
        println!("wire size: {} bytes", response.wire_bytes);
    }
    if let Some(content_type) = &response.content_type {
        println!("content-type: {content_type}");
    }
    if !response.redirect_chain.is_empty() {
        println!("redirects: {}", response.redirect_chain.len());
        for redirect in &response.redirect_chain {
            println!("  {} {} -> {}", redirect.status, redirect.from, redirect.to);
        }
    }
    match &response.stored_body {
        StoredBody::Empty => {}
        StoredBody::InMemory(body) => {
            if body.len() <= MAXIMUM_STDOUT_BODY_BYTES {
                std::io::stdout()
                    .write_all(body)
                    .map_err(|error| CliError::io(error.to_string()))?;
                if !body.ends_with(b"\n") {
                    println!();
                }
            } else {
                println!(
                    "body retained in memory but not printed because it exceeds {} bytes",
                    MAXIMUM_STDOUT_BODY_BYTES
                );
            }
        }
        StoredBody::File { path, temporary } => {
            println!(
                "body file: {}{}",
                path.display(),
                if *temporary { " (temporary)" } else { "" }
            );
        }
        StoredBody::StreamLog(path) => println!("stream log: {}", path.display()),
    }
    Ok(())
}

fn print_json_result(result: &apex_runner::ExecutionResult) -> Result<(), CliError> {
    let response = &result.response;
    let body = match &response.stored_body {
        StoredBody::Empty => json!({"kind": "empty"}),
        StoredBody::InMemory(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => json!({"kind": "utf8", "text": text, "bytes": bytes.len()}),
            Err(_) => json!({"kind": "binary", "bytes": bytes.len(), "omitted": true}),
        },
        StoredBody::File { path, temporary } => json!({
            "kind": "file",
            "path": path,
            "temporary": temporary,
        }),
        StoredBody::StreamLog(path) => json!({"kind": "stream_log", "path": path}),
    };
    let value = json!({
        "execution_id": result.execution_id.to_string(),
        "status": response.status,
        "status_text": response.status_text,
        "protocol_version": response.protocol_version,
        "headers": response.headers,
        "trailers": response.trailers,
        "received_bytes": response.received_bytes,
        "wire_bytes": response.wire_bytes,
        "declared_content_length": response.declared_content_length,
        "content_type": response.content_type,
        "content_encoding": response.content_encoding,
        "decompressed": response.decompressed,
        "redirect_chain": response.redirect_chain.iter().map(|hop| json!({
            "status": hop.status,
            "from": hop.from,
            "to": hop.to,
        })).collect::<Vec<_>>(),
        "body": body,
        "diagnostics": result.diagnostics,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|error| CliError::io(error.to_string()))?
    );
    Ok(())
}

fn record_history(
    path: &Path,
    record: &HistoryRecord,
    policy: &HistoryPolicy,
) -> Result<(), String> {
    if !policy.enabled {
        return Ok(());
    }
    HistoryDatabase::open(path)
        .and_then(|database| database.insert(record, policy))
        .map_err(|error| error.to_string())
}

fn history_command(arguments: &[String]) -> Result<(), CliError> {
    let action = arguments.first().map(String::as_str).ok_or_else(|| {
        CliError::usage("usage: apex history <list|clear> <database> [--limit n] [--json]")
    })?;
    let path = arguments
        .get(1)
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage("history command requires a database path"))?;
    let database = HistoryDatabase::open(path).map_err(|error| CliError::io(error.to_string()))?;
    match action {
        "list" => {
            let mut limit = 100_usize;
            let mut json_output = false;
            let mut index = 2;
            while index < arguments.len() {
                match arguments[index].as_str() {
                    "--limit" => {
                        let value = next_option_value(arguments, &mut index, "--limit")?;
                        limit = value
                            .parse::<usize>()
                            .map_err(|_| CliError::usage("--limit requires a positive integer"))?;
                    }
                    "--json" => json_output = true,
                    other => {
                        return Err(CliError::usage(format!(
                            "unexpected history list argument: {other}"
                        )));
                    }
                }
                index += 1;
            }
            let records = database
                .list(limit)
                .map_err(|error| CliError::io(error.to_string()))?;
            if json_output {
                let values = records
                    .iter()
                    .map(|record| {
                        json!({
                            "execution_id": record.execution_id,
                            "request_id": record.request_id,
                            "request_name": record.request_name,
                            "timestamp_ms": record.timestamp_ms,
                            "environment": record.environment,
                            "method": record.method,
                            "resolved_url": record.resolved_url,
                            "status": record.status,
                            "duration_ms": record.duration_ms,
                            "response_size": record.response_size,
                            "error_category": record.error_category,
                            "pinned": record.pinned,
                        })
                    })
                    .collect::<Vec<_>>();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&values)
                        .map_err(|error| CliError::io(error.to_string()))?
                );
            } else {
                for record in records {
                    println!(
                        "{} {} {} status={} duration={}ms error={}",
                        record.timestamp_ms,
                        record.method,
                        record.request_name,
                        record
                            .status
                            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
                        record.duration_ms,
                        record.error_category.as_deref().unwrap_or("-")
                    );
                }
            }
            Ok(())
        }
        "clear" => {
            let removed = database
                .clear_unpinned()
                .map_err(|error| CliError::io(error.to_string()))?;
            println!("removed {removed} unpinned history entries");
            Ok(())
        }
        other => Err(CliError::usage(format!("unknown history action: {other}"))),
    }
}

fn environment_command(arguments: &[String]) -> Result<(), CliError> {
    let action = arguments.first().map(String::as_str).ok_or_else(|| {
        CliError::usage("usage: apex env <list|inspect> <workspace> [environment] [--json]")
    })?;
    let workspace = arguments
        .get(1)
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage("environment command requires a workspace path"))?;
    let repository =
        WorkspaceRepository::new(&workspace).map_err(|error| CliError::io(error.to_string()))?;
    match action {
        "list" => {
            let json_output = parse_only_json_flag(&arguments[2..])?;
            let manifest = repository
                .load_manifest()
                .map_err(|error| CliError::validation(error.to_string()))?;
            let environments = repository
                .list_environments()
                .map_err(|error| CliError::validation(error.to_string()))?;
            if json_output {
                let values = environments
                    .iter()
                    .map(|environment| {
                        json!({
                            "id": environment.id.as_str(),
                            "name": environment.name,
                            "path": environment.path,
                            "variable_count": environment.variable_count,
                            "default": manifest.value.default_environment.as_deref()
                                == Some(environment.id.as_str()),
                        })
                    })
                    .collect::<Vec<_>>();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&values)
                        .map_err(|error| CliError::io(error.to_string()))?
                );
            } else if environments.is_empty() {
                println!("no environments defined");
            } else {
                for environment in environments {
                    let default = if manifest.value.default_environment.as_deref()
                        == Some(environment.id.as_str())
                    {
                        " *"
                    } else {
                        ""
                    };
                    println!(
                        "{}{}\t{}\t{} variable(s)",
                        environment.id, default, environment.name, environment.variable_count
                    );
                }
            }
            Ok(())
        }
        "inspect" => inspect_environment(&repository, &arguments[2..]),
        other => Err(CliError::usage(format!(
            "unknown environment action: {other}"
        ))),
    }
}

fn inspect_environment(
    repository: &WorkspaceRepository,
    arguments: &[String],
) -> Result<(), CliError> {
    let mut requested_environment = None;
    let mut json_output = false;
    for argument in arguments {
        if argument == "--json" {
            json_output = true;
        } else if requested_environment.replace(argument.clone()).is_some() {
            return Err(CliError::usage(
                "usage: apex env inspect <workspace> [environment] [--json]",
            ));
        }
    }
    let manifest = repository
        .load_manifest()
        .map_err(|error| CliError::validation(error.to_string()))?;
    let selected = requested_environment.or(manifest.value.default_environment.clone());
    let mut documents = Vec::new();
    if let Some(workspace) = repository
        .load_workspace_variables()
        .map_err(|error| CliError::validation(error.to_string()))?
    {
        documents.push(("workspace", workspace.path, workspace.value));
    }
    let selected_id = selected
        .map(StableId::parse)
        .transpose()
        .map_err(|error| CliError::validation(error.to_string()))?;
    if let Some(id) = selected_id.as_ref() {
        let environment = repository
            .load_environment(id)
            .map_err(|error| CliError::validation(error.to_string()))?;
        documents.push(("environment", environment.path, environment.value));
        if let Some(local) = repository
            .load_local_environment_override(id)
            .map_err(|error| CliError::validation(error.to_string()))?
        {
            documents.push(("local_environment_override", local.path, local.value));
        }
    }
    if json_output {
        let values = documents
            .iter()
            .map(|(scope, path, document)| variable_set_json(scope, path, document))
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "workspace": manifest.value.name,
                "selected_environment": selected_id.as_ref().map(StableId::as_str),
                "layers": values,
            }))
            .map_err(|error| CliError::io(error.to_string()))?
        );
    } else {
        println!("workspace: {}", manifest.value.name);
        println!(
            "environment: {}",
            selected_id.as_ref().map_or("none", StableId::as_str)
        );
        if documents.is_empty() {
            println!("no variable layers defined");
        }
        for (scope, path, document) in documents {
            println!("\n[{scope}] {} ({})", document.name, path.display());
            for variable in document.variables {
                let (source, value) = stored_variable_preview(&variable);
                println!(
                    "{} = {}\t{}\t{}{}",
                    variable.name,
                    value,
                    source,
                    sensitivity_label(variable.sensitivity),
                    if variable.enabled { "" } else { " disabled" }
                );
            }
        }
    }
    Ok(())
}

fn parse_only_json_flag(arguments: &[String]) -> Result<bool, CliError> {
    let mut json_output = false;
    for argument in arguments {
        if argument == "--json" {
            json_output = true;
        } else {
            return Err(CliError::usage(format!(
                "unexpected environment argument: {argument}"
            )));
        }
    }
    Ok(json_output)
}

fn variable_set_json(
    scope: &str,
    path: &Path,
    document: &VariableSetDocument,
) -> serde_json::Value {
    let variables = document
        .variables
        .iter()
        .map(|variable| {
            let (source, value) = stored_variable_preview(variable);
            json!({
                "name": variable.name,
                "value": value,
                "source": source,
                "sensitivity": sensitivity_label(variable.sensitivity),
                "enabled": variable.enabled,
                "description": variable.description,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "scope": scope,
        "id": document.id.as_str(),
        "name": document.name,
        "path": path,
        "variables": variables,
    })
}

fn stored_variable_preview(variable: &apex_workspace::StoredVariable) -> (String, String) {
    match &variable.source {
        StoredVariableSource::Literal(value) => {
            let displayed = if variable.sensitivity == ValueSensitivity::Public {
                value.display_value()
            } else {
                "[REDACTED]".to_owned()
            };
            ("literal".to_owned(), displayed)
        }
        StoredVariableSource::Secret(reference) => (
            format!("secret {}", reference.display_name()),
            "[SECRET]".to_owned(),
        ),
        StoredVariableSource::ProcessEnvironment { name } => (
            format!("process environment {name}"),
            "[REDACTED]".to_owned(),
        ),
    }
}

const fn sensitivity_label(sensitivity: ValueSensitivity) -> &'static str {
    match sensitivity {
        ValueSensitivity::Public => "public",
        ValueSensitivity::Sensitive => "sensitive",
        ValueSensitivity::Secret => "secret",
    }
}

fn stable_slug(value: &str) -> String {
    let mut output = String::new();
    let mut previous_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !output.is_empty() {
            output.push('-');
            previous_separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "workspace".to_owned()
    } else {
        output
    }
}

fn send_usage() -> &'static str {
    "usage: apex send <request-file> [--environment id] [--no-local-environment] [--set name=value] [--secret-env name=ENV] [--download path] [--overwrite] [--max-response-bytes n] [--memory-threshold n] [--history-db path|--no-history] [--json|--quiet]"
}

fn print_help() {
    println!(
        "{name} {version}\n\nCommands:\n  doctor\n  init <directory> [name]\n  validate <workspace-directory>\n  resolve <template> [--workspace path] [--environment id] [--set name=value]...\n  env list <workspace> [--json]\n  env inspect <workspace> [environment] [--json]\n  import-curl <curl command>\n  send <request-file> [options]\n  history list <database> [--limit n] [--json]\n  history clear <database>\n",
        name = branding::PRODUCT_NAME,
        version = env!("CARGO_PKG_VERSION")
    );
}

#[derive(Debug)]
struct CliError {
    exit_code: u8,
    message: String,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_USAGE,
            message: message.into(),
        }
    }

    fn validation(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_VALIDATION,
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_IO,
            message: message.into(),
        }
    }

    fn import(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_IMPORT,
            message: message.into(),
        }
    }

    fn execution(error: ExecutionError) -> Self {
        let exit_code = match error {
            ExecutionError::Cancelled => EXIT_CANCELLED,
            ExecutionError::InvalidUrl(_)
            | ExecutionError::UnresolvedVariable(_)
            | ExecutionError::MissingSecret(_)
            | ExecutionError::InvalidWorkspace(_)
            | ExecutionError::UploadFailure(_) => EXIT_VALIDATION,
            ExecutionError::FilesystemConflict(_) => EXIT_IO,
            _ => EXIT_NETWORK,
        };
        Self {
            exit_code,
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{Authentication, HttpRequest, RequestBody};
    use apex_secrets::SecretRef;
    use apex_workspace::{StoredVariable, StoredVariableSource};

    #[test]
    fn slug_is_stable_and_path_safe() {
        assert_eq!(stable_slug(" My API / Users "), "my-api-users");
        assert_eq!(stable_slug("***"), "workspace");
    }

    #[test]
    fn send_options_support_history_controls() {
        let custom_path = std::env::temp_dir().join("apex-custom-history.sqlite");
        let options = parse_send_options(&[
            "request.toml".to_owned(),
            "--history-db".to_owned(),
            custom_path.display().to_string(),
            "--no-history".to_owned(),
        ])
        .expect("history options parse");
        assert!(options.no_history);
        assert_eq!(options.history_db.as_deref(), Some(custom_path.as_path()));
    }

    #[test]
    fn disabled_history_does_not_create_a_database() {
        let path = std::env::temp_dir().join(format!(
            "apex-cli-disabled-history-{}.sqlite",
            apex_domain::ExecutionId::new()
        ));
        let policy = HistoryPolicy {
            enabled: false,
            ..HistoryPolicy::default()
        };
        let request = HttpRequest {
            id: StableId::parse("history-disabled").expect("id"),
            name: "History disabled".to_owned(),
            method: apex_domain::HttpMethod::Get,
            url: "https://example.test".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication: Authentication::None,
            body: RequestBody::Empty,
            settings: apex_domain::RequestSettings::default(),
            documentation: String::new(),
        };
        let record = HistoryRecord::success(
            apex_domain::ExecutionId::new().to_string(),
            &request,
            std::time::Duration::from_millis(1),
            Some(200),
            0,
            &policy,
        );
        record_history(&path, &record, &policy).expect("disabled history is a no-op");
        assert!(!path.exists());
    }

    #[test]
    fn send_options_accept_environment_secret_mapping() {
        let options = parse_send_options(&[
            "request.toml".to_owned(),
            "--secret-env".to_owned(),
            "token=APEX_TEST_TOKEN".to_owned(),
        ]);
        assert!(
            options.is_err(),
            "missing environment variable must fail closed"
        );
    }

    #[test]
    fn send_options_select_environment_and_disable_local_override() {
        let options = parse_send_options(&[
            "request.toml".to_owned(),
            "--environment".to_owned(),
            "staging".to_owned(),
            "--no-local-environment".to_owned(),
        ])
        .expect("environment options parse");
        assert_eq!(options.environment.as_deref(), Some("staging"));
        assert!(!options.include_local_environment);
    }

    #[test]
    fn environment_preview_never_reveals_sensitive_or_secret_values() {
        let sensitive = StoredVariable {
            name: "internal_host".to_owned(),
            source: StoredVariableSource::Literal(VariableValue::String(
                "internal.example.test".to_owned(),
            )),
            sensitivity: ValueSensitivity::Sensitive,
            enabled: true,
            description: None,
        };
        assert_eq!(
            stored_variable_preview(&sensitive),
            ("literal".to_owned(), "[REDACTED]".to_owned())
        );

        let secret = StoredVariable {
            name: "access_token".to_owned(),
            source: StoredVariableSource::Secret(
                SecretRef::new("staging", "access-token").expect("valid secret reference"),
            ),
            sensitivity: ValueSensitivity::Secret,
            enabled: true,
            description: None,
        };
        let (source, value) = stored_variable_preview(&secret);
        assert!(source.contains("staging/access-token"));
        assert_eq!(value, "[SECRET]");
    }
}
