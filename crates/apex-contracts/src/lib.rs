#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct Diagnostic {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct Operation {
    pub method: String,
    pub path: String,
    pub operation_id: Option<String>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GeneratedRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Value>,
    pub generated_fields: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ContractDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct OpenApiDocument {
    root: Value,
}

impl OpenApiDocument {
    pub fn parse(bytes: &[u8], maximum_bytes: usize) -> Result<Self, Vec<Diagnostic>> {
        if bytes.len() > maximum_bytes {
            return Err(vec![Diagnostic {
                path: "$".into(),
                message: "specification exceeds configured byte limit".into(),
            }]);
        }
        let root: Value = serde_json::from_slice(bytes)
            .or_else(|_| serde_yaml::from_slice(bytes))
            .map_err(|error| {
                vec![Diagnostic {
                    path: "$".into(),
                    message: format!("invalid OpenAPI JSON/YAML: {error}"),
                }]
            })?;
        let mut diagnostics = Vec::new();
        let version = root
            .get("openapi")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !(version.starts_with("3.0.") || version.starts_with("3.1.")) {
            diagnostics.push(Diagnostic {
                path: "$.openapi".into(),
                message: "only OpenAPI 3.0 and 3.1 are supported".into(),
            });
        }
        if root.get("paths").and_then(Value::as_object).is_none() {
            diagnostics.push(Diagnostic {
                path: "$.paths".into(),
                message: "paths must be an object".into(),
            });
        }
        if diagnostics.is_empty() {
            Ok(Self { root })
        } else {
            Err(diagnostics)
        }
    }

    pub fn operations(&self) -> Vec<Operation> {
        let mut result = Vec::new();
        let methods = [
            "get", "put", "post", "delete", "patch", "head", "options", "trace",
        ];
        if let Some(paths) = self.root.get("paths").and_then(Value::as_object) {
            for (path, item) in paths {
                if let Some(item) = item.as_object() {
                    for method in methods {
                        if let Some(operation) = item.get(method).and_then(Value::as_object) {
                            result.push(Operation {
                                method: method.to_ascii_uppercase(),
                                path: path.clone(),
                                operation_id: operation
                                    .get("operationId")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                summary: operation
                                    .get("summary")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                            });
                        }
                    }
                }
            }
        }
        result.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
        result
    }

    pub fn generate_request(
        &self,
        operation_id: &str,
        server: Option<&str>,
    ) -> Result<GeneratedRequest, String> {
        let operation = self
            .operations()
            .into_iter()
            .find(|op| op.operation_id.as_deref() == Some(operation_id))
            .ok_or_else(|| format!("operation '{operation_id}' was not found"))?;
        let base = server
            .map(str::to_owned)
            .or_else(|| {
                self.root
                    .pointer("/servers/0/url")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "http://localhost".into());
        Ok(GeneratedRequest {
            method: operation.method,
            url: format!("{}{}", base.trim_end_matches('/'), operation.path),
            headers: BTreeMap::new(),
            body: None,
            generated_fields: BTreeSet::from(["method".into(), "url".into()]),
        })
    }

    pub fn validate_json(schema: &Value, value: &Value, path: &str) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Some(expected) = schema.get("type").and_then(Value::as_str) {
            let valid = match expected {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "integer" => value.as_i64().is_some(),
                "number" => value.is_number(),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                _ => true,
            };
            if !valid {
                out.push(Diagnostic {
                    path: path.into(),
                    message: format!("expected {expected}"),
                });
            }
        }
        if let (Some(required), Some(object)) = (
            schema.get("required").and_then(Value::as_array),
            value.as_object(),
        ) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    out.push(Diagnostic {
                        path: format!("{path}.{key}"),
                        message: "required property is missing".into(),
                    });
                }
            }
        }
        out
    }

    pub fn markdown(&self) -> String {
        let mut out = format!(
            "# {}\n\n",
            self.root
                .pointer("/info/title")
                .and_then(Value::as_str)
                .unwrap_or("API")
        );
        for op in self.operations() {
            out.push_str(&format!(
                "## {} `{}`\n\n{}\n\n",
                op.method,
                op.path,
                op.summary.unwrap_or_default()
            ));
        }
        out
    }

    pub fn diff(&self, other: &Self) -> ContractDiff {
        let left: BTreeMap<_, _> = self
            .operations()
            .into_iter()
            .map(|op| (format!("{} {}", op.method, op.path), op))
            .collect();
        let right: BTreeMap<_, _> = other
            .operations()
            .into_iter()
            .map(|op| (format!("{} {}", op.method, op.path), op))
            .collect();
        ContractDiff {
            added: right
                .keys()
                .filter(|k| !left.contains_key(*k))
                .cloned()
                .collect(),
            removed: left
                .keys()
                .filter(|k| !right.contains_key(*k))
                .cloned()
                .collect(),
            changed: left
                .iter()
                .filter_map(|(k, v)| right.get(k).filter(|r| *r != v).map(|_| k.clone()))
                .collect(),
        }
    }

    pub fn raw(&self) -> &Value {
        &self.root
    }
}

