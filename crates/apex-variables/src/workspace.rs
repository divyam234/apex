use crate::{VariableContext, VariableScope};
use apex_domain::{StableId, ValueSensitivity};
use apex_secrets::SecretStoreChain;
use apex_workspace::{EnvironmentSummary, WorkspaceError, WorkspaceRepository};
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceVariableSelection {
    pub environment: Option<String>,
    pub include_local_override: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariableLayerSource {
    pub scope: VariableScope,
    pub label: String,
    pub variable_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedWorkspaceVariables {
    pub context: VariableContext,
    pub environment: Option<EnvironmentSummary>,
    pub sources: Vec<VariableLayerSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariableCandidateInspection {
    pub scope: VariableScope,
    pub source_label: String,
    pub enabled: bool,
    pub sensitivity: ValueSensitivity,
    pub selected: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveVariableInspection {
    pub name: String,
    pub selected_scope: Option<VariableScope>,
    pub displayed_value: String,
    pub candidates: Vec<VariableCandidateInspection>,
}

pub fn inspect_effective_variables(
    loaded: &LoadedWorkspaceVariables,
) -> Vec<EffectiveVariableInspection> {
    let mut names = BTreeSet::new();
    for (_, layer) in loaded.context.iter_layers() {
        names.extend(layer.iter().map(|(name, _)| name.clone()));
    }

    names
        .into_iter()
        .map(|name| {
            let selected = loaded.context.effective_definition(&name);
            let selected_scope = selected.map(|(scope, _)| scope);
            let displayed_value = selected.map_or_else(
                || "[UNRESOLVED]".to_owned(),
                |(_, definition)| {
                    if definition.sensitivity == ValueSensitivity::Public {
                        definition.value.display_value()
                    } else {
                        "[REDACTED]".to_owned()
                    }
                },
            );
            let candidates = VariableScope::PRECEDENCE
                .iter()
                .filter_map(|scope| {
                    loaded
                        .context
                        .layer(*scope)
                        .and_then(|layer| layer.get(&name))
                        .map(|definition| VariableCandidateInspection {
                            scope: *scope,
                            source_label: source_label(loaded, *scope),
                            enabled: definition.enabled,
                            sensitivity: definition.sensitivity,
                            selected: selected_scope == Some(*scope) && definition.enabled,
                            description: definition.description.clone(),
                        })
                })
                .collect();
            EffectiveVariableInspection {
                name,
                selected_scope,
                displayed_value,
                candidates,
            }
        })
        .collect()
}

fn source_label(loaded: &LoadedWorkspaceVariables, scope: VariableScope) -> String {
    loaded
        .sources
        .iter()
        .find(|source| source.scope == scope)
        .map_or_else(|| scope.label().to_owned(), |source| source.label.clone())
}

pub fn load_workspace_variables(
    repository: &WorkspaceRepository,
    selection: &WorkspaceVariableSelection,
    secret_stores: Option<&SecretStoreChain>,
) -> Result<LoadedWorkspaceVariables, WorkspaceVariableError> {
    let manifest = repository.load_manifest()?.value;
    let mut loaded = LoadedWorkspaceVariables::default();

    if let Some(document) = repository.load_workspace_variables()? {
        let variables = document.value.resolve(secret_stores)?;
        let count = variables.len();
        for (name, definition) in variables {
            loaded
                .context
                .layer_mut(VariableScope::Workspace)
                .insert(name, definition);
        }
        loaded.sources.push(VariableLayerSource {
            scope: VariableScope::Workspace,
            label: document.path.display().to_string(),
            variable_count: count,
        });
    }

    let selected_id = selection
        .environment
        .as_deref()
        .or(manifest.default_environment.as_deref());
    let Some(selected_id) = selected_id else {
        return Ok(loaded);
    };
    let id = StableId::parse(selected_id.to_owned())
        .map_err(|error| WorkspaceVariableError::InvalidEnvironment(error.to_string()))?;
    let environment = repository.load_environment(&id)?;
    let variables = environment.value.resolve(secret_stores)?;
    let count = variables.len();
    for (name, definition) in variables {
        loaded
            .context
            .layer_mut(VariableScope::Environment)
            .insert(name, definition);
    }
    loaded.sources.push(VariableLayerSource {
        scope: VariableScope::Environment,
        label: environment.path.display().to_string(),
        variable_count: count,
    });
    let has_local_override = repository
        .local_environment_override_path(&environment.value.id)
        .exists();
    loaded.environment = Some(EnvironmentSummary {
        id: environment.value.id.clone(),
        name: environment.value.name.clone(),
        path: environment.path,
        variable_count: environment.value.variables.len(),
        has_local_override,
    });

    if selection.include_local_override
        && let Some(local) = repository.load_local_environment_override(&id)?
    {
        let variables = local.value.resolve(secret_stores)?;
        let count = variables.len();
        for (name, definition) in variables {
            loaded
                .context
                .layer_mut(VariableScope::LocalEnvironmentOverride)
                .insert(name, definition);
        }
        loaded.sources.push(VariableLayerSource {
            scope: VariableScope::LocalEnvironmentOverride,
            label: local.path.display().to_string(),
            variable_count: count,
        });
    }

    Ok(loaded)
}

#[derive(Debug)]
pub enum WorkspaceVariableError {
    Workspace(WorkspaceError),
    InvalidEnvironment(String),
}

impl Display for WorkspaceVariableError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace(error) => Display::fmt(error, formatter),
            Self::InvalidEnvironment(detail) => {
                write!(formatter, "invalid environment selection: {detail}")
            }
        }
    }
}

impl std::error::Error for WorkspaceVariableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Workspace(error) => Some(error),
            Self::InvalidEnvironment(_) => None,
        }
    }
}

