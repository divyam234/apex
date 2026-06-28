use crate::{ImportDiagnostic, ImportError, ImportPreview, ImportSeverity, Importer};
use apex_domain::{
    Authentication, FormField, HeaderEntry, HttpMethod, HttpRequest, MultipartField,
    MultipartValue, RequestBody, RequestSettings, StableId, ValueSensitivity,
};
use apex_workspace::RequestDocument;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

const MAX_IMPORT_BYTES: usize = 32 * 1024 * 1024;
const MAX_POSTMAN_ITEMS: usize = 100_000;
const MAX_POSTMAN_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Default)]
pub struct PostmanV21Importer;

impl Importer for PostmanV21Importer {
    fn format_id(&self) -> &'static str {
        "postman-collection-v2.1"
    }

    fn preview(&self, input: &[u8]) -> Result<ImportPreview, ImportError> {
        parse_postman_v21(input)
    }
}

pub fn parse_postman_v21(input: &[u8]) -> Result<ImportPreview, ImportError> {
    if input.len() > MAX_IMPORT_BYTES {
        return Err(ImportError::InputTooLarge {
            maximum_bytes: MAX_IMPORT_BYTES,
            observed_bytes: input.len(),
        });
    }
    let root = serde_json::from_slice::<Value>(input)
        .map_err(|error| ImportError::InvalidJson(error.to_string()))?;
    let root = root.as_object().ok_or_else(|| {
        ImportError::InvalidPostmanCollection("root must be an object".to_owned())
    })?;
    validate_schema(root)?;

    let mut context = PostmanContext::default();
    report_collection_metadata(root, &mut context);
    let items = root.get("item").and_then(Value::as_array).ok_or_else(|| {
        ImportError::InvalidPostmanCollection("collection item array is missing".to_owned())
    })?;
    context.visit_items(items, "item", 0)?;
    Ok(ImportPreview {
        source_format: "postman-collection-v2.1",
        requests: context.requests,
        diagnostics: context.diagnostics,
        unsupported_fields: context.unsupported_fields.into_iter().collect(),
    })
}

#[derive(Default)]
struct PostmanContext {
    requests: Vec<RequestDocument>,
    diagnostics: Vec<ImportDiagnostic>,
    unsupported_fields: BTreeSet<String>,
    visited_items: usize,
}

impl PostmanContext {
    fn visit_items(
        &mut self,
        items: &[Value],
        source_path: &str,
        depth: usize,
    ) -> Result<(), ImportError> {
        if depth > MAX_POSTMAN_DEPTH {
            return Err(ImportError::NestingLimit {
                maximum_depth: MAX_POSTMAN_DEPTH,
            });
        }
        for (index, item) in items.iter().enumerate() {
            self.visited_items += 1;
            if self.visited_items > MAX_POSTMAN_ITEMS {
                return Err(ImportError::ItemLimit {
                    maximum_items: MAX_POSTMAN_ITEMS,
                });
            }
            let path = format!("{source_path}[{index}]");
            let object = item.as_object().ok_or_else(|| {
                ImportError::InvalidPostmanCollection(format!("{path} must be an object"))
            })?;
            if let Some(children) = object.get("item").and_then(Value::as_array) {
                self.report_folder_fields(object, &path);
                self.visit_items(children, &format!("{path}.item"), depth + 1)?;
                continue;
            }
            let request = object.get("request").ok_or_else(|| {
                ImportError::InvalidPostmanCollection(format!(
                    "{path} is neither a folder nor a request item"
                ))
            })?;
            self.report_request_item_fields(object, &path);
            let request = self.parse_request_item(object, request, &path)?;
            self.requests.push(RequestDocument::new(request));
        }
        Ok(())
    }

    fn parse_request_item(
        &mut self,
        item: &Map<String, Value>,
        request_value: &Value,
        source_path: &str,
    ) -> Result<HttpRequest, ImportError> {
        let request = request_value.as_object().ok_or_else(|| {
            ImportError::InvalidPostmanCollection(format!(
                "{source_path}.request must be an object"
            ))
        })?;
        self.report_request_fields(request, &format!("{source_path}.request"));
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Imported Postman request")
            .to_owned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET");
        let method = HttpMethod::parse(method)
            .map_err(|error| ImportError::InvalidMethod(error.to_string()))?;
        let (url, query) = parse_url(request.get("url"), &format!("{source_path}.request.url"))?;
        let headers = parse_headers(
            request.get("header"),
            &format!("{source_path}.request.header"),
        )?;
        let body = self.parse_body(
            request.get("body"),
            &headers,
            &format!("{source_path}.request.body"),
        )?;
        let documentation = request
            .get("description")
            .or_else(|| item.get("description"))
            .and_then(description_text)
            .unwrap_or_default();
        let identifier = format!("postman-request-{}", self.requests.len() + 1);
        Ok(HttpRequest {
            id: StableId::parse(identifier)
                .map_err(|error| ImportError::InvalidPostmanCollection(error.to_string()))?,
            name,
            method,
            url,
            query,
            headers,
            authentication: Authentication::None,
            body,
            settings: RequestSettings::default(),
            documentation,
        })
    }

