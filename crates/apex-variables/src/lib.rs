#![forbid(unsafe_code)]

mod workspace;
pub use workspace::*;

use apex_domain::{
    Authentication, FormField, HttpRequest, MultipartValue, RequestBody, ValueSensitivity,
    VariableDefinition, VariableValue,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VariableScope {
    BuiltInDynamic,
    Global,
    Workspace,
    Collection,
    Folder,
    Environment,
    LocalEnvironmentOverride,
    Request,
    ScriptCreated,
    RunnerIterationData,
}

impl VariableScope {
    pub const PRECEDENCE: [Self; 10] = [
        Self::BuiltInDynamic,
        Self::Global,
        Self::Workspace,
        Self::Collection,
        Self::Folder,
        Self::Environment,
        Self::LocalEnvironmentOverride,
        Self::Request,
        Self::ScriptCreated,
        Self::RunnerIterationData,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::BuiltInDynamic => "built-in dynamic",
            Self::Global => "global",
            Self::Workspace => "workspace",
            Self::Collection => "collection",
            Self::Folder => "folder",
            Self::Environment => "environment",
            Self::LocalEnvironmentOverride => "local environment override",
            Self::Request => "request",
            Self::ScriptCreated => "script-created",
            Self::RunnerIterationData => "runner iteration data",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct VariableLayer {
    values: BTreeMap<String, VariableDefinition>,
}

impl VariableLayer {
    pub fn insert(&mut self, name: impl Into<String>, definition: VariableDefinition) {
        self.values.insert(name.into(), definition);
    }

    pub fn get(&self, name: &str) -> Option<&VariableDefinition> {
        self.values.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &VariableDefinition)> {
        self.values.iter()
    }
}

#[derive(Clone, Debug, Default)]
pub struct VariableContext {
    layers: BTreeMap<VariableScope, VariableLayer>,
}

impl VariableContext {
    pub fn layer_mut(&mut self, scope: VariableScope) -> &mut VariableLayer {
        self.layers.entry(scope).or_default()
    }

    pub fn layer(&self, scope: VariableScope) -> Option<&VariableLayer> {
        self.layers.get(&scope)
    }

    pub fn effective_definition(&self, name: &str) -> Option<(VariableScope, &VariableDefinition)> {
        VariableScope::PRECEDENCE
            .iter()
            .filter_map(|scope| {
                self.layer(*scope)
                    .and_then(|layer| layer.get(name))
                    .filter(|definition| definition.enabled)
                    .map(|definition| (*scope, definition))
            })
            .next_back()
    }

    pub fn merge(&mut self, other: Self) {
        for (scope, layer) in other.layers {
            self.layers
                .entry(scope)
                .or_default()
                .values
                .extend(layer.values);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceStep {
    pub scope: VariableScope,
    pub found: bool,
    pub enabled: bool,
    pub selected: bool,
    pub sensitivity: Option<ValueSensitivity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariableTrace {
    pub expression: String,
    pub root_name: String,
    pub selected_scope: Option<VariableScope>,
    pub steps: Vec<TraceStep>,
    pub used_default: bool,
    pub dynamic: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTemplate {
    pub value: String,
    pub traces: Vec<VariableTrace>,
    pub sensitive_values: Vec<String>,
    pub unresolved: Vec<String>,
}

pub trait DynamicVariableProvider: Send + Sync {
    fn resolve(&self, name: &str) -> Option<VariableDefinition>;
}

#[derive(Debug, Default)]
pub struct SystemDynamicVariables;

impl DynamicVariableProvider for SystemDynamicVariables {
    fn resolve(&self, name: &str) -> Option<VariableDefinition> {
        let value = match name {
            "$uuid" => generate_uuid_v4(),
            "$timestamp" => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string(),
            "$timestamp_ms" => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .to_string(),
            _ => return None,
        };
        Some(VariableDefinition {
            value: VariableValue::String(value),
            sensitivity: ValueSensitivity::Public,
            enabled: true,
            description: Some("ApexAPI built-in dynamic value".to_owned()),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolverOptions {
    pub strict_unresolved: bool,
    pub preserve_unresolved: bool,
    pub maximum_depth: usize,
}

impl Default for ResolverOptions {
    fn default() -> Self {
        Self {
            strict_unresolved: true,
            preserve_unresolved: false,
            maximum_depth: 32,
        }
    }
}

pub struct VariableResolver<'a> {
    context: &'a VariableContext,
    dynamic: &'a dyn DynamicVariableProvider,
    options: ResolverOptions,
}

impl<'a> VariableResolver<'a> {
    pub fn new(
        context: &'a VariableContext,
        dynamic: &'a dyn DynamicVariableProvider,
        options: ResolverOptions,
    ) -> Self {
        Self {
            context,
            dynamic,
            options,
        }
    }

    pub fn resolve(&self, template: &str) -> Result<ResolvedTemplate, VariableError> {
        let mut stack = Vec::new();
        let mut traces = Vec::new();
        let mut sensitive_values = BTreeSet::new();
        let mut unresolved = BTreeSet::new();
        let value = self.resolve_template(
            template,
            &mut stack,
            &mut traces,
            &mut sensitive_values,
            &mut unresolved,
            0,
        )?;
        if self.options.strict_unresolved && !unresolved.is_empty() {
            return Err(VariableError::Unresolved(
                unresolved.iter().cloned().collect::<Vec<_>>(),
            ));
        }
        Ok(ResolvedTemplate {
            value,
            traces,
            sensitive_values: sensitive_values.into_iter().collect(),
            unresolved: unresolved.into_iter().collect(),
        })
    }

    fn resolve_template(
        &self,
        template: &str,
        stack: &mut Vec<String>,
        traces: &mut Vec<VariableTrace>,
        sensitive_values: &mut BTreeSet<String>,
        unresolved: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<String, VariableError> {
        if depth > self.options.maximum_depth {
            return Err(VariableError::MaximumDepth(self.options.maximum_depth));
        }
        let mut output = String::with_capacity(template.len());
        let mut cursor = 0;
        while let Some(relative_start) = template[cursor..].find("{{") {
            let start = cursor + relative_start;
            output.push_str(&template[cursor..start]);
            let expression_start = start + 2;
            let Some(relative_end) = template[expression_start..].find("}}") else {
                return Err(VariableError::UnclosedPlaceholder { byte_offset: start });
            };
            let end = expression_start + relative_end;
            let raw_expression = template[expression_start..end].trim();
            if raw_expression.is_empty() {
                return Err(VariableError::EmptyPlaceholder { byte_offset: start });
            }
            match self.resolve_expression(
                raw_expression,
                stack,
                traces,
                sensitive_values,
                unresolved,
                depth + 1,
            )? {
                Some(value) => output.push_str(&value),
                None if self.options.preserve_unresolved => {
                    output.push_str(&template[start..end + 2]);
                }
                None => {}
            }
            cursor = end + 2;
        }
        output.push_str(&template[cursor..]);
        Ok(output)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_expression(
        &self,
        raw_expression: &str,
        stack: &mut Vec<String>,
        traces: &mut Vec<VariableTrace>,
        sensitive_values: &mut BTreeSet<String>,
        unresolved: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<Option<String>, VariableError> {
        let (path_expression, default_value) = split_default(raw_expression);
        let mut path = path_expression.split('.');
        let root_name = path
            .next()
            .ok_or_else(|| VariableError::InvalidExpression(raw_expression.to_owned()))?;
        let remaining_path = path.collect::<Vec<_>>();

        if stack.iter().any(|entry| entry == root_name) {
            let mut cycle = stack.clone();
            cycle.push(root_name.to_owned());
            return Err(VariableError::Cycle(cycle));
        }

        let mut trace = self.trace_for(raw_expression, root_name);
        let dynamic_definition = root_name
            .starts_with('$')
            .then(|| self.dynamic.resolve(root_name))
            .flatten();
        let selected = dynamic_definition
            .as_ref()
            .map(|definition| (VariableScope::BuiltInDynamic, definition))
            .or_else(|| self.context.effective_definition(root_name));

        let Some((scope, definition)) = selected else {
            if let Some(default_value) = default_value {
                trace.used_default = true;
                traces.push(trace);
                return Ok(Some(default_value.to_owned()));
            }
            unresolved.insert(root_name.to_owned());
            traces.push(trace);
            return Ok(None);
        };

        trace.selected_scope = Some(scope);
        trace.dynamic = root_name.starts_with('$');
        for step in &mut trace.steps {
            step.selected = step.scope == scope && step.found && step.enabled;
        }

        let Some(value) = definition.value.get_path(&remaining_path) else {
            if let Some(default_value) = default_value {
                trace.used_default = true;
                traces.push(trace);
                return Ok(Some(default_value.to_owned()));
            }
            unresolved.insert(path_expression.to_owned());
            traces.push(trace);
            return Ok(None);
        };

        let rendered = value.display_value();
        stack.push(root_name.to_owned());
        let rendered = self.resolve_template(
            &rendered,
            stack,
            traces,
            sensitive_values,
            unresolved,
            depth,
        )?;
        stack.pop();

        if definition.sensitivity != ValueSensitivity::Public && !rendered.is_empty() {
            sensitive_values.insert(rendered.clone());
        }
        traces.push(trace);
        Ok(Some(rendered))
    }

    fn trace_for(&self, expression: &str, root_name: &str) -> VariableTrace {
        let mut steps = Vec::with_capacity(VariableScope::PRECEDENCE.len());
        for scope in VariableScope::PRECEDENCE {
            let definition = self.layer_definition(scope, root_name);
            steps.push(TraceStep {
                scope,
                found: definition.is_some(),
                enabled: definition.is_some_and(|value| value.enabled),
                selected: false,
                sensitivity: definition.map(|value| value.sensitivity),
            });
        }
        VariableTrace {
            expression: expression.to_owned(),
            root_name: root_name.to_owned(),
            selected_scope: None,
            steps,
            used_default: false,
            dynamic: false,
        }
    }

    fn layer_definition(
        &self,
        scope: VariableScope,
        root_name: &str,
    ) -> Option<&VariableDefinition> {
        if scope == VariableScope::BuiltInDynamic && root_name.starts_with('$') {
            return None;
        }
        self.context
            .layer(scope)
            .and_then(|layer| layer.get(root_name))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldResolutionTrace {
    pub field: String,
    pub traces: Vec<VariableTrace>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedHttpRequest {
    pub request: HttpRequest,
    pub field_traces: Vec<FieldResolutionTrace>,
    pub sensitive_values: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestResolutionError {
    pub field: String,
    pub source: VariableError,
}

impl Display for RequestResolutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.source)
    }
}

impl std::error::Error for RequestResolutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub fn resolve_http_request(
    request: &HttpRequest,
    context: &VariableContext,
    dynamic: &dyn DynamicVariableProvider,
    options: ResolverOptions,
) -> Result<ResolvedHttpRequest, RequestResolutionError> {
    let resolver = VariableResolver::new(context, dynamic, options);
    let mut request = request.clone();
    let mut field_traces = Vec::new();
    let mut sensitive_values = BTreeSet::new();

    let mut resolve =
        |field: String, input: &str| -> Result<ResolvedTemplate, RequestResolutionError> {
            let resolved = resolver
                .resolve(input)
                .map_err(|source| RequestResolutionError {
                    field: field.clone(),
                    source,
                })?;
            sensitive_values.extend(resolved.sensitive_values.iter().cloned());
            if !resolved.traces.is_empty() {
                field_traces.push(FieldResolutionTrace {
                    field,
                    traces: resolved.traces.clone(),
                });
            }
            Ok(resolved)
        };

    request.url = resolve("url".to_owned(), &request.url)?.value;
    for (index, field) in request.query.iter_mut().enumerate() {
        let name = resolve(format!("query[{index}].name"), &field.name)?;
        let value = resolve(format!("query[{index}].value"), &field.value)?;
        field.sensitivity = combined_sensitivity(field.sensitivity, &name, &value);
        field.name = name.value;
        field.value = value.value;
    }
    for (index, header) in request.headers.iter_mut().enumerate() {
        let name = resolve(format!("headers[{index}].name"), &header.name)?;
        let value = resolve(format!("headers[{index}].value"), &header.value)?;
        header.sensitivity = combined_sensitivity(header.sensitivity, &name, &value);
        header.name = name.value;
        header.value = value.value;
    }
    match &mut request.authentication {
        Authentication::None => {}
        Authentication::Basic { username, password } => {
            *username = resolve("auth.username".to_owned(), username)?.value;
            *password = resolve("auth.password".to_owned(), password)?.value;
        }
        Authentication::Bearer { token } => {
            *token = resolve("auth.token".to_owned(), token)?.value;
        }
        Authentication::ApiKey { name, value, .. } => {
            *name = resolve("auth.name".to_owned(), name)?.value;
            *value = resolve("auth.value".to_owned(), value)?.value;
        }
    }
    match &mut request.body {
        RequestBody::Empty => {}
        RequestBody::Text { content_type, text } => {
            if let Some(content_type) = content_type {
                *content_type = resolve("body.content_type".to_owned(), content_type)?.value;
            }
            *text = resolve("body.text".to_owned(), text)?.value;
        }
        RequestBody::Json(text) | RequestBody::Xml(text) => {
            *text = resolve("body.text".to_owned(), text)?.value;
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => {
            *query = resolve("body.query".to_owned(), query)?.value;
            *variables_json = resolve("body.variables_json".to_owned(), variables_json)?.value;
            if let Some(operation_name) = operation_name {
                *operation_name = resolve("body.operation_name".to_owned(), operation_name)?.value;
            }
        }
        RequestBody::FormUrlEncoded(fields) => {
            resolve_form_fields(fields, "body.fields", &mut resolve)?;
        }
        RequestBody::Multipart(fields) => {
            for (index, field) in fields.iter_mut().enumerate() {
                let name = resolve(format!("body.multipart[{index}].name"), &field.name)?;
                if let Some(content_type) = &mut field.content_type {
                    *content_type = resolve(
                        format!("body.multipart[{index}].content_type"),
                        content_type,
                    )?
                    .value;
                }
                let value_resolution = match &mut field.value {
                    MultipartValue::Text(value) => {
                        let resolved = resolve(format!("body.multipart[{index}].text"), value)?;
                        *value = resolved.value.clone();
                        resolved
                    }
                    MultipartValue::File { relative_path } => {
                        let resolved =
                            resolve(format!("body.multipart[{index}].file"), relative_path)?;
                        *relative_path = resolved.value.clone();
                        resolved
                    }
                };
                field.sensitivity =
                    combined_sensitivity(field.sensitivity, &name, &value_resolution);
            }
        }
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            *relative_path = resolve("body.file".to_owned(), relative_path)?.value;
        }
    }

    Ok(ResolvedHttpRequest {
        request,
        field_traces,
        sensitive_values: sensitive_values.into_iter().collect(),
    })
}

fn resolve_form_fields(
    fields: &mut [FormField],
    prefix: &str,
    resolve: &mut impl FnMut(String, &str) -> Result<ResolvedTemplate, RequestResolutionError>,
) -> Result<(), RequestResolutionError> {
    for (index, field) in fields.iter_mut().enumerate() {
        let name = resolve(format!("{prefix}[{index}].name"), &field.name)?;
        let value = resolve(format!("{prefix}[{index}].value"), &field.value)?;
        field.sensitivity = combined_sensitivity(field.sensitivity, &name, &value);
        field.name = name.value;
        field.value = value.value;
    }
    Ok(())
}

fn combined_sensitivity(
    current: ValueSensitivity,
    first: &ResolvedTemplate,
    second: &ResolvedTemplate,
) -> ValueSensitivity {
    let selected = first
        .traces
        .iter()
        .chain(&second.traces)
        .flat_map(|trace| trace.steps.iter())
        .filter(|step| step.selected)
        .filter_map(|step| step.sensitivity);
    selected.fold(current, strongest_sensitivity)
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

fn split_default(expression: &str) -> (&str, Option<&str>) {
    expression
        .split_once('|')
        .map_or((expression.trim(), None), |(name, default)| {
            (name.trim(), Some(default.trim()))
        })
}

static FALLBACK_RANDOM_COUNTER: AtomicU64 = AtomicU64::new(1);

fn generate_uuid_v4() -> String {
    let mut bytes = [0_u8; 16];
    let random_read = File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !random_read {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = u128::from(FALLBACK_RANDOM_COUNTER.fetch_add(1, Ordering::Relaxed));
        bytes.copy_from_slice(&(nanos ^ sequence).to_be_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VariableError {
    UnclosedPlaceholder { byte_offset: usize },
    EmptyPlaceholder { byte_offset: usize },
    InvalidExpression(String),
    Unresolved(Vec<String>),
    Cycle(Vec<String>),
    MaximumDepth(usize),
}

impl Display for VariableError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnclosedPlaceholder { byte_offset } => {
                write!(
                    formatter,
                    "unclosed variable placeholder at byte {byte_offset}"
                )
            }
            Self::EmptyPlaceholder { byte_offset } => {
                write!(
                    formatter,
                    "empty variable placeholder at byte {byte_offset}"
                )
            }
            Self::InvalidExpression(expression) => {
                write!(formatter, "invalid variable expression: {expression}")
            }
            Self::Unresolved(names) => {
                write!(formatter, "unresolved variables: {}", names.join(", "))
            }
            Self::Cycle(path) => write!(
                formatter,
                "cyclic variable reference: {}",
                path.join(" -> ")
            ),
            Self::MaximumDepth(depth) => {
                write!(formatter, "variable resolution exceeded depth {depth}")
            }
        }
    }
}

impl std::error::Error for VariableError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixedDynamic;

    impl DynamicVariableProvider for FixedDynamic {
        fn resolve(&self, name: &str) -> Option<VariableDefinition> {
            (name == "$uuid").then(|| definition("fixed-uuid", ValueSensitivity::Public))
        }
    }

    fn definition(value: &str, sensitivity: ValueSensitivity) -> VariableDefinition {
        VariableDefinition {
            value: VariableValue::String(value.to_owned()),
            sensitivity,
            enabled: true,
            description: None,
        }
    }

    #[test]
    fn highest_precedence_scope_wins_and_is_traced() {
        let mut context = VariableContext::default();
        context.layer_mut(VariableScope::Workspace).insert(
            "host",
            definition("workspace.test", ValueSensitivity::Public),
        );
        context.layer_mut(VariableScope::Environment).insert(
            "host",
            definition("environment.test", ValueSensitivity::Public),
        );
        context
            .layer_mut(VariableScope::Request)
            .insert("host", definition("request.test", ValueSensitivity::Public));
        let result = VariableResolver::new(&context, &FixedDynamic, ResolverOptions::default())
            .resolve("https://{{host}}/users")
            .expect("resolves");
        assert_eq!(result.value, "https://request.test/users");
        assert_eq!(
            result.traces[0].selected_scope,
            Some(VariableScope::Request)
        );
    }

    #[test]
    fn resolves_nested_objects_and_defaults() {
        let mut object = BTreeMap::new();
        object.insert(
            "host".to_owned(),
            VariableValue::String("api.test".to_owned()),
        );
        let mut context = VariableContext::default();
        context.layer_mut(VariableScope::Environment).insert(
            "service",
            VariableDefinition {
                value: VariableValue::Object(object),
                sensitivity: ValueSensitivity::Public,
                enabled: true,
                description: None,
            },
        );
        let result = VariableResolver::new(&context, &FixedDynamic, ResolverOptions::default())
            .resolve("{{service.host}}/{{missing|fallback}}")
            .expect("resolves");
        assert_eq!(result.value, "api.test/fallback");
    }

    #[test]
    fn rejects_cycles() {
        let mut context = VariableContext::default();
        context
            .layer_mut(VariableScope::Workspace)
            .insert("a", definition("{{b}}", ValueSensitivity::Public));
        context
            .layer_mut(VariableScope::Workspace)
            .insert("b", definition("{{a}}", ValueSensitivity::Public));
        let error = VariableResolver::new(&context, &FixedDynamic, ResolverOptions::default())
            .resolve("{{a}}")
            .expect_err("cycle must fail");
        assert!(matches!(error, VariableError::Cycle(_)));
    }

    #[test]
    fn reports_sensitive_resolved_values() {
        let mut context = VariableContext::default();
        context
            .layer_mut(VariableScope::Environment)
            .insert("token", definition("top-secret", ValueSensitivity::Secret));
        let result = VariableResolver::new(&context, &FixedDynamic, ResolverOptions::default())
            .resolve("Bearer {{token}}")
            .expect("resolves");
        assert_eq!(result.sensitive_values, ["top-secret"]);
    }

    #[test]
    fn supports_injected_dynamic_values() {
        let context = VariableContext::default();
        let result = VariableResolver::new(&context, &FixedDynamic, ResolverOptions::default())
            .resolve("{{$uuid}}")
            .expect("resolves");
        assert_eq!(result.value, "fixed-uuid");
    }

    fn request_fixture() -> HttpRequest {
        HttpRequest {
            id: apex_domain::StableId::parse("resolve-request").expect("valid id"),
            name: "Resolve request".to_owned(),
            method: apex_domain::HttpMethod::Post,
            url: "https://{{host}}/users".to_owned(),
            query: vec![FormField {
                name: "token".to_owned(),
                value: "{{token}}".to_owned(),
                enabled: true,
                sensitivity: ValueSensitivity::Public,
            }],
            headers: vec![
                apex_domain::HeaderEntry::new("X-Token", "{{token}}").expect("valid header"),
            ],
            authentication: Authentication::Bearer {
                token: "{{token}}".to_owned(),
            },
            body: RequestBody::Json("{\"name\":\"{{name}}\"}".to_owned()),
            settings: apex_domain::RequestSettings::default(),
            documentation: String::new(),
        }
    }

    #[test]
    fn resolves_complete_http_request_with_field_traces() {
        let mut context = VariableContext::default();
        context
            .layer_mut(VariableScope::Environment)
            .insert("host", definition("api.test", ValueSensitivity::Public));
        context
            .layer_mut(VariableScope::Environment)
            .insert("token", definition("top-secret", ValueSensitivity::Secret));
        context
            .layer_mut(VariableScope::Request)
            .insert("name", definition("Ada", ValueSensitivity::Public));
        let resolved = resolve_http_request(
            &request_fixture(),
            &context,
            &FixedDynamic,
            ResolverOptions::default(),
        )
        .expect("request resolves");
        assert_eq!(resolved.request.url, "https://api.test/users");
        assert_eq!(resolved.request.query[0].value, "top-secret");
        assert_eq!(
            resolved.request.query[0].sensitivity,
            ValueSensitivity::Secret
        );
        assert_eq!(
            resolved.request.headers[0].sensitivity,
            ValueSensitivity::Secret
        );
        assert_eq!(resolved.sensitive_values, ["top-secret"]);
        assert!(
            resolved
                .field_traces
                .iter()
                .any(|trace| trace.field == "url")
        );
        assert_eq!(
            resolved.request.body,
            RequestBody::Json("{\"name\":\"Ada\"}".to_owned())
        );
    }

    #[test]
    fn request_resolution_reports_exact_field() {
        let error = resolve_http_request(
            &request_fixture(),
            &VariableContext::default(),
            &FixedDynamic,
            ResolverOptions::default(),
        )
        .expect_err("missing host must fail");
        assert_eq!(error.field, "url");
        assert!(matches!(error.source, VariableError::Unresolved(_)));
    }
}
