#![forbid(unsafe_code)]

use apex_domain::CancellationToken;
use rhai::{Dynamic, Engine, EvalAltResult, Map, Scope};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ScriptLimits {
    pub timeout: Duration,
    pub maximum_operations: u64,
    pub maximum_string_bytes: usize,
    pub maximum_array_items: usize,
    pub maximum_map_items: usize,
    pub maximum_log_entries: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            maximum_operations: 100_000,
            maximum_string_bytes: 256 * 1024,
            maximum_array_items: 10_000,
            maximum_map_items: 10_000,
            maximum_log_entries: 256,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScriptContext {
    pub request: Value,
    pub response: Value,
    pub variables: Value,
    pub environment: Value,
    pub collection: Value,
    pub cookies: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptOutput {
    pub value: String,
    pub logs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptErrorKind {
    Cancelled,
    Timeout,
    OperationLimit,
    ResourceLimit,
    Compile,
    Runtime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptError {
    pub kind: ScriptErrorKind,
    pub message: String,
}

impl Display for ScriptError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ScriptError {}

#[derive(Clone, Debug, Default)]
pub struct ScriptRuntime;

impl ScriptRuntime {
    pub fn execute(
        &self,
        source: &str,
        context: &ScriptContext,
        limits: &ScriptLimits,
        cancellation: CancellationToken,
    ) -> Result<ScriptOutput, ScriptError> {
        validate_limits(source, limits)?;

        let started = Instant::now();
        let logs = Arc::new(Mutex::new(Vec::new()));
        let mut engine = Engine::new_raw();
        engine.set_max_operations(limits.maximum_operations);
        engine.set_max_string_size(limits.maximum_string_bytes);
        engine.set_max_array_size(limits.maximum_array_items);
        engine.set_max_map_size(limits.maximum_map_items);
        engine.set_max_call_levels(64);
        engine.set_max_expr_depths(64, 32);

        let progress_cancellation = cancellation.clone();
        let timeout = limits.timeout;
        engine.on_progress(move |_| {
            if progress_cancellation.is_cancelled() || started.elapsed() >= timeout {
                Some(Dynamic::UNIT)
            } else {
                None
            }
        });

        let log_output = Arc::clone(&logs);
        let maximum_log_entries = limits.maximum_log_entries;
        engine.register_fn("log", move |value: Dynamic| {
            if let Ok(mut entries) = log_output.lock()
                && entries.len() < maximum_log_entries
            {
                entries.push(value.to_string());
            }
        });

        let mut scope = Scope::new();
        scope.push_constant("request", json_to_dynamic(&context.request));
        scope.push_constant("response", json_to_dynamic(&context.response));
        scope.push_constant("variables", json_to_dynamic(&context.variables));
        scope.push_constant("environment", json_to_dynamic(&context.environment));
        scope.push_constant("collection", json_to_dynamic(&context.collection));
        scope.push_constant("cookies", json_to_dynamic(&context.cookies));

        let result = engine
            .eval_with_scope::<Dynamic>(&mut scope, source)
            .map_err(|error| classify_error(error, started, limits.timeout, &cancellation))?;
        let logs = logs
            .lock()
            .map_err(|_| ScriptError {
                kind: ScriptErrorKind::Runtime,
                message: "script log buffer is unavailable".to_owned(),
            })?
            .clone();

        Ok(ScriptOutput {
            value: result.to_string(),
            logs,
        })
    }
}

fn validate_limits(source: &str, limits: &ScriptLimits) -> Result<(), ScriptError> {
    if source.len() > limits.maximum_string_bytes {
        return Err(ScriptError {
            kind: ScriptErrorKind::ResourceLimit,
            message: "script source exceeds the configured byte limit".to_owned(),
        });
    }
    if limits.timeout.is_zero()
        || limits.maximum_operations == 0
        || limits.maximum_string_bytes == 0
        || limits.maximum_array_items == 0
        || limits.maximum_map_items == 0
    {
        return Err(ScriptError {
            kind: ScriptErrorKind::ResourceLimit,
            message: "script limits must be non-zero".to_owned(),
        });
    }
    Ok(())
}

fn classify_error(
    error: Box<EvalAltResult>,
    started: Instant,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> ScriptError {
    let text = error.to_string();
    let (kind, message) = if cancellation.is_cancelled() {
        (
            ScriptErrorKind::Cancelled,
            "script execution was cancelled".to_owned(),
        )
    } else if started.elapsed() >= timeout {
        (
            ScriptErrorKind::Timeout,
            "script execution exceeded its time limit".to_owned(),
        )
    } else if text.contains("Too many operations") {
        (
            ScriptErrorKind::OperationLimit,
            "script exceeded its operation limit".to_owned(),
        )
    } else if text.contains("Syntax error") || text.contains("Parse error") {
        (ScriptErrorKind::Compile, text)
    } else if text.contains("exceeds") || text.contains("too large") {
        (ScriptErrorKind::ResourceLimit, text)
    } else {
        (ScriptErrorKind::Runtime, text)
    };
    ScriptError { kind, message }
}

fn json_to_dynamic(value: &Value) -> Dynamic {
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(value) => Dynamic::from_bool(*value),
        Value::Number(value) => value.as_i64().map_or_else(
            || Dynamic::from_float(value.as_f64().unwrap_or_default()),
            Dynamic::from_int,
        ),
        Value::String(value) => Dynamic::from(value.clone()),
        Value::Array(values) => Dynamic::from_array(values.iter().map(json_to_dynamic).collect()),
        Value::Object(values) => {
            let map: Map = values
                .iter()
                .map(|(key, value)| (key.clone().into(), json_to_dynamic(value)))
                .collect();
            Dynamic::from_map(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exposes_only_explicit_context_and_bounded_logs() {
        let context = ScriptContext {
            request: json!({"method": "GET"}),
            ..ScriptContext::default()
        };
        let output = ScriptRuntime
            .execute(
                "log(request.method); log(2); log(3); request.method",
                &context,
                &ScriptLimits {
                    maximum_log_entries: 2,
                    ..ScriptLimits::default()
                },
                CancellationToken::default(),
            )
            .expect("script executes");
        assert_eq!(output.value, "GET");
        assert_eq!(output.logs, vec!["GET", "2"]);
    }

    #[test]
    fn ambient_os_apis_are_not_registered() {
        for source in [
            "open(\"/etc/passwd\")",
            "exec(\"id\")",
            "fetch(\"https://example.com\")",
        ] {
            let error = ScriptRuntime
                .execute(
                    source,
                    &ScriptContext::default(),
                    &ScriptLimits::default(),
                    CancellationToken::default(),
                )
                .expect_err("ambient capability must be unavailable");
            assert_eq!(error.kind, ScriptErrorKind::Runtime);
        }
    }

    #[test]
    fn operation_limit_stops_infinite_loop() {
        let error = ScriptRuntime
            .execute(
                "while true {}",
                &ScriptContext::default(),
                &ScriptLimits {
                    maximum_operations: 100,
                    ..ScriptLimits::default()
                },
                CancellationToken::default(),
            )
            .expect_err("loop must stop");
        assert_eq!(error.kind, ScriptErrorKind::OperationLimit);
    }

    #[test]
    fn cancellation_is_observed() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let error = ScriptRuntime
            .execute(
                "while true {}",
                &ScriptContext::default(),
                &ScriptLimits::default(),
                cancellation,
            )
            .expect_err("cancelled script must stop");
        assert_eq!(error.kind, ScriptErrorKind::Cancelled);
    }
}