    fn parse_body(
        &mut self,
        value: Option<&Value>,
        headers: &[HeaderEntry],
        source_path: &str,
    ) -> Result<RequestBody, ImportError> {
        let Some(value) = value else {
            return Ok(RequestBody::Empty);
        };
        let body = value.as_object().ok_or_else(|| {
            ImportError::InvalidPostmanCollection(format!("{source_path} must be an object"))
        })?;
        let mode = body.get("mode").and_then(Value::as_str).unwrap_or("raw");
        report_unknown_keys(
            body,
            &[
                "mode",
                "raw",
                "urlencoded",
                "formdata",
                "file",
                "graphql",
                "options",
                "disabled",
            ],
            source_path,
            self,
        );
        if body.get("disabled").and_then(Value::as_bool) == Some(true) {
            self.warning(
                "postman.disabled-body",
                "A disabled Postman body was preserved as an empty ApexAPI body.",
                source_path,
            );
            return Ok(RequestBody::Empty);
        }
        match mode {
            "raw" => {
                let text = body
                    .get("raw")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let language = body
                    .get("options")
                    .and_then(Value::as_object)
                    .and_then(|options| options.get("raw"))
                    .and_then(Value::as_object)
                    .and_then(|raw| raw.get("language"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let content_type = headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case("content-type"))
                    .map(|header| header.value.as_str());
                if language.eq_ignore_ascii_case("json")
                    || content_type.is_some_and(|value| value.contains("json"))
                {
                    Ok(RequestBody::Json(text))
                } else if language.eq_ignore_ascii_case("xml")
                    || content_type.is_some_and(|value| value.contains("xml"))
                {
                    Ok(RequestBody::Xml(text))
                } else {
                    Ok(RequestBody::Text {
                        content_type: content_type.map(str::to_owned),
                        text,
                    })
                }
            }
            "urlencoded" => {
                let fields = body
                    .get("urlencoded")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        ImportError::InvalidPostmanCollection(format!(
                            "{source_path}.urlencoded must be an array"
                        ))
                    })?;
                Ok(RequestBody::FormUrlEncoded(
                    fields
                        .iter()
                        .enumerate()
                        .map(|(index, field)| {
                            parse_form_field(field, &format!("{source_path}.urlencoded[{index}]"))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
            "formdata" => {
                let fields = body
                    .get("formdata")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        ImportError::InvalidPostmanCollection(format!(
                            "{source_path}.formdata must be an array"
                        ))
                    })?;
                Ok(RequestBody::Multipart(
                    fields
                        .iter()
                        .enumerate()
                        .map(|(index, field)| {
                            parse_multipart_field(
                                field,
                                &format!("{source_path}.formdata[{index}]"),
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
            "file" => {
                let source = body
                    .get("file")
                    .and_then(Value::as_object)
                    .and_then(|file| file.get("src"))
                    .and_then(file_source)
                    .ok_or_else(|| {
                        ImportError::InvalidPostmanCollection(format!(
                            "{source_path}.file.src must contain a path"
                        ))
                    })?;
                Ok(RequestBody::BinaryFile {
                    relative_path: source,
                })
            }
            "graphql" => {
                let graphql = body
                    .get("graphql")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        ImportError::InvalidPostmanCollection(format!(
                            "{source_path}.graphql must be an object"
                        ))
                    })?;
                let query = graphql
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let variables_json = match graphql.get("variables") {
                    Some(Value::String(value)) => value.clone(),
                    Some(value) => serde_json::to_string(value)
                        .map_err(|error| ImportError::InvalidJson(error.to_string()))?,
                    None => "{}".to_owned(),
                };
                Ok(RequestBody::GraphQl {
                    query,
                    variables_json,
                    operation_name: None,
                })
            }
            unsupported => {
                self.unsupported(
                    format!("{source_path}.mode:{unsupported}"),
                    "postman.unsupported-body-mode",
                    format!(
                        "Postman body mode '{unsupported}' is not converted; the request body is empty."
                    ),
                    source_path,
                );
                Ok(RequestBody::Empty)
            }
        }
    }

    fn report_folder_fields(&mut self, object: &Map<String, Value>, source_path: &str) {
        for field in ["event", "auth", "variable", "protocolProfileBehavior"] {
            if object.contains_key(field) {
                self.unsupported(
                    format!("{source_path}.{field}"),
                    "postman.folder-metadata",
                    format!("Postman folder field '{field}' requires manual review."),
                    source_path,
                );
            }
        }
        report_unknown_keys(
            object,
            &[
                "name",
                "item",
                "description",
                "event",
                "auth",
                "variable",
                "protocolProfileBehavior",
                "id",
            ],
            source_path,
            self,
        );
    }

    fn report_request_item_fields(&mut self, object: &Map<String, Value>, source_path: &str) {
        for field in ["event", "response", "protocolProfileBehavior"] {
            if object.contains_key(field) {
                self.unsupported(
                    format!("{source_path}.{field}"),
                    "postman.request-item-metadata",
                    format!("Postman request item field '{field}' requires manual review."),
                    source_path,
                );
            }
        }
        report_unknown_keys(
            object,
            &[
                "name",
                "request",
                "response",
                "event",
                "description",
                "id",
                "protocolProfileBehavior",
            ],
            source_path,
            self,
        );
    }

    fn report_request_fields(&mut self, object: &Map<String, Value>, source_path: &str) {
        for field in ["auth", "proxy", "certificate"] {
            if object.contains_key(field) {
                self.unsupported(
                    format!("{source_path}.{field}"),
                    "postman.request-security-metadata",
                    format!(
                        "Postman request field '{field}' was not copied. Configure it explicitly in ApexAPI."
                    ),
                    source_path,
                );
            }
        }
        report_unknown_keys(
            object,
            &[
                "url",
                "auth",
                "proxy",
                "certificate",
                "method",
                "description",
                "header",
                "body",
            ],
            source_path,
            self,
        );
    }

    fn unsupported(
        &mut self,
        field: String,
        code: &'static str,
        message: String,
        source_path: &str,
    ) {
        self.unsupported_fields.insert(field);
        self.diagnostics.push(ImportDiagnostic {
            severity: ImportSeverity::Warning,
            code,
            message,
            source_path: Some(source_path.to_owned()),
        });
    }

    fn warning(&mut self, code: &'static str, message: &str, source_path: &str) {
        self.diagnostics.push(ImportDiagnostic {
            severity: ImportSeverity::Warning,
            code,
            message: message.to_owned(),
            source_path: Some(source_path.to_owned()),
        });
    }
}

fn validate_schema(root: &Map<String, Value>) -> Result<(), ImportError> {
    let schema = root
        .get("info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("schema"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ImportError::InvalidPostmanCollection("info.schema is missing".to_owned())
        })?;
    if schema.contains("/v2.1.0/") || schema.ends_with("/v2.1.0") {
        Ok(())
    } else {
        Err(ImportError::UnsupportedPostmanSchema(schema.to_owned()))
    }
}

fn report_collection_metadata(root: &Map<String, Value>, context: &mut PostmanContext) {
    for field in ["event", "auth", "variable", "protocolProfileBehavior"] {
        if root.contains_key(field) {
            context.unsupported(
                field.to_owned(),
                "postman.collection-metadata",
                format!("Postman collection field '{field}' requires manual review."),
                field,
            );
        }
    }
    report_unknown_keys(
        root,
        &[
            "info",
            "item",
            "event",
            "auth",
            "variable",
            "protocolProfileBehavior",
        ],
        "$",
        context,
    );
}

fn report_unknown_keys(
    object: &Map<String, Value>,
    known: &[&str],
    source_path: &str,
    context: &mut PostmanContext,
) {
    let known = known.iter().copied().collect::<BTreeSet<_>>();
    for key in object.keys() {
        if !known.contains(key.as_str()) {
            context.unsupported(
                format!("{source_path}.{key}"),
                "postman.unknown-field",
                format!("Unknown Postman field '{key}' is retained in the import report."),
                source_path,
            );
        }
    }
}

fn parse_url(
    value: Option<&Value>,
    source_path: &str,
) -> Result<(String, Vec<FormField>), ImportError> {
    match value {
        Some(Value::String(url)) => Ok((url.clone(), Vec::new())),
        Some(Value::Object(object)) => {
            if let Some(raw) = object.get("raw").and_then(Value::as_str) {
                return Ok((raw.to_owned(), Vec::new()));
            }
            let protocol = object
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("https");
            let host = string_or_array(object.get("host")).unwrap_or_default();
            let path = string_or_array(object.get("path")).unwrap_or_default();
            let mut url = format!("{protocol}://{host}");
            if !path.is_empty() {
                url.push('/');
                url.push_str(&path);
            }
            let query = object
                .get("query")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            parse_form_field(value, &format!("{source_path}.query[{index}]"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            if host.is_empty() {
                Err(ImportError::InvalidPostmanCollection(format!(
                    "{source_path} has no raw URL or host"
                )))
            } else {
                Ok((url, query))
            }
        }
        _ => Err(ImportError::InvalidPostmanCollection(format!(
            "{source_path} must be a string or object"
        ))),
    }
}

fn parse_headers(
    value: Option<&Value>,
    source_path: &str,
) -> Result<Vec<HeaderEntry>, ImportError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let headers = value.as_array().ok_or_else(|| {
        ImportError::InvalidPostmanCollection(format!("{source_path} must be an array"))
    })?;
    headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            let path = format!("{source_path}[{index}]");
            let header = header.as_object().ok_or_else(|| {
                ImportError::InvalidPostmanCollection(format!("{path} must be an object"))
            })?;
            let key = header.get("key").and_then(Value::as_str).ok_or_else(|| {
                ImportError::InvalidPostmanCollection(format!("{path}.key is missing"))
            })?;
            let value = header
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut entry = HeaderEntry::new(key, value)
                .map_err(|error| ImportError::InvalidHeader(error.to_string()))?;
            entry.enabled = header.get("disabled").and_then(Value::as_bool) != Some(true);
            Ok(entry)
        })
        .collect()
}

fn parse_form_field(value: &Value, source_path: &str) -> Result<FormField, ImportError> {
    let object = value.as_object().ok_or_else(|| {
        ImportError::InvalidPostmanCollection(format!("{source_path} must be an object"))
    })?;
    let name = object.get("key").and_then(Value::as_str).ok_or_else(|| {
        ImportError::InvalidPostmanCollection(format!("{source_path}.key is missing"))
    })?;
    let value = object
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(FormField {
        name: name.to_owned(),
        value: value.to_owned(),
        enabled: object.get("disabled").and_then(Value::as_bool) != Some(true),
        sensitivity: ValueSensitivity::Public,
    })
}

fn parse_multipart_field(value: &Value, source_path: &str) -> Result<MultipartField, ImportError> {
    let object = value.as_object().ok_or_else(|| {
        ImportError::InvalidPostmanCollection(format!("{source_path} must be an object"))
    })?;
    let name = object.get("key").and_then(Value::as_str).ok_or_else(|| {
        ImportError::InvalidPostmanCollection(format!("{source_path}.key is missing"))
    })?;
    let kind = object.get("type").and_then(Value::as_str).unwrap_or("text");
    let value = if kind == "file" {
        let path = object.get("src").and_then(file_source).ok_or_else(|| {
            ImportError::InvalidPostmanCollection(format!(
                "{source_path}.src must contain a file path"
            ))
        })?;
        MultipartValue::File {
            relative_path: path,
        }
    } else {
        MultipartValue::Text(
            object
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )
    };
    Ok(MultipartField {
        name: name.to_owned(),
        value,
        content_type: object
            .get("contentType")
            .and_then(Value::as_str)
            .map(str::to_owned),
        enabled: object.get("disabled").and_then(Value::as_bool) != Some(true),
        sensitivity: ValueSensitivity::Public,
    })
}

fn file_source(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Array(values) => values.iter().find_map(file_source),
        _ => None,
    }
}

fn string_or_array(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Array(values)) => Some(
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("/"),
        ),
        _ => None,
    }
}

fn description_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => object
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "https://schema.getpostman.com/json/collection/v2.1.0/collection.json";

