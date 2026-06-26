use super::{
    CURRENT_SCHEMA_VERSION, FileFingerprint, LoadedDocument, WorkspaceError, WorkspaceRepository,
    append_unknown_fields, atomic_write_checked, detect_conflict_error, parse_assignment,
    parse_sensitivity, parse_string, parse_u32, quote, read_limited, sensitivity_name,
};
use apex_domain::{StableId, ValueSensitivity, VariableDefinition, VariableValue};
use apex_secrets::{SecretRef, SecretStoreChain};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const MAXIMUM_VARIABLE_SET_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum StoredVariableSource {
    Literal(VariableValue),
    Secret(SecretRef),
    ProcessEnvironment { name: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredVariable {
    pub name: String,
    pub source: StoredVariableSource,
    pub sensitivity: ValueSensitivity,
    pub enabled: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableSetDocument {
    pub schema_version: u32,
    pub id: StableId,
    pub name: String,
    pub variables: Vec<StoredVariable>,
    pub unknown_fields: BTreeMap<String, String>,
}

impl VariableSetDocument {
    pub fn new(id: StableId, name: impl Into<String>) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id,
            name: name.into(),
            variables: Vec::new(),
            unknown_fields: BTreeMap::new(),
        }
    }

    pub fn resolve(
        &self,
        secret_stores: Option<&SecretStoreChain>,
    ) -> Result<BTreeMap<String, VariableDefinition>, WorkspaceError> {
        let mut resolved = BTreeMap::new();
        for variable in &self.variables {
            validate_variable_name(&variable.name)?;
            let (value, source_description, source_sensitivity) = match &variable.source {
                StoredVariableSource::Literal(value) => {
                    if variable.sensitivity == ValueSensitivity::Secret {
                        return Err(WorkspaceError::SecretResolution(format!(
                            "variable '{}' is marked secret but contains a literal value",
                            variable.name
                        )));
                    }
                    (
                        value.clone(),
                        "workspace variable file".to_owned(),
                        variable.sensitivity,
                    )
                }
                StoredVariableSource::Secret(reference) => {
                    let stores = secret_stores.ok_or_else(|| {
                        WorkspaceError::SecretResolution(format!(
                            "secret store is unavailable for {}",
                            reference.display_name()
                        ))
                    })?;
                    let secret = stores
                        .resolve(reference)
                        .map_err(|error| WorkspaceError::SecretResolution(error.to_string()))?;
                    let value = secret
                        .value
                        .expose()
                        .map_err(|error| WorkspaceError::SecretResolution(error.to_string()))?;
                    (
                        VariableValue::String(value.to_owned()),
                        format!(
                            "secret {} from {}",
                            reference.display_name(),
                            secret.source_store
                        ),
                        ValueSensitivity::Secret,
                    )
                }
                StoredVariableSource::ProcessEnvironment { name } => {
                    validate_process_environment_name(name)?;
                    let value = env::var(name).map_err(|_| {
                        WorkspaceError::SecretResolution(format!(
                            "process environment variable {name} is unavailable"
                        ))
                    })?;
                    (
                        VariableValue::String(value),
                        format!("process environment {name}"),
                        variable.sensitivity,
                    )
                }
            };
            let description = variable
                .description
                .as_ref()
                .map_or(source_description.clone(), |description| {
                    format!("{description} ({source_description})")
                });
            if resolved
                .insert(
                    variable.name.clone(),
                    VariableDefinition {
                        value,
                        sensitivity: strongest_sensitivity(
                            variable.sensitivity,
                            source_sensitivity,
                        ),
                        enabled: variable.enabled,
                        description: Some(description),
                    },
                )
                .is_some()
            {
                return Err(WorkspaceError::InvalidFormat(format!(
                    "duplicate variable name: {}",
                    variable.name
                )));
            }
        }
        Ok(resolved)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentSummary {
    pub id: StableId,
    pub name: String,
    pub path: PathBuf,
    pub variable_count: usize,
}

impl WorkspaceRepository {
    pub fn workspace_variables_path(&self) -> PathBuf {
        self.root.join("variables.toml")
    }

    pub fn environment_path(&self, id: &StableId) -> PathBuf {
        self.root
            .join("environments")
            .join(format!("{}.toml", id.as_str()))
    }

    pub fn local_environment_override_path(&self, id: &StableId) -> PathBuf {
        self.root
            .join(".apex")
            .join("environments")
            .join(format!("{}.local.toml", id.as_str()))
    }

    pub fn load_workspace_variables(
        &self,
    ) -> Result<Option<LoadedDocument<VariableSetDocument>>, WorkspaceError> {
        let path = self.workspace_variables_path();
        if !path.exists() {
            return Ok(None);
        }
        self.load_variable_set(&path).map(Some)
    }

    pub fn load_environment(
        &self,
        id: &StableId,
    ) -> Result<LoadedDocument<VariableSetDocument>, WorkspaceError> {
        self.load_variable_set(&self.environment_path(id))
    }

    pub fn load_local_environment_override(
        &self,
        id: &StableId,
    ) -> Result<Option<LoadedDocument<VariableSetDocument>>, WorkspaceError> {
        let path = self.local_environment_override_path(id);
        if !path.exists() {
            return Ok(None);
        }
        self.load_variable_set(&path).map(Some)
    }

    pub fn load_variable_set(
        &self,
        path: &Path,
    ) -> Result<LoadedDocument<VariableSetDocument>, WorkspaceError> {
        let bytes = read_limited(path, MAXIMUM_VARIABLE_SET_BYTES)?;
        let content = std::str::from_utf8(&bytes)
            .map_err(|_| WorkspaceError::InvalidUtf8(path.to_owned()))?;
        detect_conflict_error(path, content)?;
        let value = parse_variable_set(content)?;
        Ok(LoadedDocument {
            value,
            path: path.to_owned(),
            fingerprint: FileFingerprint::from_bytes(&bytes),
        })
    }

    pub fn save_variable_set(
        &self,
        path: &Path,
        document: &VariableSetDocument,
        expected: Option<FileFingerprint>,
    ) -> Result<FileFingerprint, WorkspaceError> {
        let content = format_variable_set(document)?;
        atomic_write_checked(path, content.as_bytes(), expected)
    }

    pub fn list_environments(&self) -> Result<Vec<EnvironmentSummary>, WorkspaceError> {
        let directory = self.root.join("environments");
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut summaries = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("toml")
            {
                continue;
            }
            let loaded = self.load_variable_set(&path)?;
            summaries.push(EnvironmentSummary {
                id: loaded.value.id,
                name: loaded.value.name,
                path,
                variable_count: loaded.value.variables.len(),
            });
        }
        summaries.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(summaries)
    }
}

pub fn format_variable_set(document: &VariableSetDocument) -> Result<String, WorkspaceError> {
    if document.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(WorkspaceError::UnsupportedSchemaVersion {
            found: document.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    let mut names = BTreeSet::new();
    let mut output = String::new();
    output.push_str(&format!("schema_version = {}\n", document.schema_version));
    output.push_str(&format!("id = {}\n", quote(document.id.as_str())));
    output.push_str(&format!("name = {}\n", quote(&document.name)));
    append_unknown_fields(&mut output, &document.unknown_fields);

    for variable in &document.variables {
        validate_variable_name(&variable.name)?;
        if !names.insert(variable.name.clone()) {
            return Err(WorkspaceError::InvalidFormat(format!(
                "duplicate variable name: {}",
                variable.name
            )));
        }
        if matches!(variable.source, StoredVariableSource::Literal(_))
            && variable.sensitivity == ValueSensitivity::Secret
        {
            return Err(WorkspaceError::SecretResolution(format!(
                "variable '{}' is marked secret but contains a literal value",
                variable.name
            )));
        }
        if matches!(variable.source, StoredVariableSource::Secret(_))
            && variable.sensitivity != ValueSensitivity::Secret
        {
            return Err(WorkspaceError::InvalidFormat(format!(
                "variable '{}' uses a secret source and must use secret sensitivity",
                variable.name
            )));
        }
        output.push_str("\n[[variables]]\n");
        output.push_str(&format!("name = {}\n", quote(&variable.name)));
        output.push_str(&format!("enabled = {}\n", variable.enabled));
        output.push_str(&format!(
            "sensitivity = {}\n",
            quote(sensitivity_name(variable.sensitivity))
        ));
        if let Some(description) = &variable.description {
            output.push_str(&format!("description = {}\n", quote(description)));
        }
        match &variable.source {
            StoredVariableSource::Literal(value) => {
                output.push_str("source = \"literal\"\n");
                format_literal_value(&mut output, value)?;
            }
            StoredVariableSource::Secret(reference) => {
                output.push_str("source = \"secret\"\n");
                output.push_str(&format!(
                    "secret_namespace = {}\n",
                    quote(&reference.namespace)
                ));
                output.push_str(&format!("secret_name = {}\n", quote(&reference.name)));
            }
            StoredVariableSource::ProcessEnvironment { name } => {
                validate_process_environment_name(name)?;
                output.push_str("source = \"process_environment\"\n");
                output.push_str(&format!("environment_name = {}\n", quote(name)));
            }
        }
    }
    Ok(output)
}

pub fn parse_variable_set(input: &str) -> Result<VariableSetDocument, WorkspaceError> {
    let mut root = BTreeMap::new();
    let mut raw_variables = Vec::new();
    let mut current: Option<BTreeMap<String, String>> = None;
    for (line_index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[[variables]]" {
            if let Some(values) = current.take() {
                raw_variables.push(values);
            }
            current = Some(BTreeMap::new());
            continue;
        }
        if trimmed.starts_with('[') {
            return Err(WorkspaceError::InvalidFormat(format!(
                "unsupported variable-set section on line {}",
                line_index + 1
            )));
        }
        let (key, value) = parse_assignment(trimmed, line_index + 1)?;
        if let Some(current) = current.as_mut() {
            current.insert(key, value);
        } else {
            root.insert(key, value);
        }
    }
    if let Some(values) = current {
        raw_variables.push(values);
    }

    let schema_version = parse_u32(required(&root, "schema_version")?, "schema_version")?;
    if schema_version != CURRENT_SCHEMA_VERSION {
        return Err(WorkspaceError::UnsupportedSchemaVersion {
            found: schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    let id = StableId::parse(parse_string(required(&root, "id")?)?)
        .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
    let name = parse_string(required(&root, "name")?)?;
    let unknown_fields = root
        .into_iter()
        .filter(|(key, _)| !matches!(key.as_str(), "schema_version" | "id" | "name"))
        .collect();
    let mut variables = Vec::with_capacity(raw_variables.len());
    let mut names = BTreeSet::new();
    for values in raw_variables {
        let variable = parse_stored_variable(&values)?;
        if !names.insert(variable.name.clone()) {
            return Err(WorkspaceError::InvalidFormat(format!(
                "duplicate variable name: {}",
                variable.name
            )));
        }
        variables.push(variable);
    }
    Ok(VariableSetDocument {
        schema_version,
        id,
        name,
        variables,
        unknown_fields,
    })
}

fn parse_stored_variable(
    values: &BTreeMap<String, String>,
) -> Result<StoredVariable, WorkspaceError> {
    let name = parse_string(required(values, "name")?)?;
    validate_variable_name(&name)?;
    let enabled = values
        .get("enabled")
        .map_or(Ok(true), |value| parse_bool(value, "enabled"))?;
    let sensitivity = values
        .get("sensitivity")
        .map_or(Ok(ValueSensitivity::Public), |value| {
            parse_string(value).and_then(|value| parse_sensitivity(&value))
        })?;
    let description = values
        .get("description")
        .map(|value| parse_string(value))
        .transpose()?;
    let source = values
        .get("source")
        .map(|value| parse_string(value))
        .transpose()?
        .unwrap_or_else(|| "literal".to_owned());
    let source = match source.as_str() {
        "literal" => {
            if sensitivity == ValueSensitivity::Secret {
                return Err(WorkspaceError::SecretResolution(format!(
                    "variable '{name}' is marked secret but contains a literal value"
                )));
            }
            StoredVariableSource::Literal(parse_literal_value(values)?)
        }
        "secret" => {
            if sensitivity != ValueSensitivity::Secret {
                return Err(WorkspaceError::InvalidFormat(format!(
                    "variable '{name}' uses a secret source and must use secret sensitivity"
                )));
            }
            let namespace = parse_string(required(values, "secret_namespace")?)?;
            let secret_name = parse_string(required(values, "secret_name")?)?;
            StoredVariableSource::Secret(
                SecretRef::new(namespace, secret_name)
                    .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?,
            )
        }
        "process_environment" => {
            let environment_name = parse_string(required(values, "environment_name")?)?;
            validate_process_environment_name(&environment_name)?;
            StoredVariableSource::ProcessEnvironment {
                name: environment_name,
            }
        }
        other => {
            return Err(WorkspaceError::InvalidFormat(format!(
                "unsupported variable source: {other}"
            )));
        }
    };
    Ok(StoredVariable {
        name,
        source,
        sensitivity,
        enabled,
        description,
    })
}

fn format_literal_value(output: &mut String, value: &VariableValue) -> Result<(), WorkspaceError> {
    match value {
        VariableValue::Null => output.push_str("value_kind = \"null\"\n"),
        VariableValue::Bool(value) => {
            output.push_str("value_kind = \"bool\"\n");
            output.push_str(&format!("value = {value}\n"));
        }
        VariableValue::Number(value) => {
            if !value.is_finite() {
                return Err(WorkspaceError::InvalidFormat(
                    "variable numbers must be finite".to_owned(),
                ));
            }
            output.push_str("value_kind = \"number\"\n");
            output.push_str(&format!("value = {value}\n"));
        }
        VariableValue::String(value) => {
            output.push_str("value_kind = \"string\"\n");
            output.push_str(&format!("value = {}\n", quote(value)));
        }
        VariableValue::Object(_) | VariableValue::Array(_) => {
            output.push_str("value_kind = \"json\"\n");
            let json = serde_json::to_string(&variable_value_to_json(value)?)
                .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
            output.push_str(&format!("value = {}\n", quote(&json)));
        }
    }
    Ok(())
}

fn parse_literal_value(values: &BTreeMap<String, String>) -> Result<VariableValue, WorkspaceError> {
    let kind = values
        .get("value_kind")
        .map(|value| parse_string(value))
        .transpose()?
        .unwrap_or_else(|| "string".to_owned());
    match kind.as_str() {
        "null" => Ok(VariableValue::Null),
        "bool" => Ok(VariableValue::Bool(parse_bool(
            required(values, "value")?,
            "value",
        )?)),
        "number" => required(values, "value")?.parse::<f64>().map_or_else(
            |_| {
                Err(WorkspaceError::InvalidFormat(
                    "variable number is invalid".to_owned(),
                ))
            },
            |value| {
                if value.is_finite() {
                    Ok(VariableValue::Number(value))
                } else {
                    Err(WorkspaceError::InvalidFormat(
                        "variable numbers must be finite".to_owned(),
                    ))
                }
            },
        ),
        "string" => Ok(VariableValue::String(parse_string(required(
            values, "value",
        )?)?)),
        "json" => {
            let json = parse_string(required(values, "value")?)?;
            let value: serde_json::Value = serde_json::from_str(&json)
                .map_err(|error| WorkspaceError::InvalidFormat(error.to_string()))?;
            json_to_variable_value(value)
        }
        other => Err(WorkspaceError::InvalidFormat(format!(
            "unsupported variable value kind: {other}"
        ))),
    }
}

fn variable_value_to_json(value: &VariableValue) -> Result<serde_json::Value, WorkspaceError> {
    match value {
        VariableValue::Null => Ok(serde_json::Value::Null),
        VariableValue::Bool(value) => Ok(serde_json::Value::Bool(*value)),
        VariableValue::Number(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| {
                WorkspaceError::InvalidFormat("variable numbers must be finite".to_owned())
            }),
        VariableValue::String(value) => Ok(serde_json::Value::String(value.clone())),
        VariableValue::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), variable_value_to_json(value)?)))
            .collect::<Result<serde_json::Map<_, _>, WorkspaceError>>()
            .map(serde_json::Value::Object),
        VariableValue::Array(values) => values
            .iter()
            .map(variable_value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
    }
}

fn json_to_variable_value(value: serde_json::Value) -> Result<VariableValue, WorkspaceError> {
    match value {
        serde_json::Value::Null => Ok(VariableValue::Null),
        serde_json::Value::Bool(value) => Ok(VariableValue::Bool(value)),
        serde_json::Value::Number(value) => {
            value.as_f64().map(VariableValue::Number).ok_or_else(|| {
                WorkspaceError::InvalidFormat("JSON number cannot be represented".to_owned())
            })
        }
        serde_json::Value::String(value) => Ok(VariableValue::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_to_variable_value)
            .collect::<Result<Vec<_>, _>>()
            .map(VariableValue::Array),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, json_to_variable_value(value)?)))
            .collect::<Result<BTreeMap<_, _>, WorkspaceError>>()
            .map(VariableValue::Object),
    }
}

