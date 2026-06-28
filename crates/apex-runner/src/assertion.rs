use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct AssertionContext {
    pub status: Option<u16>,
    pub elapsed: Option<Duration>,
    pub headers: Vec<(String, String)>,
    pub cookies: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub json: Option<Value>,
    pub graphql_errors: Option<usize>,
    pub grpc_status: Option<i32>,
    pub stream_event_count: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Assertion {
    StatusEquals(u16),
    TimingAtMostMillis(u64),
    HeaderEquals { name: String, value: String },
    HeaderPresent(String),
    CookieEquals { name: String, value: String },
    BodyContains(String),
    JsonPointerEquals { pointer: String, expected: Value },
    JsonType { pointer: String, expected: JsonType },
    GraphqlErrorCount(usize),
    GrpcStatus(i32),
    StreamEventCountAtLeast(usize),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum JsonType {
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum AssertionState {
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssertionResult {
    pub state: AssertionState,
    pub assertion: Assertion,
    pub expected: Option<Value>,
    pub actual: Option<Value>,
    pub message: String,
}

impl Assertion {
    pub fn evaluate(&self, context: &AssertionContext) -> AssertionResult {
        match self {
            Self::StatusEquals(expected) => match context.status {
                Some(actual) => {
                    compare_number(self, u64::from(*expected), u64::from(actual), "status")
                }
                None => skipped(self, "status is unavailable"),
            },
            Self::TimingAtMostMillis(expected) => match context.elapsed {
                Some(actual) => {
                    let actual = u64::try_from(actual.as_millis()).unwrap_or(u64::MAX);
                    comparison(
                        self,
                        actual <= *expected,
                        Value::from(*expected),
                        Value::from(actual),
                        "elapsed milliseconds",
                    )
                }
                None => skipped(self, "timing is unavailable"),
            },
            Self::HeaderEquals { name, value } => {
                let values: Vec<&str> = context
                    .headers
                    .iter()
                    .filter(|(header, _)| header.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.as_str())
                    .collect();
                if values.is_empty() {
                    comparison(
                        self,
                        false,
                        Value::from(value.clone()),
                        Value::Null,
                        "header value",
                    )
                } else {
                    let passed = values.iter().any(|actual| *actual == value);
                    comparison(
                        self,
                        passed,
                        Value::from(value.clone()),
                        Value::from(values.join(", ")),
                        "header value",
                    )
                }
            }
            Self::HeaderPresent(name) => comparison(
                self,
                context
                    .headers
                    .iter()
                    .any(|(header, _)| header.eq_ignore_ascii_case(name)),
                Value::Bool(true),
                Value::Bool(
                    context
                        .headers
                        .iter()
                        .any(|(header, _)| header.eq_ignore_ascii_case(name)),
                ),
                "header presence",
            ),
            Self::CookieEquals { name, value } => match context.cookies.get(name) {
                Some(actual) => comparison(
                    self,
                    actual == value,
                    Value::from(value.clone()),
                    Value::from(actual.clone()),
                    "cookie value",
                ),
                None => comparison(
                    self,
                    false,
                    Value::from(value.clone()),
                    Value::Null,
                    "cookie value",
                ),
            },
            Self::BodyContains(expected) => match std::str::from_utf8(&context.body) {
                Ok(actual) => comparison(
                    self,
                    actual.contains(expected),
                    Value::from(expected.clone()),
                    Value::from(actual.to_owned()),
                    "body substring",
                ),
                Err(_) => skipped(self, "body is not UTF-8 text"),
            },
            Self::JsonPointerEquals { pointer, expected } => match &context.json {
                Some(json) => match json.pointer(pointer) {
                    Some(actual) => comparison(
                        self,
                        actual == expected,
                        expected.clone(),
                        actual.clone(),
                        "JSON pointer value",
                    ),
                    None => comparison(
                        self,
                        false,
                        expected.clone(),
                        Value::Null,
                        "JSON pointer value",
                    ),
                },
                None => skipped(self, "JSON body is unavailable"),
            },
            Self::JsonType { pointer, expected } => match &context.json {
                Some(json) => match json.pointer(pointer) {
                    Some(actual) => {
                        let actual_type = json_type(actual);
                        comparison(
                            self,
                            actual_type == *expected,
                            Value::from(format!("{expected:?}")),
                            Value::from(format!("{actual_type:?}")),
                            "JSON type",
                        )
                    }
                    None => comparison(
                        self,
                        false,
                        Value::from(format!("{expected:?}")),
                        Value::Null,
                        "JSON type",
                    ),
                },
                None => skipped(self, "JSON body is unavailable"),
            },
            Self::GraphqlErrorCount(expected) => match context.graphql_errors {
                Some(actual) => {
                    compare_number(self, *expected as u64, actual as u64, "GraphQL error count")
                }
                None => skipped(self, "GraphQL result is unavailable"),
            },
            Self::GrpcStatus(expected) => match context.grpc_status {
                Some(actual) => comparison(
                    self,
                    actual == *expected,
                    Value::from(*expected),
                    Value::from(actual),
                    "gRPC status",
                ),
                None => skipped(self, "gRPC status is unavailable"),
            },
            Self::StreamEventCountAtLeast(expected) => match context.stream_event_count {
                Some(actual) => comparison(
                    self,
                    actual >= *expected,
                    Value::from(*expected as u64),
                    Value::from(actual as u64),
                    "stream event count",
                ),
                None => skipped(self, "stream event count is unavailable"),
            },
        }
    }
}

pub fn evaluate_all(assertions: &[Assertion], context: &AssertionContext) -> Vec<AssertionResult> {
    assertions
        .iter()
        .map(|assertion| assertion.evaluate(context))
        .collect()
}

fn compare_number(
    assertion: &Assertion,
    expected: u64,
    actual: u64,
    label: &str,
) -> AssertionResult {
    comparison(
        assertion,
        expected == actual,
        Value::from(expected),
        Value::from(actual),
        label,
    )
}

fn comparison(
    assertion: &Assertion,
    passed: bool,
    expected: Value,
    actual: Value,
    label: &str,
) -> AssertionResult {
    AssertionResult {
        state: if passed {
            AssertionState::Passed
        } else {
            AssertionState::Failed
        },
        assertion: assertion.clone(),
        expected: Some(expected),
        actual: Some(actual),
        message: if passed {
            format!("{label} matched")
        } else {
            format!("{label} did not match")
        },
    }
}

fn skipped(assertion: &Assertion, message: &str) -> AssertionResult {
    AssertionResult {
        state: AssertionState::Skipped,
        assertion: assertion.clone(),
        expected: None,
        actual: None,
        message: message.to_owned(),
    }
}

fn json_type(value: &Value) -> JsonType {
    match value {
        Value::Null => JsonType::Null,
        Value::Bool(_) => JsonType::Boolean,
        Value::Number(_) => JsonType::Number,
        Value::String(_) => JsonType::String,
        Value::Array(_) => JsonType::Array,
        Value::Object(_) => JsonType::Object,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> AssertionContext {
        AssertionContext {
            status: Some(201),
            elapsed: Some(Duration::from_millis(42)),
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            cookies: BTreeMap::from([("session".to_owned(), "abc".to_owned())]),
            body: br#"{"data":{"id":7}}"#.to_vec(),
            json: Some(json!({"data":{"id":7}})),
            graphql_errors: Some(0),
            grpc_status: Some(0),
            stream_event_count: Some(3),
        }
    }

    #[test]
    fn evaluates_protocol_neutral_assertions() {
        let assertions = vec![
            Assertion::StatusEquals(201),
            Assertion::TimingAtMostMillis(50),
            Assertion::HeaderEquals {
                name: "content-type".to_owned(),
                value: "application/json".to_owned(),
            },
            Assertion::CookieEquals {
                name: "session".to_owned(),
                value: "abc".to_owned(),
            },
            Assertion::BodyContains("data".to_owned()),
            Assertion::JsonPointerEquals {
                pointer: "/data/id".to_owned(),
                expected: json!(7),
            },
            Assertion::JsonType {
                pointer: "/data".to_owned(),
                expected: JsonType::Object,
            },
            Assertion::GraphqlErrorCount(0),
            Assertion::GrpcStatus(0),
            Assertion::StreamEventCountAtLeast(2),
        ];
        assert!(
            evaluate_all(&assertions, &context())
                .iter()
                .all(|result| result.state == AssertionState::Passed)
        );
    }

    #[test]
    fn failures_preserve_expected_and_actual() {
        let result = Assertion::StatusEquals(200).evaluate(&context());
        assert_eq!(result.state, AssertionState::Failed);
        assert_eq!(result.expected, Some(json!(200)));
        assert_eq!(result.actual, Some(json!(201)));
    }

    #[test]
    fn unavailable_protocol_data_is_skipped() {
        let result = Assertion::GrpcStatus(0).evaluate(&AssertionContext::default());
        assert_eq!(result.state, AssertionState::Skipped);
    }
}