pub fn resolve_local_reference(
    root: &Path,
    reference: &str,
    maximum_bytes: usize,
) -> Result<Value, String> {
    let file = reference.split('#').next().unwrap_or_default();
    if file.is_empty() {
        return Err("local fragment references are resolved from the parsed document".into());
    }
    let relative = Path::new(file);
    if relative.is_absolute()
        || relative.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("reference escapes the specification root".into());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("invalid specification root: {e}"))?;
    let path: PathBuf = root
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("could not resolve reference: {e}"))?;
    if !path.starts_with(&root) {
        return Err("reference escapes the specification root".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("could not read reference: {e}"))?;
    if bytes.len() > maximum_bytes {
        return Err("referenced document exceeds configured byte limit".into());
    }
    serde_json::from_slice(&bytes)
        .or_else(|_| serde_yaml::from_slice(&bytes))
        .map_err(|e| format!("invalid referenced JSON/YAML: {e}"))
}

pub fn preserve_customizations(
    previous: &GeneratedRequest,
    generated: &GeneratedRequest,
) -> GeneratedRequest {
    let mut next = generated.clone();
    if !previous.generated_fields.contains("headers") {
        next.headers = previous.headers.clone();
    }
    if !previous.generated_fields.contains("body") {
        next.body = previous.body.clone();
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const SPEC: &str = r#"openapi: 3.1.0
info: {title: Pets, version: '1'}
servers: [{url: https://api.test}]
paths:
  /pets:
    get: {operationId: listPets, summary: List pets, responses: {'200': {description: ok}}}
"#;

    #[test]
    fn parses_browses_generates_and_documents() {
        let doc = OpenApiDocument::parse(SPEC.as_bytes(), 4096).expect("spec");
        assert_eq!(
            doc.operations()[0].operation_id.as_deref(),
            Some("listPets")
        );
        assert_eq!(
            doc.generate_request("listPets", None).expect("request").url,
            "https://api.test/pets"
        );
        assert!(doc.markdown().contains("GET `/pets`"));
    }
    #[test]
    fn diagnostics_and_schema_validation_are_actionable() {
        let err = OpenApiDocument::parse(b"openapi: 2.0\npaths: {}", 1024).expect_err("version");
        assert_eq!(err[0].path, "$.openapi");
        let errors = OpenApiDocument::validate_json(
            &json!({"type":"object","required":["id"]}),
            &json!({}),
            "$.body",
        );
        assert_eq!(errors[0].path, "$.body.id");
    }
    #[test]
    fn diff_and_customization_boundaries_are_stable() {
        let a = OpenApiDocument::parse(SPEC.as_bytes(), 4096).expect("a");
        let b = OpenApiDocument::parse(SPEC.replace("/pets:", "/animals:").as_bytes(), 4096)
            .expect("b");
        assert_eq!(a.diff(&b).added, vec!["GET /animals"]);
        let mut old = a.generate_request("listPets", None).expect("old");
        old.headers.insert("x-user".into(), "1".into());
        let next = preserve_customizations(&old, &old);
        assert_eq!(next.headers["x-user"], "1");
    }
    #[test]
    fn local_reference_rejects_traversal() {
        assert!(resolve_local_reference(Path::new("."), "../secret", 1024).is_err());
    }
}
