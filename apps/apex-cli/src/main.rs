#![forbid(unsafe_code)]

use apex_domain::{
    ExecutionError, ExecutionEvent, StableId, ValueSensitivity, VariableDefinition, VariableValue,
    branding,
};
use apex_export::{CodeTarget, CodegenOptions, generate as generate_code};
use apex_history::{
    BodyDifference, HistoryDatabase, HistoryPolicy, HistoryQuery, HistoryRecord, HistorySnapshot,
    SemanticDiffPolicy, semantic_response_diff,
};
use apex_http::HttpAdapter;
use apex_import::{ImportPreview, parse_curl, parse_postman_v21};
use apex_runner::{
    ExecutionContext, ExecutionEventSink, ProtocolAdapter, ProtocolRequest, ResolvedRequest,
    StoredBody,
};
use apex_secrets::{EnvironmentSecretStore, SecretLeakDetector, SecretStoreChain};
use apex_variables::{
    ResolverOptions, SystemDynamicVariables, VariableContext, VariableResolver, VariableScope,
    WorkspaceVariableSelection, load_workspace_variables,
    resolve_http_request as resolve_shared_http_request,
};
use apex_workspace::{
    SearchField, SearchIndexPolicy, SearchQuery, StoredVariableSource, VariableSetDocument,
    WorkspaceManifest, WorkspaceRepository, WorkspaceSearchIndex, format_request,
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
        "import-postman" => import_postman(&arguments[1..]),
        "search" => search_workspace(&arguments[1..]),
        "codegen" => codegen_request(&arguments[1..]),
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

fn import_postman(arguments: &[String]) -> Result<(), CliError> {
    let path = arguments
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage("usage: apex import-postman <collection.json> [--json]"))?;
    let json_output = arguments
        .iter()
        .skip(1)
        .any(|argument| argument == "--json");
    if arguments
        .iter()
        .skip(1)
        .any(|argument| argument != "--json")
    {
        return Err(CliError::usage(
            "usage: apex import-postman <collection.json> [--json]",
        ));
    }
    let input = std::fs::read(&path)
        .map_err(|error| CliError::io(format!("{}: {error}", path.display())))?;
    let preview = parse_postman_v21(&input).map_err(|error| CliError::import(error.to_string()))?;
    print_import_preview(&preview, json_output)
}

fn print_import_preview(preview: &ImportPreview, json_output: bool) -> Result<(), CliError> {
    if json_output {
        let diagnostics = preview
            .diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "severity": format!("{:?}", diagnostic.severity).to_ascii_lowercase(),
                    "code": diagnostic.code,
                    "message": diagnostic.message,
                    "source_path": diagnostic.source_path,
                })
            })
            .collect::<Vec<_>>();
        let requests = preview
            .requests
            .iter()
            .map(|request| {
                json!({
                    "id": request.request.id.as_str(),
                    "name": request.request.name,
                    "method": request.request.method.as_str(),
                    "url": request.request.url,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source_format": preview.source_format,
                "requests": requests,
                "diagnostics": diagnostics,
                "unsupported_fields": preview.unsupported_fields,
                "has_errors": preview.has_errors(),
            }))
            .map_err(|error| CliError::import(error.to_string()))?
        );
        return Ok(());
    }

    println!("source format: {}", preview.source_format);
    println!("requests: {}", preview.requests.len());
    println!("unsupported fields: {}", preview.unsupported_fields.len());
    for field in &preview.unsupported_fields {
        println!("unsupported: {field}");
    }
    for diagnostic in &preview.diagnostics {
        println!(
            "diagnostic: {:?} {}: {}{}",
            diagnostic.severity,
            diagnostic.code,
            diagnostic.message,
            diagnostic
                .source_path
                .as_ref()
                .map_or_else(String::new, |path| format!(" [{path}]"))
        );
    }
    for request in &preview.requests {
        println!("--- request preview ---");
        print!("{}", format_request(request));
    }
    Ok(())
}

