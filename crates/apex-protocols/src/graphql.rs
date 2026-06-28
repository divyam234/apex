use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum GraphqlOperationKind {
    Query,
    Mutation,
    Subscription,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GraphqlRequest {
    pub endpoint: String,
    pub query: String,
    pub operation_name: Option<String>,
    #[serde(default)]
    pub variables: Value,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub persisted_query: bool,
    pub allow_experimental_subscription: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GraphqlHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GraphqlResponse {
    pub data: Option<Value>,
    pub errors: Vec<GraphqlError>,
    pub extensions: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GraphqlError {
    pub message: String,
    #[serde(default)]
    pub path: Vec<Value>,
    pub extensions: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct SchemaField {
    pub name: String,
    pub type_name: String,
    pub deprecated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct SchemaType {
    pub name: String,
    pub kind: String,
    pub fields: Vec<SchemaField>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct IntrospectionSchema {
    pub query_type: Option<String>,
    pub mutation_type: Option<String>,
    pub subscription_type: Option<String>,
    pub types: Vec<SchemaType>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GraphqlHistoryEntry {
    pub operation_name: Option<String>,
    pub kind: GraphqlOperationKind,
    pub variables: Value,
    pub response: GraphqlResponse,
}

pub fn operation_kind(query: &str) -> Result<GraphqlOperationKind, String> {
    let stripped = strip_comments(query);
    let first = stripped
        .split_whitespace()
        .next()
        .ok_or_else(|| "GraphQL document is empty".to_owned())?;
    match first {
        "query" | "{" => Ok(GraphqlOperationKind::Query),
        "mutation" => Ok(GraphqlOperationKind::Mutation),
        "subscription" => Ok(GraphqlOperationKind::Subscription),
        _ if stripped.starts_with('{') => Ok(GraphqlOperationKind::Query),
        _ => Err(format!("unsupported GraphQL operation token '{first}'")),
    }
}

pub fn validate_request(request: &GraphqlRequest) -> Result<GraphqlOperationKind, Vec<String>> {
    let mut errors = Vec::new();
    if !(request.endpoint.starts_with("http://") || request.endpoint.starts_with("https://")) {
        errors.push("GraphQL endpoint must use http or https".to_owned());
    }
    if request.query.len() > 2 * 1024 * 1024 {
        errors.push("GraphQL document exceeds the 2 MiB limit".to_owned());
    }
    if !request.variables.is_object() && !request.variables.is_null() {
        errors.push("GraphQL variables must be an object or null".to_owned());
    }
    let kind = match operation_kind(&request.query) {
        Ok(kind) => Some(kind),
        Err(error) => {
            errors.push(error);
            None
        }
    };
    if kind == Some(GraphqlOperationKind::Subscription) && !request.allow_experimental_subscription
    {
        errors.push("GraphQL subscriptions require explicit experimental opt-in".to_owned());
    }
    if balanced_delimiters(&request.query).is_err() {
        errors.push("GraphQL document has unbalanced delimiters".to_owned());
    }
    match (errors.is_empty(), kind) {
        (true, Some(kind)) => Ok(kind),
        _ => Err(errors),
    }
}

pub fn build_http_request(request: &GraphqlRequest) -> Result<GraphqlHttpRequest, Vec<String>> {
    let kind = validate_request(request)?;
    if kind == GraphqlOperationKind::Subscription {
        return Err(vec![
            "subscriptions require a streaming transport adapter".to_owned(),
        ]);
    }
    let mut headers = request.headers.clone();
    headers
        .entry("content-type".to_owned())
        .or_insert_with(|| "application/json".to_owned());
    let mut body = Map::new();
    body.insert("query".to_owned(), Value::String(request.query.clone()));
    if let Some(operation_name) = &request.operation_name {
        body.insert(
            "operationName".to_owned(),
            Value::String(operation_name.clone()),
        );
    }
    body.insert("variables".to_owned(), request.variables.clone());
    if request.persisted_query {
        body.insert(
            "extensions".to_owned(),
            json!({
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": persisted_query_hash(&request.query)
                }
            }),
        );
    }
    Ok(GraphqlHttpRequest {
        method: "POST".to_owned(),
        url: request.endpoint.clone(),
        headers,
        body: Value::Object(body),
    })
}

pub fn parse_response(bytes: &[u8], maximum_bytes: usize) -> Result<GraphqlResponse, String> {
    if bytes.len() > maximum_bytes {
        return Err("GraphQL response exceeds configured byte limit".to_owned());
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("GraphQL response is not valid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "GraphQL response must be a JSON object".to_owned())?;
    let data = object.get("data").cloned();
    let extensions = object.get("extensions").cloned();
    let errors = object
        .get("errors")
        .map_or_else(|| Ok(Vec::new()), parse_errors)?;
    Ok(GraphqlResponse {
        data,
        errors,
        extensions,
    })
}

pub fn parse_introspection(value: &Value) -> Result<IntrospectionSchema, String> {
    let schema = value
        .pointer("/data/__schema")
        .and_then(Value::as_object)
        .ok_or_else(|| "introspection response is missing data.__schema".to_owned())?;
    let type_name = |key: &str| {
        schema
            .get(key)
            .and_then(Value::as_object)
            .and_then(|object| object.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let mut types = Vec::new();
    if let Some(entries) = schema.get("types").and_then(Value::as_array) {
        for entry in entries.iter().take(100_000) {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let Some(name) = entry.get("name").and_then(Value::as_str) else {
                continue;
            };
            let kind = entry
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN");
            let mut fields = Vec::new();
            if let Some(raw_fields) = entry.get("fields").and_then(Value::as_array) {
                for field in raw_fields.iter().take(100_000) {
                    let Some(field) = field.as_object() else {
                        continue;
                    };
                    let Some(field_name) = field.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    fields.push(SchemaField {
                        name: field_name.to_owned(),
                        type_name: render_type(field.get("type"))
                            .unwrap_or_else(|| "Unknown".to_owned()),
                        deprecated: field
                            .get("isDeprecated")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    });
                }
            }
            types.push(SchemaType {
                name: name.to_owned(),
                kind: kind.to_owned(),
                fields,
            });
        }
    }
    types.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(IntrospectionSchema {
        query_type: type_name("queryType"),
        mutation_type: type_name("mutationType"),
        subscription_type: type_name("subscriptionType"),
        types,
    })
}

pub fn persisted_query_hash(query: &str) -> String {
    format!("{:x}", Sha256::digest(query.as_bytes()))
}

fn parse_errors(value: &Value) -> Result<Vec<GraphqlError>, String> {
    let values = value
        .as_array()
        .ok_or_else(|| "GraphQL errors must be an array".to_owned())?;
    let mut errors = Vec::with_capacity(values.len());
    for value in values {
        let object = value
            .as_object()
            .ok_or_else(|| "GraphQL error entry must be an object".to_owned())?;
        let message = object
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| "GraphQL error entry is missing message".to_owned())?;
        errors.push(GraphqlError {
            message: message.to_owned(),
            path: object
                .get("path")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            extensions: object.get("extensions").cloned(),
        });
    }
    Ok(errors)
}

fn render_type(value: Option<&Value>) -> Option<String> {
    let value = value?.as_object()?;
    let kind = value.get("kind")?.as_str()?;
    match kind {
        "NON_NULL" => Some(format!("{}!", render_type(value.get("ofType"))?)),
        "LIST" => Some(format!("[{}]", render_type(value.get("ofType"))?)),
        _ => value.get("name")?.as_str().map(str::to_owned),
    }
}

fn strip_comments(query: &str) -> String {
    query
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn balanced_delimiters(query: &str) -> Result<(), ()> {
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in query.chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        if character == '"' {
            quoted = true;
        } else if matches!(character, '{' | '(' | '[') {
            stack.push(character);
        } else if matches!(character, '}' | ')' | ']') {
            let expected = match character {
                '}' => '{',
                ')' => '(',
                ']' => '[',
                _ => unreachable!(),
            };
            if stack.pop() != Some(expected) {
                return Err(());
            }
        }
    }
    if stack.is_empty() && !quoted {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(query: &str) -> GraphqlRequest {
        GraphqlRequest {
            endpoint: "https://example.test/graphql".to_owned(),
            query: query.to_owned(),
            operation_name: Some("User".to_owned()),
            variables: json!({"id": 7}),
            headers: BTreeMap::new(),
            persisted_query: true,
            allow_experimental_subscription: false,
        }
    }

    #[test]
    fn builds_query_for_shared_http_engine() {
        let built = build_http_request(&request("query User($id: ID!) { user(id: $id) { id } }"))
            .expect("valid request");
        assert_eq!(built.method, "POST");
        assert_eq!(built.body["variables"]["id"], 7);
        assert_eq!(built.body["extensions"]["persistedQuery"]["version"], 1);
        assert_eq!(built.headers["content-type"], "application/json");
    }

    #[test]
    fn subscription_requires_explicit_boundary() {
        let error = validate_request(&request("subscription Events { events { id } }"))
            .expect_err("subscription must require opt-in");
        assert!(error.iter().any(|message| message.contains("experimental")));
    }

    #[test]
    fn parses_response_and_introspection_fixture() {
        let response = parse_response(br#"{"data":{"user":{"id":7}},"errors":[]}"#, 1024)
            .expect("response parses");
        assert_eq!(response.data.expect("data")["user"]["id"], 7);
        let fixture = json!({"data":{"__schema":{"queryType":{"name":"Query"},"mutationType":null,"subscriptionType":null,"types":[{"kind":"OBJECT","name":"Query","fields":[{"name":"user","isDeprecated":false,"type":{"kind":"NON_NULL","ofType":{"kind":"OBJECT","name":"User"}}}]}]}}});
        let schema = parse_introspection(&fixture).expect("schema parses");
        assert_eq!(schema.query_type.as_deref(), Some("Query"));
        assert_eq!(schema.types[0].fields[0].type_name, "User!");
    }
}