impl From<WorkspaceError> for WorkspaceVariableError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{ValueSensitivity, VariableDefinition, VariableValue};
    use apex_workspace::{
        StoredVariable, StoredVariableSource, VariableSetDocument, WorkspaceManifest,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_workspace() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("apex-variable-context-{unique}"))
    }

    fn variable(name: &str, value: &str) -> StoredVariable {
        StoredVariable {
            name: name.to_owned(),
            source: StoredVariableSource::Literal(VariableValue::String(value.to_owned())),
            sensitivity: ValueSensitivity::Public,
            enabled: true,
            description: None,
        }
    }

    #[test]
    fn loads_workspace_environment_and_local_override_with_precedence() {
        let root = temporary_workspace();
        let repository = WorkspaceRepository::new(&root).expect("repository");
        let mut manifest =
            WorkspaceManifest::new(StableId::parse("workspace").expect("id"), "Workspace");
        manifest.default_environment = Some("development".to_owned());
        repository.initialize(&manifest).expect("initialize");

        let mut workspace = VariableSetDocument::new(
            StableId::parse("workspace-vars").expect("id"),
            "Workspace variables",
        );
        workspace.variables.push(variable("host", "workspace.test"));
        repository
            .save_variable_set(&repository.workspace_variables_path(), &workspace, None)
            .expect("save workspace variables");

        let environment_id = StableId::parse("development").expect("id");
        let mut environment = VariableSetDocument::new(environment_id.clone(), "Development");
        environment
            .variables
            .push(variable("host", "environment.test"));
        repository
            .save_variable_set(
                &repository.environment_path(&environment_id),
                &environment,
                None,
            )
            .expect("save environment");

        let mut local = VariableSetDocument::new(environment_id.clone(), "Development local");
        local.variables.push(variable("host", "local.test"));
        repository
            .save_variable_set(
                &repository.local_environment_override_path(&environment_id),
                &local,
                None,
            )
            .expect("save local override");

        let loaded = load_workspace_variables(
            &repository,
            &WorkspaceVariableSelection {
                environment: None,
                include_local_override: true,
            },
            None,
        )
        .expect("loads variables");
        let (scope, definition) = loaded
            .context
            .effective_definition("host")
            .expect("effective variable");
        assert_eq!(scope, VariableScope::LocalEnvironmentOverride);
        assert_eq!(
            definition.value,
            VariableValue::String("local.test".to_owned())
        );
        assert_eq!(loaded.environment.expect("environment").name, "Development");
        assert_eq!(loaded.sources.len(), 3);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn effective_inspection_reports_sources_and_redacts_selected_secret() {
        let mut loaded = LoadedWorkspaceVariables::default();
        loaded.context.layer_mut(VariableScope::Workspace).insert(
            "token",
            VariableDefinition {
                value: VariableValue::String("workspace-public".to_owned()),
                sensitivity: ValueSensitivity::Public,
                enabled: true,
                description: Some("workspace candidate".to_owned()),
            },
        );
        loaded.context.layer_mut(VariableScope::Environment).insert(
            "token",
            VariableDefinition {
                value: VariableValue::String("top-secret".to_owned()),
                sensitivity: ValueSensitivity::Secret,
                enabled: true,
                description: Some("secret candidate".to_owned()),
            },
        );
        loaded
            .context
            .layer_mut(VariableScope::LocalEnvironmentOverride)
            .insert(
                "token",
                VariableDefinition {
                    value: VariableValue::String("disabled-local".to_owned()),
                    sensitivity: ValueSensitivity::Sensitive,
                    enabled: false,
                    description: None,
                },
            );
        loaded.sources = vec![
            VariableLayerSource {
                scope: VariableScope::Workspace,
                label: "variables.toml".to_owned(),
                variable_count: 1,
            },
            VariableLayerSource {
                scope: VariableScope::Environment,
                label: "environments/development.toml".to_owned(),
                variable_count: 1,
            },
            VariableLayerSource {
                scope: VariableScope::LocalEnvironmentOverride,
                label: ".apex/environments/development.local.toml".to_owned(),
                variable_count: 1,
            },
        ];

        let inspection = inspect_effective_variables(&loaded);
        assert_eq!(inspection.len(), 1);
        let token = &inspection[0];
        assert_eq!(token.name, "token");
        assert_eq!(token.selected_scope, Some(VariableScope::Environment));
        assert_eq!(token.displayed_value, "[REDACTED]");
        assert!(!format!("{token:?}").contains("top-secret"));
        assert_eq!(token.candidates.len(), 3);
        assert_eq!(
            token
                .candidates
                .iter()
                .find(|candidate| candidate.selected)
                .expect("selected candidate")
                .source_label,
            "environments/development.toml"
        );
        assert!(
            !token
                .candidates
                .iter()
                .find(|candidate| candidate.scope == VariableScope::LocalEnvironmentOverride)
                .expect("local candidate")
                .selected
        );
    }

    #[test]
    fn explicit_environment_overrides_manifest_default() {
        let root = temporary_workspace();
        let repository = WorkspaceRepository::new(&root).expect("repository");
        let mut manifest =
            WorkspaceManifest::new(StableId::parse("workspace").expect("id"), "Workspace");
        manifest.default_environment = Some("development".to_owned());
        repository.initialize(&manifest).expect("initialize");
        let staging_id = StableId::parse("staging").expect("id");
        let staging = VariableSetDocument::new(staging_id.clone(), "Staging");
        repository
            .save_variable_set(&repository.environment_path(&staging_id), &staging, None)
            .expect("save staging");
        let loaded = load_workspace_variables(
            &repository,
            &WorkspaceVariableSelection {
                environment: Some("staging".to_owned()),
                include_local_override: false,
            },
            None,
        )
        .expect("loads staging");
        assert_eq!(loaded.environment.expect("environment").id, staging_id);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