fn search_workspace(arguments: &[String]) -> Result<(), CliError> {
    if arguments.len() < 2 {
        return Err(CliError::usage(
            "usage: apex search <workspace> <query> [--exact] [--method METHOD] [--field FIELD] [--limit N] [--json]",
        ));
    }
    let repository =
        WorkspaceRepository::new(&arguments[0]).map_err(|error| CliError::io(error.to_string()))?;
    let mut query = SearchQuery {
        text: arguments[1].clone(),
        ..SearchQuery::default()
    };
    let mut json_output = false;
    let mut index = 2;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--exact" => {
                query.exact = true;
                index += 1;
            }
            "--method" => {
                query.method = Some(
                    arguments
                        .get(index + 1)
                        .ok_or_else(|| CliError::usage("--method requires a value"))?
                        .clone(),
                );
                index += 2;
            }
            "--field" => {
                query.field =
                    Some(parse_search_field(arguments.get(index + 1).ok_or_else(
                        || CliError::usage("--field requires a value"),
                    )?)?);
                index += 2;
            }
            "--limit" => {
                query.limit = Some(
                    arguments
                        .get(index + 1)
                        .ok_or_else(|| CliError::usage("--limit requires a value"))?
                        .parse::<usize>()
                        .map_err(|_| CliError::usage("--limit must be a positive integer"))?,
                );
                index += 2;
            }
            "--json" => {
                json_output = true;
                index += 1;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unexpected search argument: {other}"
                )));
            }
        }
    }
    let mut search = WorkspaceSearchIndex::open(&repository, SearchIndexPolicy::default())
        .map_err(|error| CliError::io(error.to_string()))?;
    let refresh = search
        .refresh(&repository)
        .map_err(|error| CliError::validation(error.to_string()))?;
    let results = search
        .search(&query)
        .map_err(|error| CliError::validation(error.to_string()))?;
    if json_output {
        let results = results
            .iter()
            .map(|result| {
                json!({
                    "path": result.relative_path,
                    "name": result.name,
                    "method": result.method,
                    "url": result.url,
                    "score": result.score,
                    "matched_fields": result.matched_fields.iter().map(|field| format!("{field:?}").to_ascii_lowercase()).collect::<Vec<_>>(),
                    "truncated_source": result.truncated_source,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "refresh": {
                    "scanned": refresh.scanned,
                    "inserted": refresh.inserted,
                    "updated": refresh.updated,
                    "unchanged": refresh.unchanged,
                    "removed": refresh.removed,
                    "truncated": refresh.truncated,
                },
                "results": results,
            }))
            .map_err(|error| CliError::validation(error.to_string()))?
        );
    } else {
        for result in results {
            println!(
                "{}\t{}\t{}\t{}\tscore={}",
                result.method,
                result.name,
                result.url,
                result.relative_path.display(),
                result.score
            );
        }
    }
    Ok(())
}

fn parse_search_field(value: &str) -> Result<SearchField, CliError> {
    match value {
        "name" => Ok(SearchField::Name),
        "method" => Ok(SearchField::Method),
        "url" => Ok(SearchField::Url),
        "headers" => Ok(SearchField::Headers),
        "body" => Ok(SearchField::Body),
        "documentation" | "docs" => Ok(SearchField::Documentation),
        other => Err(CliError::usage(format!(
            "unknown search field '{other}'; expected name, method, url, headers, body, or documentation"
        ))),
    }
}

