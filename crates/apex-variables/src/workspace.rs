use crate::{VariableContext, VariableScope};
use apex_domain::StableId;
use apex_secrets::SecretStoreChain;
use apex_workspace::{EnvironmentSummary, WorkspaceError, WorkspaceRepository};
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
    loaded.environment = Some(EnvironmentSummary {
        id: environment.value.id.clone(),
        name: environment.value.name.clone(),
        path: environment.path,
        variable_count: environment.value.variables.len(),
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
    use apex_domain::{ValueSensitivity, VariableValue};
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