fn parse_bool(value: &str, key: &str) -> Result<bool, WorkspaceError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(WorkspaceError::InvalidFormat(format!(
            "{key} must be true or false"
        ))),
    }
}

fn required<'a>(
    values: &'a BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, WorkspaceError> {
    values
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| WorkspaceError::InvalidFormat(format!("missing required field: {key}")))
}

fn validate_variable_name(name: &str) -> Result<(), WorkspaceError> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidFormat(format!(
            "invalid variable name: {name}"
        )))
    }
}

fn validate_process_environment_name(name: &str) -> Result<(), WorkspaceError> {
    let valid = !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidFormat(format!(
            "invalid process environment variable name: {name}"
        )))
    }
}

fn strongest_sensitivity(left: ValueSensitivity, right: ValueSensitivity) -> ValueSensitivity {
    match (left, right) {
        (ValueSensitivity::Secret, _) | (_, ValueSensitivity::Secret) => ValueSensitivity::Secret,
        (ValueSensitivity::Sensitive, _) | (_, ValueSensitivity::Sensitive) => {
            ValueSensitivity::Sensitive
        }
        _ => ValueSensitivity::Public,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_secrets::{SecretStore, SecretValue, SessionSecretStore};
    use std::sync::Arc;

    fn variable_set() -> VariableSetDocument {
        let mut nested = BTreeMap::new();
        nested.insert(
            "host".to_owned(),
            VariableValue::String("api.example.test".to_owned()),
        );
        VariableSetDocument {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: StableId::parse("development").expect("valid id"),
            name: "Development".to_owned(),
            variables: vec![
                StoredVariable {
                    name: "service".to_owned(),
                    source: StoredVariableSource::Literal(VariableValue::Object(nested)),
                    sensitivity: ValueSensitivity::Public,
                    enabled: true,
                    description: Some("Service configuration".to_owned()),
                },
                StoredVariable {
                    name: "token".to_owned(),
                    source: StoredVariableSource::Secret(
                        SecretRef::new("development", "token").expect("valid secret ref"),
                    ),
                    sensitivity: ValueSensitivity::Secret,
                    enabled: true,
                    description: None,
                },
            ],
            unknown_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn variable_set_round_trip_preserves_nested_values_and_secret_references() {
        let original = variable_set();
        let formatted = format_variable_set(&original).expect("formats");
        assert!(!formatted.contains("top-secret"));
        assert_eq!(parse_variable_set(&formatted).expect("parses"), original);
    }

    #[test]
    fn resolves_secret_references_without_persisting_values() {
        let store = Arc::new(SessionSecretStore::default());
        let reference = SecretRef::new("development", "token").expect("valid ref");
        store
            .put(&reference, SecretValue::new(b"top-secret".to_vec()))
            .expect("stores secret");
        let mut chain = SecretStoreChain::default();
        chain.push(store);
        let resolved = variable_set().resolve(Some(&chain)).expect("resolves");
        assert_eq!(
            resolved["token"].value,
            VariableValue::String("top-secret".to_owned())
        );
        assert_eq!(resolved["token"].sensitivity, ValueSensitivity::Secret);
    }

    #[test]
    fn rejects_secret_source_without_secret_sensitivity() {
        let input = r#"schema_version = 1
id = "development"
name = "Development"

[[variables]]
name = "token"
enabled = true
sensitivity = "public"
source = "secret"
secret_namespace = "development"
secret_name = "token"
"#;
        let error = parse_variable_set(input).expect_err("secret source must remain secret");
        assert!(matches!(error, WorkspaceError::InvalidFormat(_)));
    }

    #[test]
    fn rejects_plaintext_secret_literals() {
        let input = r#"schema_version = 1
id = "development"
name = "Development"

[[variables]]
name = "token"
enabled = true
sensitivity = "secret"
source = "literal"
value_kind = "string"
value = "do-not-store"
"#;
        let error = parse_variable_set(input).expect_err("must reject plaintext secret");
        assert!(matches!(error, WorkspaceError::SecretResolution(_)));
    }
}