fn codegen_request(arguments: &[String]) -> Result<(), CliError> {
    if arguments.len() != 2 {
        return Err(CliError::usage(
            "usage: apex codegen <request-file> <curl|httpie|rust-reqwest|python-requests|go-net-http>",
        ));
    }
    let request_path = std::fs::canonicalize(&arguments[0])
        .map_err(|error| CliError::io(format!("{}: {error}", arguments[0])))?;
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
    let target = parse_code_target(&arguments[1])?;
    let generated = generate_code(&loaded.value.request, target, CodegenOptions::default())
        .map_err(|error| CliError::validation(error.to_string()))?;
    print!("{}", generated.code);
    if !generated.code.ends_with('\n') {
        println!();
    }
    for warning in generated.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn parse_code_target(value: &str) -> Result<CodeTarget, CliError> {
    match value {
        "curl" => Ok(CodeTarget::Curl),
        "httpie" => Ok(CodeTarget::Httpie),
        "rust-reqwest" => Ok(CodeTarget::RustReqwest),
        "python-requests" => Ok(CodeTarget::PythonRequests),
        "go-net-http" => Ok(CodeTarget::GoNetHttp),
        other => Err(CliError::usage(format!("unknown code target '{other}'"))),
    }
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
    history_request_snapshot: bool,
    history_response_snapshot: bool,
    history_snapshot_bytes: Option<usize>,
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
    let history_source_document = loaded.value.clone();
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
        store_request_snapshot: options.history_request_snapshot,
        store_response_snapshot: options.history_response_snapshot,
        maximum_snapshot_bytes: options
            .history_snapshot_bytes
            .unwrap_or_else(|| HistoryPolicy::default().maximum_snapshot_bytes),
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
            let snapshot = HistorySnapshot::capture(
                Some(&history_source_document),
                Some(&result),
                &policy,
                &SecretLeakDetector::default(),
            );
            match snapshot {
                Ok(snapshot) => {
                    if let Err(error) =
                        record_history(&history_path, &record, Some(&snapshot), &policy)
                    {
                        result
                            .diagnostics
                            .push(format!("history was not recorded: {error}"));
                    }
                }
                Err(error) => result
                    .diagnostics
                    .push(format!("history snapshot was not recorded: {error}")),
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
            let snapshot = HistorySnapshot::capture(
                Some(&history_source_document),
                None,
                &policy,
                &SecretLeakDetector::default(),
            );
            let history_result = match snapshot {
                Ok(snapshot) => record_history(&history_path, &record, Some(&snapshot), &policy),
                Err(error) => Err(error.to_string()),
            };
            if let Err(history_error) = history_result
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
        history_request_snapshot: false,
        history_response_snapshot: false,
        history_snapshot_bytes: None,
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
            "--history-request-snapshot" => options.history_request_snapshot = true,
            "--history-response-snapshot" => options.history_response_snapshot = true,
            "--history-snapshot-bytes" => {
                options.history_snapshot_bytes = Some(parse_positive_usize(
                    next_option_value(arguments, &mut index, "--history-snapshot-bytes")?,
                    "--history-snapshot-bytes",
                )?);
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
    if options.no_history
        && (options.history_request_snapshot
            || options.history_response_snapshot
            || options.history_snapshot_bytes.is_some())
    {
        return Err(CliError::usage(
            "history snapshot options cannot be combined with --no-history",
        ));
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

fn parse_positive_usize(value: &str, option: &str) -> Result<usize, CliError> {
    let value = value
        .parse::<usize>()
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
    snapshot: Option<&HistorySnapshot>,
    policy: &HistoryPolicy,
) -> Result<(), String> {
    if !policy.enabled {
        return Ok(());
    }
    HistoryDatabase::open(path)
        .and_then(|database| database.insert_with_snapshot(record, snapshot, policy))
        .map_err(|error| error.to_string())
}

fn history_command(arguments: &[String]) -> Result<(), CliError> {
    let action = arguments.first().map(String::as_str).ok_or_else(|| {
        CliError::usage("usage: apex history <list|restore|diff|clear> <database> [arguments]")
    })?;
    let path = arguments
        .get(1)
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage("history command requires a database path"))?;
    let database = HistoryDatabase::open(path).map_err(|error| CliError::io(error.to_string()))?;
    match action {
        "list" => history_list(&database, &arguments[2..]),
        "restore" => history_restore(&database, &arguments[2..]),
        "diff" => history_diff(&database, &arguments[2..]),
        "clear" => {
            if arguments.len() != 2 {
                return Err(CliError::usage("usage: apex history clear <database>"));
            }
            let removed = database
                .clear_unpinned()
                .map_err(|error| CliError::io(error.to_string()))?;
            println!("removed {removed} unpinned history entries");
            Ok(())
        }
        other => Err(CliError::usage(format!("unknown history action: {other}"))),
    }
}

fn history_list(database: &HistoryDatabase, arguments: &[String]) -> Result<(), CliError> {
    let mut query = HistoryQuery::default();
    let mut json_output = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--limit" => {
                query.limit = parse_positive_usize(
                    next_option_value(arguments, &mut index, "--limit")?,
                    "--limit",
                )?;
            }
            "--method" => {
                query.method =
                    Some(next_option_value(arguments, &mut index, "--method")?.to_owned());
            }
            "--status" => {
                query.status = Some(
                    next_option_value(arguments, &mut index, "--status")?
                        .parse::<u16>()
                        .map_err(|_| CliError::usage("--status requires an HTTP status code"))?,
                );
            }
            "--request" => {
                query.request_id =
                    Some(next_option_value(arguments, &mut index, "--request")?.to_owned());
            }
            "--environment" => {
                query.environment =
                    Some(next_option_value(arguments, &mut index, "--environment")?.to_owned());
            }
            "--error" => {
                query.error_category =
                    Some(next_option_value(arguments, &mut index, "--error")?.to_owned());
            }
            "--text" => {
                query.text = Some(next_option_value(arguments, &mut index, "--text")?.to_owned());
            }
            "--after" => {
                query.after_timestamp_ms = Some(
                    next_option_value(arguments, &mut index, "--after")?
                        .parse::<i64>()
                        .map_err(|_| CliError::usage("--after requires a millisecond timestamp"))?,
                );
            }
            "--before" => {
                query.before_timestamp_ms = Some(
                    next_option_value(arguments, &mut index, "--before")?
                        .parse::<i64>()
                        .map_err(|_| {
                            CliError::usage("--before requires a millisecond timestamp")
                        })?,
                );
            }
            "--pinned" => query.pinned = Some(true),
            "--unpinned" => query.pinned = Some(false),
            "--json" => json_output = true,
            other => {
                return Err(CliError::usage(format!(
                    "unexpected history list argument: {other}"
                )));
            }
        }
        index += 1;
    }
    let entries = database
        .query(&query)
        .map_err(|error| CliError::io(error.to_string()))?;
    if json_output {
        let values = entries.iter().map(history_entry_json).collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&values)
                .map_err(|error| CliError::io(error.to_string()))?
        );
    } else {
        for entry in entries {
            let record = &entry.record;
            let snapshot = entry.snapshot.as_ref();
            println!(
                "{} {} {} status={} duration={}ms error={} snapshot={}{}",
                record.timestamp_ms,
                record.method,
                record.request_name,
                record
                    .status
                    .map_or_else(|| "-".to_owned(), |value| value.to_string()),
                record.duration_ms,
                record.error_category.as_deref().unwrap_or("-"),
                if snapshot.is_some() { "yes" } else { "no" },
                snapshot.map_or_else(String::new, |snapshot| {
                    if snapshot.request_truncated || snapshot.response_truncated {
                        " (truncated)".to_owned()
                    } else {
                        String::new()
                    }
                })
            );
        }
    }
    Ok(())
}

