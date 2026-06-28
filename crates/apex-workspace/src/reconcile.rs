use crate::{WorkspaceChange, WorkspaceChangeKind, WorkspaceResourceKind};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExternalChangeReason {
    Created,
    Modified,
    Removed,
    Renamed { from: PathBuf, to: Option<PathBuf> },
    RescanRequired,
}

impl ExternalChangeReason {
    pub fn summary(&self) -> String {
        match self {
            Self::Created => "the request was created externally".to_owned(),
            Self::Modified => "the request was modified externally".to_owned(),
            Self::Removed => "the request was removed externally".to_owned(),
            Self::Renamed { from, to } => match to {
                Some(to) => format!(
                    "the request was renamed externally from {} to {}",
                    from.display(),
                    to.display()
                ),
                None => format!(
                    "the request path changed externally from {}",
                    from.display()
                ),
            },
            Self::RescanRequired => {
                "the filesystem watcher requires the workspace to be rescanned".to_owned()
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentReconcileAction {
    None,
    Verify {
        path: PathBuf,
        reason: ExternalChangeReason,
        conflict_if_changed: bool,
    },
    Missing {
        path: PathBuf,
        reason: ExternalChangeReason,
        had_unsaved_changes: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceReconciliation {
    pub refresh_tree: bool,
    pub document: DocumentReconcileAction,
}

pub fn reconcile_workspace_change(
    current_request: Option<&Path>,
    dirty: bool,
    change: &WorkspaceChange,
) -> WorkspaceReconciliation {
    let refresh_tree = change.kind == WorkspaceChangeKind::RescanRequired
        || change.paths.iter().any(|path| {
            matches!(
                path.resource,
                WorkspaceResourceKind::Request | WorkspaceResourceKind::Collection
            )
        });
    let Some(current_request) = current_request else {
        return WorkspaceReconciliation {
            refresh_tree,
            document: DocumentReconcileAction::None,
        };
    };

    let document = match change.kind {
        WorkspaceChangeKind::RescanRequired => DocumentReconcileAction::Verify {
            path: current_request.to_owned(),
            reason: ExternalChangeReason::RescanRequired,
            conflict_if_changed: dirty,
        },
        WorkspaceChangeKind::Renamed => reconcile_rename(current_request, dirty, change),
        WorkspaceChangeKind::Removed => {
            if change
                .paths
                .iter()
                .any(|path| path.relative_path == current_request)
            {
                DocumentReconcileAction::Missing {
                    path: current_request.to_owned(),
                    reason: ExternalChangeReason::Removed,
                    had_unsaved_changes: dirty,
                }
            } else {
                DocumentReconcileAction::None
            }
        }
        WorkspaceChangeKind::Created | WorkspaceChangeKind::Modified => {
            if change
                .paths
                .iter()
                .any(|path| path.relative_path == current_request)
            {
                DocumentReconcileAction::Verify {
                    path: current_request.to_owned(),
                    reason: if change.kind == WorkspaceChangeKind::Created {
                        ExternalChangeReason::Created
                    } else {
                        ExternalChangeReason::Modified
                    },
                    conflict_if_changed: dirty,
                }
            } else {
                DocumentReconcileAction::None
            }
        }
    };

    WorkspaceReconciliation {
        refresh_tree,
        document,
    }
}

fn reconcile_rename(
    current_request: &Path,
    dirty: bool,
    change: &WorkspaceChange,
) -> DocumentReconcileAction {
    let Some(source) = change.paths.first().map(|path| path.relative_path.clone()) else {
        return DocumentReconcileAction::None;
    };
    let destination = change.paths.get(1).map(|path| path.relative_path.clone());
    if source == current_request {
        return DocumentReconcileAction::Verify {
            path: destination
                .clone()
                .unwrap_or_else(|| current_request.to_owned()),
            reason: ExternalChangeReason::Renamed {
                from: source,
                to: destination,
            },
            conflict_if_changed: dirty,
        };
    }
    if destination.as_deref() == Some(current_request) {
        return DocumentReconcileAction::Verify {
            path: current_request.to_owned(),
            reason: ExternalChangeReason::Renamed {
                from: source,
                to: destination,
            },
            conflict_if_changed: dirty,
        };
    }
    DocumentReconcileAction::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspaceChangedPath;

    fn request_path(value: &str) -> WorkspaceChangedPath {
        WorkspaceChangedPath {
            relative_path: PathBuf::from(value),
            resource: WorkspaceResourceKind::Request,
        }
    }

    #[test]
    fn clean_modified_document_is_verified_before_reload() {
        let change = WorkspaceChange {
            kind: WorkspaceChangeKind::Modified,
            paths: vec![request_path("collections/users/get.request.toml")],
        };
        let reconciliation = reconcile_workspace_change(
            Some(Path::new("collections/users/get.request.toml")),
            false,
            &change,
        );
        assert!(reconciliation.refresh_tree);
        assert_eq!(
            reconciliation.document,
            DocumentReconcileAction::Verify {
                path: PathBuf::from("collections/users/get.request.toml"),
                reason: ExternalChangeReason::Modified,
                conflict_if_changed: false,
            }
        );
    }

    #[test]
    fn dirty_modified_document_verifies_without_allowing_silent_reload() {
        let change = WorkspaceChange {
            kind: WorkspaceChangeKind::Modified,
            paths: vec![request_path("collections/users/get.request.toml")],
        };
        let reconciliation = reconcile_workspace_change(
            Some(Path::new("collections/users/get.request.toml")),
            true,
            &change,
        );
        assert_eq!(
            reconciliation.document,
            DocumentReconcileAction::Verify {
                path: PathBuf::from("collections/users/get.request.toml"),
                reason: ExternalChangeReason::Modified,
                conflict_if_changed: true,
            }
        );
    }

    #[test]
    fn removed_document_records_dirty_state() {
        let change = WorkspaceChange {
            kind: WorkspaceChangeKind::Removed,
            paths: vec![request_path("collections/users/get.request.toml")],
        };
        let reconciliation = reconcile_workspace_change(
            Some(Path::new("collections/users/get.request.toml")),
            true,
            &change,
        );
        assert_eq!(
            reconciliation.document,
            DocumentReconcileAction::Missing {
                path: PathBuf::from("collections/users/get.request.toml"),
                reason: ExternalChangeReason::Removed,
                had_unsaved_changes: true,
            }
        );
    }

    #[test]
    fn rename_follows_destination_and_preserves_conflict_policy() {
        let change = WorkspaceChange {
            kind: WorkspaceChangeKind::Renamed,
            paths: vec![
                request_path("collections/users/old.request.toml"),
                request_path("collections/users/new.request.toml"),
            ],
        };
        let reconciliation = reconcile_workspace_change(
            Some(Path::new("collections/users/old.request.toml")),
            true,
            &change,
        );
        assert_eq!(
            reconciliation.document,
            DocumentReconcileAction::Verify {
                path: PathBuf::from("collections/users/new.request.toml"),
                reason: ExternalChangeReason::Renamed {
                    from: PathBuf::from("collections/users/old.request.toml"),
                    to: Some(PathBuf::from("collections/users/new.request.toml")),
                },
                conflict_if_changed: true,
            }
        );
    }

    #[test]
    fn rescan_rechecks_active_document_and_tree() {
        let change = WorkspaceChange {
            kind: WorkspaceChangeKind::RescanRequired,
            paths: Vec::new(),
        };
        let reconciliation = reconcile_workspace_change(
            Some(Path::new("collections/users/get.request.toml")),
            false,
            &change,
        );
        assert!(reconciliation.refresh_tree);
        assert_eq!(
            reconciliation.document,
            DocumentReconcileAction::Verify {
                path: PathBuf::from("collections/users/get.request.toml"),
                reason: ExternalChangeReason::RescanRequired,
                conflict_if_changed: false,
            }
        );
    }

    #[test]
    fn unrelated_environment_change_does_not_touch_request_state() {
        let change = WorkspaceChange {
            kind: WorkspaceChangeKind::Modified,
            paths: vec![WorkspaceChangedPath {
                relative_path: PathBuf::from("environments/development.toml"),
                resource: WorkspaceResourceKind::Environment,
            }],
        };
        let reconciliation = reconcile_workspace_change(
            Some(Path::new("collections/users/get.request.toml")),
            false,
            &change,
        );
        assert!(!reconciliation.refresh_tree);
        assert_eq!(reconciliation.document, DocumentReconcileAction::None);
    }
}