    #[test]
    fn imports_nested_requests_duplicate_headers_and_json_body() {
        let input = serde_json::json!({
            "info": {"name": "Users", "schema": SCHEMA},
            "item": [{
                "name": "Folder",
                "item": [{
                    "name": "Create user",
                    "request": {
                        "method": "POST",
                        "url": {"raw": "https://api.test/users"},
                        "header": [
                            {"key": "X-Trace", "value": "one"},
                            {"key": "X-Trace", "value": "two", "disabled": true},
                            {"key": "Content-Type", "value": "application/json"}
                        ],
                        "body": {
                            "mode": "raw",
                            "raw": "{\"name\":\"Ada\"}",
                            "options": {"raw": {"language": "json"}}
                        }
                    }
                }]
            }]
        });
        let preview = parse_postman_v21(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert_eq!(preview.requests.len(), 1);
        let request = &preview.requests[0].request;
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.headers.len(), 3);
        assert!(!request.headers[1].enabled);
        assert!(matches!(&request.body, RequestBody::Json(body) if body.contains("Ada")));
    }

    #[test]
    fn scripts_auth_variables_and_examples_are_never_silently_dropped() {
        let input = serde_json::json!({
            "info": {"name": "Audit", "schema": SCHEMA},
            "event": [{"listen": "prerequest", "script": {"exec": ["console.log('x')"]}}],
            "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "secret"}]},
            "variable": [{"key": "baseUrl", "value": "https://api.test"}],
            "item": [{
                "name": "Get user",
                "event": [{"listen": "test", "script": {"exec": ["pm.test('x')"]}}],
                "response": [{"name": "Example", "code": 200}],
                "request": {
                    "method": "GET",
                    "url": "https://api.test/users/1",
                    "auth": {"type": "apikey", "apikey": [{"key": "value", "value": "secret"}]}
                }
            }]
        });
        let preview = parse_postman_v21(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert!(
            preview
                .unsupported_fields
                .iter()
                .any(|field| field == "event")
        );
        assert!(
            preview
                .unsupported_fields
                .iter()
                .any(|field| field == "auth")
        );
        assert!(
            preview
                .unsupported_fields
                .iter()
                .any(|field| field == "variable")
        );
        assert!(
            preview
                .unsupported_fields
                .iter()
                .any(|field| field.ends_with(".response"))
        );
        assert!(
            preview
                .unsupported_fields
                .iter()
                .any(|field| field.ends_with(".request.auth"))
        );
        assert_eq!(
            preview.requests[0].request.authentication,
            Authentication::None
        );
        let debug = format!("{preview:?}");
        assert!(!debug.contains("bearer\""));
        assert!(
            !preview
                .unsupported_fields
                .iter()
                .any(|field| field.contains("secret"))
        );
    }

    #[test]
    fn imports_urlencoded_multipart_and_file_bodies() {
        let input = serde_json::json!({
            "info": {"name": "Bodies", "schema": SCHEMA},
            "item": [
                {"name": "Form", "request": {"method": "POST", "url": "https://api.test/form", "body": {"mode": "urlencoded", "urlencoded": [{"key": "a", "value": "1", "disabled": true}]}}},
                {"name": "Multipart", "request": {"method": "POST", "url": "https://api.test/upload", "body": {"mode": "formdata", "formdata": [{"key": "note", "value": "hello", "type": "text"}, {"key": "file", "src": "fixtures/data.bin", "type": "file", "contentType": "application/octet-stream"}]}}},
                {"name": "File", "request": {"method": "POST", "url": "https://api.test/binary", "body": {"mode": "file", "file": {"src": ["fixtures/blob.bin"]}}}}
            ]
        });
        let preview = parse_postman_v21(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert!(
            matches!(&preview.requests[0].request.body, RequestBody::FormUrlEncoded(fields) if !fields[0].enabled)
        );
        assert!(
            matches!(&preview.requests[1].request.body, RequestBody::Multipart(fields) if matches!(&fields[1].value, MultipartValue::File { relative_path } if relative_path == "fixtures/data.bin"))
        );
        assert!(
            matches!(&preview.requests[2].request.body, RequestBody::BinaryFile { relative_path } if relative_path == "fixtures/blob.bin")
        );
    }

    #[test]
    fn rejects_non_v21_schema_and_excessive_nesting() {
        let wrong = serde_json::json!({
            "info": {"schema": "https://schema.getpostman.com/json/collection/v2.0.0/collection.json"},
            "item": []
        });
        assert!(matches!(
            parse_postman_v21(&serde_json::to_vec(&wrong).unwrap()),
            Err(ImportError::UnsupportedPostmanSchema(_))
        ));

        let mut item = serde_json::json!({"name": "Leaf", "request": {"method": "GET", "url": "https://api.test"}});
        for index in 0..=MAX_POSTMAN_DEPTH {
            item = serde_json::json!({"name": format!("Folder {index}"), "item": [item]});
        }
        let deep = serde_json::json!({"info": {"schema": SCHEMA}, "item": [item]});
        let result = parse_postman_v21(&serde_json::to_vec(&deep).unwrap());
        assert!(
            matches!(result, Err(ImportError::NestingLimit { .. })),
            "unexpected deep-collection result: {result:?}"
        );
    }
}