fn history_entry_json(entry: &apex_history::HistoryEntry) -> serde_json::Value {
    let record = &entry.record;
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
        "snapshot": entry.snapshot.as_ref().map(|snapshot| json!({
            "request_available": snapshot.request_toml.is_some(),
            "request_truncated": snapshot.request_truncated,
            "response_available": snapshot.response_body.is_some(),
            "response_status": snapshot.response_status,
            "response_content_type": snapshot.response_content_type,
            "response_truncated": snapshot.response_truncated,
        })),
    })
}

fn history_restore(database: &HistoryDatabase, arguments: &[String]) -> Result<(), CliError> {
    let execution_id = arguments.first().ok_or_else(|| {
        CliError::usage(
            "usage: apex history restore <database> <execution-id> [--output path] [--overwrite]",
        )
    })?;
    let mut output = None::<PathBuf>;
    let mut overwrite = false;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--output" => {
                output = Some(PathBuf::from(next_option_value(
                    arguments, &mut index, "--output",
                )?));
            }
            "--overwrite" => overwrite = true,
            other => {
                return Err(CliError::usage(format!(
                    "unexpected history restore argument: {other}"
                )));
            }
        }
        index += 1;
    }
    let document = database
        .restore_request(execution_id)
        .map_err(|error| CliError::validation(error.to_string()))?
        .ok_or_else(|| {
            CliError::validation(format!(
                "history entry '{execution_id}' has no restorable request snapshot"
            ))
        })?;
    let formatted = format_request(&document);
    if let Some(path) = output {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true);
        if overwrite {
            options.truncate(true);
        } else {
            options.create_new(true);
        }
        let mut file = options
            .open(&path)
            .map_err(|error| CliError::io(format!("{}: {error}", path.display())))?;
        file.write_all(formatted.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| CliError::io(format!("{}: {error}", path.display())))?;
        println!("restored request to {}", path.display());
    } else {
        print!("{formatted}");
    }
    Ok(())
}

fn history_diff(database: &HistoryDatabase, arguments: &[String]) -> Result<(), CliError> {
    if arguments.len() < 2 || arguments.len() > 3 {
        return Err(CliError::usage(
            "usage: apex history diff <database> <left-id> <right-id> [--json]",
        ));
    }
    let json_output = arguments.get(2).is_some_and(|value| value == "--json");
    if arguments.len() == 3 && !json_output {
        return Err(CliError::usage(
            "usage: apex history diff <database> <left-id> <right-id> [--json]",
        ));
    }
    let left = database
        .get(&arguments[0])
        .map_err(|error| CliError::io(error.to_string()))?
        .ok_or_else(|| {
            CliError::validation(format!("history entry not found: {}", arguments[0]))
        })?;
    let right = database
        .get(&arguments[1])
        .map_err(|error| CliError::io(error.to_string()))?
        .ok_or_else(|| {
            CliError::validation(format!("history entry not found: {}", arguments[1]))
        })?;
    let diff = semantic_response_diff(&left, &right, &SemanticDiffPolicy::default());
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&history_diff_json(&diff))
                .map_err(|error| CliError::io(error.to_string()))?
        );
    } else {
        println!(
            "status: {:?} -> {:?}{}",
            diff.status.left,
            diff.status.right,
            if diff.status.changed {
                " (changed)"
            } else {
                ""
            }
        );
        println!(
            "duration: {}ms -> {}ms{}",
            diff.duration_ms.left,
            diff.duration_ms.right,
            if diff.duration_ms.changed {
                " (changed)"
            } else {
                ""
            }
        );
        println!("header changes: {}", diff.headers.len());
        println!("cookie changes: {}", diff.cookies.len());
        println!("body: {}", body_diff_label(&diff.body));
    }
    Ok(())
}

fn history_diff_json(diff: &apex_history::SemanticResponseDiff) -> serde_json::Value {
    let body = match &diff.body {
        BodyDifference::Unavailable => json!({"kind": "unavailable"}),
        BodyDifference::Unchanged => json!({"kind": "unchanged"}),
        BodyDifference::Json(body) => json!({
            "kind": "json",
            "truncated": body.truncated,
            "changes": body.changes.iter().map(|change| json!({
                "pointer": change.pointer,
                "kind": format!("{:?}", change.kind).to_ascii_lowercase(),
                "left": change.left,
                "right": change.right,
            })).collect::<Vec<_>>(),
        }),
        BodyDifference::Text(body) => json!({
            "kind": "text",
            "common_prefix_lines": body.common_prefix_lines,
            "common_suffix_lines": body.common_suffix_lines,
            "left_changed_lines": body.left_changed_lines,
            "right_changed_lines": body.right_changed_lines,
            "truncated": body.truncated,
        }),
        BodyDifference::Binary(body) => json!({
            "kind": "binary",
            "left_length": body.left_length,
            "right_length": body.right_length,
            "first_difference": body.first_difference,
            "left_truncated": body.left_truncated,
            "right_truncated": body.right_truncated,
        }),
    };
    json!({
        "status": {
            "left": diff.status.left,
            "right": diff.status.right,
            "changed": diff.status.changed,
        },
        "duration_ms": {
            "left": diff.duration_ms.left,
            "right": diff.duration_ms.right,
            "changed": diff.duration_ms.changed,
        },
        "response_size": {
            "left": diff.response_size.left,
            "right": diff.response_size.right,
            "changed": diff.response_size.changed,
        },
        "headers": diff.headers.iter().map(|header| json!({
            "name": header.name,
            "left_values": header.left_values,
            "right_values": header.right_values,
        })).collect::<Vec<_>>(),
        "cookies": diff.cookies.iter().map(|cookie| json!({
            "name": cookie.name,
            "left_value": cookie.left_value,
            "right_value": cookie.right_value,
        })).collect::<Vec<_>>(),
        "body": body,
    })
}

fn body_diff_label(body: &BodyDifference) -> &'static str {
    match body {
        BodyDifference::Unavailable => "unavailable",
        BodyDifference::Unchanged => "unchanged",
        BodyDifference::Json(_) => "JSON structural changes",
        BodyDifference::Text(_) => "text changes",
        BodyDifference::Binary(_) => "binary changes",
    }
}

fn environment_command(arguments: &[String]) -> Result<(), CliError> {
    let action = arguments.first().map(String::as_str).ok_or_else(|| {
        CliError::usage(
            "usage: apex env <list|inspect|create|rename|delete|default> <workspace> [arguments]",
        )
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
                            "has_local_override": environment.has_local_override,
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
        "create" => create_environment(&repository, &arguments[2..]),
        "rename" => rename_environment(&repository, &arguments[2..]),
        "delete" => delete_environment(&repository, &arguments[2..]),
        "default" => set_default_environment(&repository, &arguments[2..]),
        other => Err(CliError::usage(format!(
            "unknown environment action: {other}"
        ))),
    }
}

fn create_environment(
    repository: &WorkspaceRepository,
    arguments: &[String],
) -> Result<(), CliError> {
    if arguments.len() != 2 {
        return Err(CliError::usage(
            "usage: apex env create <workspace> <id> <name>",
        ));
    }
    let id = StableId::parse(arguments[0].clone())
        .map_err(|error| CliError::validation(error.to_string()))?;
    let document = VariableSetDocument::new(id.clone(), arguments[1].clone());
    repository
        .create_environment(&document)
        .map_err(|error| CliError::validation(error.to_string()))?;
    println!("created environment {} ({})", id, document.name);
    Ok(())
}

fn rename_environment(
    repository: &WorkspaceRepository,
    arguments: &[String],
) -> Result<(), CliError> {
    if arguments.len() != 2 {
        return Err(CliError::usage(
            "usage: apex env rename <workspace> <id> <name>",
        ));
    }
    let id = StableId::parse(arguments[0].clone())
        .map_err(|error| CliError::validation(error.to_string()))?;
    let loaded = repository
        .load_environment(&id)
        .map_err(|error| CliError::validation(error.to_string()))?;
    repository
        .rename_environment(&id, arguments[1].clone(), loaded.fingerprint)
        .map_err(|error| CliError::validation(error.to_string()))?;
    println!("renamed environment {} to {}", id, arguments[1]);
    Ok(())
}

fn delete_environment(
    repository: &WorkspaceRepository,
    arguments: &[String],
) -> Result<(), CliError> {
    if arguments.len() != 1 {
        return Err(CliError::usage("usage: apex env delete <workspace> <id>"));
    }
    let id = StableId::parse(arguments[0].clone())
        .map_err(|error| CliError::validation(error.to_string()))?;
    let loaded = repository
        .load_environment(&id)
        .map_err(|error| CliError::validation(error.to_string()))?;
    let receipt = repository
        .delete_environment(&id, loaded.fingerprint)
        .map_err(|error| CliError::validation(error.to_string()))?;
    match receipt.cleanup_pending {
        Some(path) => println!(
            "deleted environment {}; cleanup remains at {}",
            receipt.id,
            path.display()
        ),
        None => println!("deleted environment {}", receipt.id),
    }
    Ok(())
}

fn set_default_environment(
    repository: &WorkspaceRepository,
    arguments: &[String],
) -> Result<(), CliError> {
    if arguments.len() != 1 {
        return Err(CliError::usage(
            "usage: apex env default <workspace> <id|none>",
        ));
    }
    let selected = if arguments[0] == "none" {
        None
    } else {
        Some(
            StableId::parse(arguments[0].clone())
                .map_err(|error| CliError::validation(error.to_string()))?,
        )
    };
    let manifest = repository
        .load_manifest()
        .map_err(|error| CliError::validation(error.to_string()))?;
    repository
        .set_default_environment(selected.as_ref(), manifest.fingerprint)
        .map_err(|error| CliError::validation(error.to_string()))?;
    println!(
        "default environment: {}",
        selected.as_ref().map_or("none", StableId::as_str)
    );
    Ok(())
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
        "{name} {version}\n\nCommands:\n  doctor\n  init <directory> [name]\n  validate <workspace-directory>\n  resolve <template> [--workspace path] [--environment id] [--set name=value]...\n  env list <workspace> [--json]\n  env inspect <workspace> [environment] [--json]\n  env create <workspace> <id> <name>\n  env rename <workspace> <id> <name>\n  env delete <workspace> <id>\n  env default <workspace> <id|none>\n  import-curl <curl command>\n  import-postman <collection.json> [--json]\n  search <workspace> <query> [filters]\n  codegen <request-file> <target>\n  send <request-file> [options]\n  history list <database> [filters] [--json]\n  history restore <database> <execution-id> [--output path]\n  history diff <database> <left-id> <right-id> [--json]\n  history clear <database>\n",
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
        record_history(&path, &record, None, &policy).expect("disabled history is a no-op");
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
    fn environment_crud_commands_update_real_workspace_state() {
        let root = std::env::temp_dir().join(format!(
            "apex-cli-env-crud-{}",
            apex_domain::ExecutionId::new()
        ));
        let repository = WorkspaceRepository::new(&root).expect("repository");
        repository
            .initialize(&WorkspaceManifest::new(
                StableId::parse("workspace").expect("workspace id"),
                "CLI environment fixture",
            ))
            .expect("initialize");

        create_environment(
            &repository,
            &["development".to_owned(), "Development".to_owned()],
        )
        .expect("create");
        rename_environment(
            &repository,
            &["development".to_owned(), "Developer machine".to_owned()],
        )
        .expect("rename");
        assert_eq!(
            repository
                .load_environment(&StableId::parse("development").unwrap())
                .unwrap()
                .value
                .name,
            "Developer machine"
        );

        set_default_environment(&repository, &["development".to_owned()]).expect("set default");
        assert!(delete_environment(&repository, &["development".to_owned()]).is_err());
        set_default_environment(&repository, &["none".to_owned()]).expect("clear default");
        delete_environment(&repository, &["development".to_owned()]).expect("delete");
        assert!(repository.list_environments().unwrap().is_empty());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn search_codegen_and_postman_commands_use_real_files() {
        let root = std::env::temp_dir().join(format!(
            "apex-cli-productivity-{}",
            apex_domain::ExecutionId::new()
        ));
        let repository = WorkspaceRepository::new(&root).expect("repository");
        repository
            .initialize(&WorkspaceManifest::new(
                StableId::parse("workspace").expect("workspace id"),
                "CLI productivity fixture",
            ))
            .expect("initialize");
        let collection = repository
            .collection_path("users")
            .expect("collection path");
        std::fs::create_dir_all(&collection).expect("create collection");
        let request_path = collection.join("get-user.request.toml");
        let request = HttpRequest {
            id: StableId::parse("get-user").expect("request id"),
            name: "Get user".to_owned(),
            method: apex_domain::HttpMethod::Get,
            url: "https://example.test/users/1".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication: Authentication::Bearer {
                token: "{{access_token}}".to_owned(),
            },
            body: RequestBody::Empty,
            settings: apex_domain::RequestSettings::default(),
            documentation: "customer profile marker".to_owned(),
        };
        repository
            .save_request(
                &request_path,
                &apex_workspace::RequestDocument::new(request),
                None,
                &apex_secrets::SecretLeakDetector::default(),
            )
            .expect("save request");

        search_workspace(&[
            root.display().to_string(),
            "customer".to_owned(),
            "--exact".to_owned(),
        ])
        .expect("search command");
        assert!(root.join(".apex/search.sqlite").exists());
        codegen_request(&[request_path.display().to_string(), "curl".to_owned()])
            .expect("codegen command");

        let postman_path = root.join("postman.json");
        std::fs::write(
            &postman_path,
            serde_json::to_vec(&json!({
                "info": {
                    "name": "Imported",
                    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
                },
                "auth": {
                    "type": "bearer",
                    "bearer": [{"key": "token", "value": "must-not-appear"}]
                },
                "item": [{
                    "name": "Imported request",
                    "request": {"method": "GET", "url": "https://example.test/imported"}
                }]
            }))
            .expect("serialize Postman fixture"),
        )
        .expect("write Postman fixture");
        import_postman(&[postman_path.display().to_string(), "--json".to_owned()])
            .expect("Postman command");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn productivity_command_target_parsers_fail_closed() {
        assert!(parse_code_target("unknown").is_err());
        assert!(parse_search_field("secrets").is_err());
    }

    #[test]
    fn send_options_support_bounded_opt_in_history_snapshots() {
        let options = parse_send_options(&[
            "request.toml".to_owned(),
            "--history-request-snapshot".to_owned(),
            "--history-response-snapshot".to_owned(),
            "--history-snapshot-bytes".to_owned(),
            "4096".to_owned(),
        ])
        .expect("snapshot options parse");
        assert!(options.history_request_snapshot);
        assert!(options.history_response_snapshot);
        assert_eq!(options.history_snapshot_bytes, Some(4096));
        assert!(
            parse_send_options(&[
                "request.toml".to_owned(),
                "--no-history".to_owned(),
                "--history-request-snapshot".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn history_restore_filter_and_diff_commands_use_snapshot_data() {
        let root = std::env::temp_dir().join(format!(
            "apex-cli-history-{}",
            apex_domain::ExecutionId::new()
        ));
        std::fs::create_dir_all(&root).expect("create history fixture");
        let database_path = root.join("history.sqlite");
        let database = HistoryDatabase::open(&database_path).expect("history database");
        let policy = HistoryPolicy {
            store_request_snapshot: true,
            store_response_snapshot: true,
            ..HistoryPolicy::default()
        };
        let request = HttpRequest {
            id: StableId::parse("history-request").expect("request id"),
            name: "History request".to_owned(),
            method: apex_domain::HttpMethod::Get,
            url: "https://example.test/history".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication: Authentication::None,
            body: RequestBody::Empty,
            settings: apex_domain::RequestSettings::default(),
            documentation: "restorable".to_owned(),
        };
        let document = apex_workspace::RequestDocument::new(request.clone());
        for (execution_id, status, body) in [
            ("history-left", 200, br#"{"value":1}"#.as_slice()),
            ("history-right", 201, br#"{"value":2}"#.as_slice()),
        ] {
            let record = HistoryRecord::success(
                execution_id,
                &request,
                std::time::Duration::from_millis(u64::from(status)),
                Some(status),
                body.len() as u64,
                &policy,
            );
            let snapshot = HistorySnapshot {
                request_toml: Some(format_request(&document)),
                response_status: Some(status),
                response_headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
                response_body: Some(body.to_vec()),
                response_content_type: Some("application/json".to_owned()),
                ..HistorySnapshot::default()
            };
            database
                .insert_with_snapshot(&record, Some(&snapshot), &policy)
                .expect("insert history snapshot");
        }

        history_list(
            &database,
            &[
                "--method".to_owned(),
                "GET".to_owned(),
                "--status".to_owned(),
                "201".to_owned(),
                "--text".to_owned(),
                "History".to_owned(),
            ],
        )
        .expect("filtered history list");
        let restored = root.join("restored.request.toml");
        history_restore(
            &database,
            &[
                "history-left".to_owned(),
                "--output".to_owned(),
                restored.display().to_string(),
            ],
        )
        .expect("restore history request");
        let restored_text = std::fs::read_to_string(&restored).expect("read restored request");
        assert_eq!(
            apex_workspace::parse_request(&restored_text)
                .expect("parse restored request")
                .request
                .id,
            request.id
        );
        history_diff(
            &database,
            &[
                "history-left".to_owned(),
                "history-right".to_owned(),
                "--json".to_owned(),
            ],
        )
        .expect("history diff");
        std::fs::remove_dir_all(root).expect("cleanup");
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
