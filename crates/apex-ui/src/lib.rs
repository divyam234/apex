#![forbid(unsafe_code)]

pub mod session;
mod workspace_monitor;
use session::{CloseTabError, RequestTabState, ResourceIdentity, WorkspaceSession};

use apex_ai::{AiConfig, AiRequest, PayloadPreview, preview as preview_ai, send_confirmed};
use apex_contracts::OpenApiDocument;
use apex_domain::{
    ApiKeyPlacement, Authentication, CancellationToken, ExecutionError, ExecutionEvent, FormField,
    HeaderEntry, HttpMethod, HttpRequest, MultipartField, MultipartValue, RequestBody,
    RequestSettings, StableId, ValueSensitivity, VariableValue,
};
use apex_export::{CodeTarget, CodegenOptions, generate as generate_code};
use apex_git::GitRepository;
use apex_history::{
    BodyDifference, HistoryDatabase, HistoryEntry, HistoryQuery, SemanticDiffPolicy,
    semantic_response_diff,
};
use apex_http::HttpAdapter;
use apex_import::{ImportPreview, parse_curl, parse_postman_v21};
use apex_mock::{MockConfig, MockRoute, MockServer};
use apex_plugins::{
    Capability, ExtensionPoint, PluginLimits, PluginManifest, ValidatedPlugin, invoke_plugin,
    validate_plugin,
};
use apex_protocols::{
    BoundedStreamLog, GraphqlRequest, GrpcRequest, StreamDirection, StreamProtocol,
    build_http_request as build_graphql_http_request, reflection_request, validate_grpc_request,
    validate_request as validate_graphql_request,
};
use apex_runner::{
    CookiePolicy, ExecutionContext, ExecutionEventSink, ExecutionResult, FailurePolicy,
    ItemExecution, ProtocolAdapter, ProtocolRequest, ResolvedRequest, RunConfig, RunItem,
    RunSummary, StoredBody, run_collection,
};
use apex_secrets::{EnvironmentSecretStore, SecretLeakDetector, SecretRef, SecretStoreChain};
use apex_variables::{
    ResolverOptions, SystemDynamicVariables, VariableContext, WorkspaceVariableSelection,
    load_workspace_variables, resolve_http_request,
};
use apex_workspace::{
    CollectionDocument, DocumentReconcileAction, EnvironmentSummary, ExternalChangeReason,
    FileFingerprint, FolderDocument, OrderDocument, RequestDocument, SearchIndexPolicy,
    SearchQuery, SearchResult, StoredVariable, StoredVariableSource, VariableSetDocument,
    WorkspaceChange, WorkspaceRepository, WorkspaceRequestEntry, WorkspaceSearchIndex,
    reconcile_workspace_change,
};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement, Render, Styled, Subscription,
    Window, actions, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, TitleBar,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    dock::{DockArea, DockItem, Panel, PanelEvent, PanelStyle},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    notification::Notification,
    tab::{Tab, TabBar},
    tree::{TreeItem, TreeState, tree},
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::thread;
use workspace_monitor::{WorkspaceMonitorMessage, start_workspace_monitor};

actions!(
    apex,
    [
        NewRequest,
        SendRequest,
        CancelRequest,
        SaveRequest,
        OpenCommandPalette,
        FocusUrl,
        CycleEnvironment,
        NextRequestTab,
        PreviousRequestTab,
        ReopenClosedRequestTab
    ]
);

const APP_CONTEXT: &str = "ApexAPI";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind {
    NewRequest,
    Send,
    Cancel,
    Save,
    FocusUrl,
    CommandPalette,
    CycleEnvironment,
    NextRequestTab,
    PreviousRequestTab,
    ReopenClosedRequestTab,
}

impl CommandKind {
    fn dispatch(self, window: &mut Window, cx: &mut App) {
        match self {
            Self::NewRequest => window.dispatch_action(Box::new(NewRequest), cx),
            Self::Send => window.dispatch_action(Box::new(SendRequest), cx),
            Self::Cancel => window.dispatch_action(Box::new(CancelRequest), cx),
            Self::Save => window.dispatch_action(Box::new(SaveRequest), cx),
            Self::FocusUrl => window.dispatch_action(Box::new(FocusUrl), cx),
            Self::CommandPalette => window.dispatch_action(Box::new(OpenCommandPalette), cx),
            Self::CycleEnvironment => window.dispatch_action(Box::new(CycleEnvironment), cx),
            Self::NextRequestTab => window.dispatch_action(Box::new(NextRequestTab), cx),
            Self::PreviousRequestTab => window.dispatch_action(Box::new(PreviousRequestTab), cx),
            Self::ReopenClosedRequestTab => {
                window.dispatch_action(Box::new(ReopenClosedRequestTab), cx)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub binding: &'static str,
    pub kind: CommandKind,
}

impl CommandDescriptor {
    pub const fn new(
        id: &'static str,
        label: &'static str,
        binding: &'static str,
        kind: CommandKind,
    ) -> Self {
        Self {
            id,
            label,
            binding,
            kind,
        }
    }
}

pub const COMMANDS: &[CommandDescriptor] = &[
    CommandDescriptor::new(
        "request.new",
        "Create request draft",
        "Ctrl/Cmd+N",
        CommandKind::NewRequest,
    ),
    CommandDescriptor::new(
        "request.send",
        "Send current request",
        "Ctrl/Cmd+Enter",
        CommandKind::Send,
    ),
    CommandDescriptor::new(
        "request.cancel",
        "Cancel current request",
        "Escape",
        CommandKind::Cancel,
    ),
    CommandDescriptor::new(
        "request.save",
        "Save current draft",
        "Ctrl/Cmd+S",
        CommandKind::Save,
    ),
    CommandDescriptor::new(
        "request.focus-url",
        "Focus URL",
        "Ctrl/Cmd+L",
        CommandKind::FocusUrl,
    ),
    CommandDescriptor::new(
        "application.command-palette",
        "Open command palette",
        "Ctrl/Cmd+Shift+P",
        CommandKind::CommandPalette,
    ),
    CommandDescriptor::new(
        "environment.cycle",
        "Switch environment",
        "Ctrl/Cmd+K Ctrl/Cmd+E",
        CommandKind::CycleEnvironment,
    ),
    CommandDescriptor::new(
        "request-tab.next",
        "Activate next request tab",
        "Ctrl/Cmd+PageDown",
        CommandKind::NextRequestTab,
    ),
    CommandDescriptor::new(
        "request-tab.previous",
        "Activate previous request tab",
        "Ctrl/Cmd+PageUp",
        CommandKind::PreviousRequestTab,
    ),
    CommandDescriptor::new(
        "request-tab.reopen-closed",
        "Reopen recently closed request tab",
        "Ctrl/Cmd+Shift+T",
        CommandKind::ReopenClosedRequestTab,
    ),
];

pub fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewRequest, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-enter", SendRequest, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-s", SaveRequest, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-l", FocusUrl, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-p", OpenCommandPalette, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-k cmd-e", CycleEnvironment, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-pagedown", NextRequestTab, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-pageup", PreviousRequestTab, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-t", ReopenClosedRequestTab, Some(APP_CONTEXT)),
        KeyBinding::new("escape", CancelRequest, Some(APP_CONTEXT)),
    ]);

    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-n", NewRequest, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-enter", SendRequest, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-s", SaveRequest, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-l", FocusUrl, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-p", OpenCommandPalette, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-k ctrl-e", CycleEnvironment, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-pagedown", NextRequestTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-pageup", PreviousRequestTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-t", ReopenClosedRequestTab, Some(APP_CONTEXT)),
        KeyBinding::new("escape", CancelRequest, Some(APP_CONTEXT)),
    ]);
}

#[derive(Clone, Debug)]
struct WorkspaceBrowser {
    label: String,
    repository: Option<WorkspaceRepository>,
    requests: Vec<WorkspaceRequestEntry>,
    environments: Vec<EnvironmentSummary>,
    default_environment: Option<String>,
    error: Option<String>,
}

impl WorkspaceBrowser {
    fn open(root: Option<PathBuf>) -> Self {
        let Some(root) = root else {
            return Self {
                label: "Local Draft Workspace".to_owned(),
                repository: None,
                requests: Vec::new(),
                environments: Vec::new(),
                default_environment: None,
                error: None,
            };
        };
        let repository = match WorkspaceRepository::new(root.clone()) {
            Ok(repository) => repository,
            Err(error) => {
                return Self {
                    label: root.display().to_string(),
                    repository: None,
                    requests: Vec::new(),
                    environments: Vec::new(),
                    default_environment: None,
                    error: Some(error.to_string()),
                };
            }
        };
        let manifest = match repository.load_manifest() {
            Ok(manifest) => manifest,
            Err(error) => {
                return Self {
                    label: root.display().to_string(),
                    repository: Some(repository),
                    requests: Vec::new(),
                    environments: Vec::new(),
                    default_environment: None,
                    error: Some(error.to_string()),
                };
            }
        };
        let requests = repository.list_requests();
        let environments = repository.list_environments();
        let error = requests
            .as_ref()
            .err()
            .map(ToString::to_string)
            .or_else(|| environments.as_ref().err().map(ToString::to_string));
        Self {
            label: manifest.value.name,
            repository: Some(repository),
            requests: requests.unwrap_or_default(),
            environments: environments.unwrap_or_default(),
            default_environment: manifest.value.default_environment,
            error,
        }
    }

    fn initial_document(&self) -> Option<(DocumentStore, HttpRequest, FileFingerprint)> {
        let repository = self.repository.clone()?;
        let entry = self.requests.first()?;
        let loaded = repository.load_request(&entry.path).ok()?;
        Some((
            DocumentStore::new(repository, entry.path.clone()),
            loaded.value.request,
            loaded.fingerprint,
        ))
    }

    fn variable_context(&self, environment: Option<&str>) -> Result<VariableContext, String> {
        let Some(repository) = &self.repository else {
            return Ok(VariableContext::default());
        };
        let mut stores = SecretStoreChain::default();
        stores.push(Arc::new(EnvironmentSecretStore));
        load_workspace_variables(
            repository,
            &WorkspaceVariableSelection {
                environment: environment.map(str::to_owned),
                include_local_override: true,
            },
            Some(&stores),
        )
        .map(|loaded| loaded.context)
        .map_err(|error| error.to_string())
    }

    fn environment_name(&self, id: Option<&str>) -> String {
        match id {
            None => "No environment".to_owned(),
            Some(id) => self
                .environments
                .iter()
                .find(|environment| environment.id.as_str() == id)
                .map_or_else(|| id.to_owned(), |environment| environment.name.clone()),
        }
    }
}

pub struct ApexShell {
    dock_area: Entity<DockArea>,
    collections_panel: Entity<CollectionsPanel>,
    _history_panel: Entity<HistoryPanel>,
    _workspace_tools_panel: Entity<WorkspaceToolsPanel>,
    _automation_panel: Entity<AutomationPanel>,
    _protocol_panel: Entity<ProtocolPanel>,
    _lifecycle_panel: Entity<LifecyclePanel>,
    request_panel: Entity<RequestPanel>,
    workspace_label: String,
    repository: Option<WorkspaceRepository>,
    environments: Vec<EnvironmentSummary>,
    selected_environment: Option<String>,
    environment_label: String,
    workspace_watch_status: String,
}

impl ApexShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_workspace(None, window, cx)
    }

    pub fn new_with_workspace(
        workspace_root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_workspace_and_environment(workspace_root, None, window, cx)
    }

    pub fn new_with_workspace_and_environment(
        workspace_root: Option<PathBuf>,
        initial_environment: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let browser = WorkspaceBrowser::open(workspace_root);
        let initial_document = browser.initial_document();
        let selected_environment =
            initial_environment.or_else(|| browser.default_environment.clone());
        let environment_label = browser.environment_name(selected_environment.as_deref());
        let (variable_context, variable_error) =
            match browser.variable_context(selected_environment.as_deref()) {
                Ok(context) => (context, None),
                Err(error) => (VariableContext::default(), Some(error)),
            };
        let response_panel = cx.new(ResponsePanel::new);
        let network = Arc::new(NetworkEngine::new());
        let request_panel = cx.new(|cx| {
            RequestPanel::new(
                RequestPanelInit {
                    response_panel: response_panel.clone(),
                    network: network.clone(),
                    initial_document,
                    variable_context,
                    environment_label: environment_label.clone(),
                    variable_error,
                },
                window,
                cx,
            )
        });
        let collections_panel =
            cx.new(|cx| CollectionsPanel::new(request_panel.clone(), browser.clone(), window, cx));
        let history_panel = cx.new(|cx| {
            HistoryPanel::new(
                request_panel.clone(),
                browser.repository.as_ref(),
                window,
                cx,
            )
        });
        let inspector_panel = cx.new(InspectorPanel::new);
        let automation_panel = cx.new(|cx| {
            AutomationPanel::new(
                browser.repository.clone(),
                request_panel.clone(),
                window,
                cx,
            )
        });
        let protocol_panel = cx.new(|cx| ProtocolPanel::new(window, cx));
        let lifecycle_panel = cx.new(|cx| {
            LifecyclePanel::new(
                browser.repository.clone(),
                request_panel.clone(),
                window,
                cx,
            )
        });
        let workspace_tools_panel = cx.new(|cx| {
            WorkspaceToolsPanel::new(
                browser.repository.clone(),
                request_panel.clone(),
                window,
                cx,
            )
        });
        let dock_area = cx.new(|cx| {
            DockArea::new("apex-main-dock", Some(1), window, cx).panel_style(PanelStyle::TabBar)
        });
        let dock_weak = dock_area.downgrade();

        let center = DockItem::tab(request_panel.clone(), &dock_weak, window, cx);
        let left = DockItem::tabs(
            vec![
                Arc::new(collections_panel.clone()),
                Arc::new(history_panel.clone()),
                Arc::new(automation_panel.clone()),
            ],
            &dock_weak,
            window,
            cx,
        );
        let right = DockItem::tabs(
            vec![
                Arc::new(inspector_panel),
                Arc::new(workspace_tools_panel.clone()),
                Arc::new(protocol_panel.clone()),
                Arc::new(lifecycle_panel.clone()),
            ],
            &dock_weak,
            window,
            cx,
        );
        let bottom = DockItem::tab(response_panel, &dock_weak, window, cx);

        dock_area.update(cx, |dock, cx| {
            dock.set_center(center, window, cx);
            dock.set_left_dock(left, Some(px(292.)), true, window, cx);
            dock.set_right_dock(right, Some(px(300.)), false, window, cx);
            dock.set_bottom_dock(bottom, Some(px(300.)), true, window, cx);
        });

        let (workspace_watch_status, monitor_receiver) = match browser.repository.clone() {
            Some(repository) => match start_workspace_monitor(repository) {
                Ok(receiver) => ("watching".to_owned(), Some(receiver)),
                Err(error) => (format!("watch error: {error}"), None),
            },
            None => ("not active".to_owned(), None),
        };
        if let Some(receiver) = monitor_receiver {
            cx.spawn(async move |this, cx| {
                while let Ok(message) = receiver.recv().await {
                    let Some(this) = this.upgrade() else {
                        break;
                    };
                    let _ = this.update(cx, |shell, cx| {
                        shell.apply_workspace_monitor_message(message, cx);
                    });
                }
            })
            .detach();
        }

        Self {
            dock_area,
            collections_panel,
            _history_panel: history_panel,
            _workspace_tools_panel: workspace_tools_panel,
            _automation_panel: automation_panel,
            _protocol_panel: protocol_panel,
            _lifecycle_panel: lifecycle_panel,
            request_panel,
            workspace_label: browser.label,
            repository: browser.repository,
            environments: browser.environments,
            selected_environment,
            environment_label,
            workspace_watch_status,
        }
    }

    fn apply_workspace_monitor_message(
        &mut self,
        message: WorkspaceMonitorMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            WorkspaceMonitorMessage::Update { change, requests } => {
                self.workspace_watch_status = "watching".to_owned();
                if let Some(requests) = requests {
                    self.collections_panel.update(cx, |panel, cx| {
                        panel.apply_request_index(requests, cx);
                    });
                }
                if let Some(repository) = self.repository.clone() {
                    self.request_panel.update(cx, |panel, cx| {
                        panel.observe_workspace_change(repository, &change, cx);
                    });
                }
            }
            WorkspaceMonitorMessage::Failed(error) => {
                self.workspace_watch_status = format!("watch error: {error}");
            }
        }
        cx.notify();
    }

    fn send_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_panel
            .update(cx, |panel, cx| panel.start_send(window, cx));
    }

    fn cancel_request(&mut self, cx: &mut Context<Self>) {
        self.request_panel.update(cx, RequestPanel::cancel);
    }

    fn save_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_panel
            .update(cx, |panel, cx| panel.save_with_notification(window, cx));
    }

    fn new_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_panel
            .update(cx, |panel, cx| panel.new_draft(window, cx));
        window.push_notification(Notification::info("Created a new local request draft"), cx);
    }

    fn activate_relative_request_tab(
        &mut self,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_panel.update(cx, |panel, cx| {
            panel.activate_relative_tab(direction, window, cx);
        });
    }

    fn reopen_closed_request_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_panel.update(cx, |panel, cx| {
            panel.reopen_closed_tab(window, cx);
        });
    }

    fn focus_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_panel
            .read(cx)
            .url_state
            .focus_handle(cx)
            .focus(window);
    }

    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, _, _| {
            dialog.title("ApexAPI Command Palette").child(
                gpui_component::v_flex()
                    .gap_1()
                    .children(COMMANDS.iter().copied().map(|command| {
                        Button::new(command.id)
                            .label(format!("{}    {}", command.label, command.binding))
                            .ghost()
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                command.kind.dispatch(window, cx);
                            })
                    })),
            )
        });
    }

    fn cycle_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.environments.is_empty() {
            window.push_notification(Notification::info("No environments are defined"), cx);
            return;
        }
        let next = self
            .selected_environment
            .as_deref()
            .and_then(|current| {
                self.environments
                    .iter()
                    .position(|environment| environment.id.as_str() == current)
            })
            .map_or_else(
                || Some(self.environments[0].id.as_str().to_owned()),
                |index| {
                    let next_index = (index + 1) % self.environments.len();
                    Some(self.environments[next_index].id.as_str().to_owned())
                },
            );
        let context = match (&self.repository, next.as_deref()) {
            (Some(repository), environment) => {
                let mut stores = SecretStoreChain::default();
                stores.push(Arc::new(EnvironmentSecretStore));
                load_workspace_variables(
                    repository,
                    &WorkspaceVariableSelection {
                        environment: environment.map(str::to_owned),
                        include_local_override: true,
                    },
                    Some(&stores),
                )
                .map(|loaded| loaded.context)
                .map_err(|error| error.to_string())
            }
            (None, _) => Ok(VariableContext::default()),
        };
        match context {
            Ok(context) => {
                self.selected_environment = next;
                self.environment_label = self
                    .selected_environment
                    .as_deref()
                    .and_then(|id| {
                        self.environments
                            .iter()
                            .find(|environment| environment.id.as_str() == id)
                    })
                    .map_or_else(
                        || "No environment".to_owned(),
                        |environment| environment.name.clone(),
                    );
                let label = self.environment_label.clone();
                self.request_panel.update(cx, |panel, cx| {
                    panel.set_environment(context, label.clone(), None, cx);
                });
                window.push_notification(
                    Notification::success(format!("Environment: {}", self.environment_label)),
                    cx,
                );
                cx.notify();
            }
            Err(error) => window.push_notification(
                Notification::error(format!("Environment switch failed: {error}")),
                cx,
            ),
        }
    }

    fn activity_bar(&self, cx: &App) -> impl IntoElement {
        gpui_component::v_flex()
            .w(px(48.))
            .h_full()
            .items_center()
            .py_2()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .w(px(36.))
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().accent)
                    .child(Icon::new(IconName::FolderOpen)),
            )
    }
}

impl Render for ApexShell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.request_panel.read(cx).is_running();
        let dirty = self.request_panel.read(cx).dirty;
        let external_attention = self.request_panel.read(cx).has_external_attention();
        gpui_component::v_flex()
            .id("apex-shell")
            .key_context(APP_CONTEXT)
            .on_action(cx.listener(|this, _: &NewRequest, window, cx| this.new_request(window, cx)))
            .on_action(
                cx.listener(|this, _: &SendRequest, window, cx| this.send_request(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CancelRequest, _, cx| this.cancel_request(cx)))
            .on_action(
                cx.listener(|this, _: &SaveRequest, window, cx| this.save_request(window, cx)),
            )
            .on_action(cx.listener(|this, _: &FocusUrl, window, cx| this.focus_url(window, cx)))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                this.open_command_palette(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CycleEnvironment, window, cx| {
                this.cycle_environment(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextRequestTab, window, cx| {
                this.activate_relative_request_tab(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousRequestTab, window, cx| {
                this.activate_relative_request_tab(-1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedRequestTab, window, cx| {
                this.reopen_closed_request_tab(window, cx)
            }))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new().child(
                    h_flex()
                        .size_full()
                        .gap_3()
                        .child(div().font_semibold().child("ApexAPI"))
                        .child(
                            div()
                                .px_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(self.workspace_label.clone()),
                        )
                        .child(
                            Button::new("environment-switcher")
                                .icon(IconName::Globe)
                                .label(self.environment_label.clone())
                                .ghost()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cycle_environment(window, cx);
                                })),
                        )
                        .child(
                            Button::new("global-search")
                                .icon(IconName::Search)
                                .label("Commands")
                                .ghost()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_command_palette(window, cx);
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if running {
                                    "Request running"
                                } else if external_attention {
                                    "External workspace change"
                                } else if dirty {
                                    "Unsaved draft"
                                } else {
                                    "Saved"
                                }),
                        ),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.activity_bar(cx))
                    .child(self.dock_area.clone()),
            )
            .child(
                h_flex()
                    .h(px(24.))
                    .px_3()
                    .gap_4()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .text_xs()
                    .child(format!("workspace: {}", self.workspace_label))
                    .child(format!("environment: {}", self.environment_label))
                    .child(if running {
                        "HTTP: running"
                    } else {
                        "HTTP: idle"
                    })
                    .child(format!("watch: {}", self.workspace_watch_status))
                    .child("proxy: direct")
                    .child("TLS: rustls")
                    .child(div().flex_1())
                    .child("UTF-8"),
            )
    }
}

#[derive(Default)]
struct BrowserNode {
    folders: BTreeMap<String, BrowserNode>,
    requests: Vec<WorkspaceRequestEntry>,
}

fn workspace_tree(entries: &[WorkspaceRequestEntry]) -> Vec<TreeItem> {
    let mut collections = BTreeMap::<String, BrowserNode>::new();
    for entry in entries {
        let mut node = collections.entry(entry.collection.clone()).or_default();
        for folder in &entry.folders {
            node = node.folders.entry(folder.clone()).or_default();
        }
        node.requests.push(entry.clone());
    }
    collections
        .into_iter()
        .map(|(name, node)| browser_folder_item(&name, &format!("collection:{name}"), node))
        .collect()
}

fn browser_folder_item(label: &str, id: &str, node: BrowserNode) -> TreeItem {
    let mut item = TreeItem::new(id.to_owned(), label.to_owned()).expanded(true);
    for (folder, child) in node.folders {
        item = item.child(browser_folder_item(
            &folder,
            &format!("folder:{id}/{folder}"),
            child,
        ));
    }
    for request in node.requests {
        item = item.child(TreeItem::new(
            request.relative_path.to_string_lossy().into_owned(),
            format!("{}  {}", request.method.as_str(), request.name),
        ));
    }
    item
}

fn workspace_browser_items(requests: &[WorkspaceRequestEntry]) -> Vec<TreeItem> {
    if requests.is_empty() {
        vec![
            TreeItem::new("local-drafts", "Local Drafts")
                .expanded(true)
                .child(TreeItem::new("gui-draft", "GUI Draft")),
        ]
    } else {
        workspace_tree(requests)
    }
}

fn workspace_segments(value: &str) -> Vec<String> {
    value
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| segment.trim().to_owned())
        .collect()
}

fn workspace_relative_path(
    repository: &WorkspaceRepository,
    value: &str,
) -> Result<PathBuf, String> {
    let relative = PathBuf::from(value.trim());
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(
            "workspace operation paths must be non-empty, relative, and cannot contain '..'"
                .to_owned(),
        );
    }
    let target = repository.root().join(relative);
    let canonical_root = fs::canonicalize(repository.root()).map_err(|error| error.to_string())?;
    let mut existing = target.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| "workspace output path has no existing ancestor".to_owned())?;
    }
    let canonical_existing = fs::canonicalize(existing).map_err(|error| error.to_string())?;
    if !canonical_existing.starts_with(&canonical_root) {
        return Err(format!(
            "workspace output path escapes through {}",
            existing.display()
        ));
    }
    Ok(target)
}

fn apply_workspace_operation(
    repository: &WorkspaceRepository,
    command: &str,
) -> Result<String, String> {
    let parts = command.split('|').map(str::trim).collect::<Vec<_>>();
    let operation = parts.first().copied().unwrap_or("");
    let part = |index: usize, name: &str| {
        parts
            .get(index)
            .copied()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{operation} is missing {name}"))
    };
    match operation {
        "create_collection" => {
            let slug = part(1, "slug")?;
            let id = StableId::parse(part(2, "id")?).map_err(|error| error.to_string())?;
            let name = part(3, "name")?;
            repository
                .create_collection(slug, &CollectionDocument::new(id, name))
                .map_err(|error| error.to_string())?;
            Ok(format!("Created collection '{slug}'"))
        }
        "rename_collection" => {
            let source = part(1, "source slug")?;
            let target = part(2, "target slug")?;
            let fingerprint = repository
                .collection_fingerprint(source)
                .map_err(|error| error.to_string())?;
            repository
                .rename_collection(source, target, fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Renamed collection '{source}' to '{target}'"))
        }
        "duplicate_collection" => {
            let source = part(1, "source slug")?;
            let target = part(2, "target slug")?;
            let fingerprint = repository
                .collection_fingerprint(source)
                .map_err(|error| error.to_string())?;
            repository
                .duplicate_collection(source, target, fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Duplicated collection '{source}' as '{target}'"))
        }
        "delete_collection" => {
            let slug = part(1, "slug")?;
            let fingerprint = repository
                .collection_fingerprint(slug)
                .map_err(|error| error.to_string())?;
            repository
                .delete_collection(slug, fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Deleted collection '{slug}'"))
        }
        "archive_collection" => {
            let slug = part(1, "slug")?;
            let archived = part(2, "true/false")?
                .parse::<bool>()
                .map_err(|_| "archive_collection requires true or false".to_owned())?;
            let loaded = repository
                .load_collection(slug)
                .map_err(|error| error.to_string())?;
            repository
                .set_collection_archived(slug, archived, loaded.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Set collection '{slug}' archived={archived}"))
        }
        "create_folder" => {
            let collection = part(1, "collection")?;
            let folders = workspace_segments(part(2, "folder path")?);
            let id = StableId::parse(part(3, "id")?).map_err(|error| error.to_string())?;
            let name = part(4, "name")?;
            repository
                .create_folder(collection, &folders, &FolderDocument::new(id, name))
                .map_err(|error| error.to_string())?;
            Ok(format!("Created folder '{}'/{}", collection, folders.join("/")))
        }
        "rename_folder" => {
            let collection = part(1, "collection")?;
            let folders = workspace_segments(part(2, "folder path")?);
            let target = part(3, "target slug")?;
            let fingerprint = repository
                .folder_fingerprint(collection, &folders)
                .map_err(|error| error.to_string())?;
            repository
                .rename_folder(collection, &folders, target, fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Renamed folder to '{target}'"))
        }
        "move_folder" | "duplicate_folder" => {
            let collection = part(1, "collection")?;
            let source = workspace_segments(part(2, "source folder path")?);
            let target_parent = workspace_segments(part(3, "target parent path")?);
            let target_slug = part(4, "target slug")?;
            let fingerprint = repository
                .folder_fingerprint(collection, &source)
                .map_err(|error| error.to_string())?;
            if operation == "move_folder" {
                repository
                    .move_folder(collection, &source, &target_parent, target_slug, fingerprint)
                    .map_err(|error| error.to_string())?;
            } else {
                repository
                    .duplicate_folder(collection, &source, &target_parent, target_slug, fingerprint)
                    .map_err(|error| error.to_string())?;
            }
            Ok(format!("{operation} completed"))
        }
        "delete_folder" => {
            let collection = part(1, "collection")?;
            let folders = workspace_segments(part(2, "folder path")?);
            let fingerprint = repository
                .folder_fingerprint(collection, &folders)
                .map_err(|error| error.to_string())?;
            repository
                .delete_folder(collection, &folders, fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Deleted folder '{}'/{}", collection, folders.join("/")))
        }
        "archive_folder" => {
            let collection = part(1, "collection")?;
            let folders = workspace_segments(part(2, "folder path")?);
            let archived = part(3, "true/false")?
                .parse::<bool>()
                .map_err(|_| "archive_folder requires true or false".to_owned())?;
            let loaded = repository
                .load_folder(collection, &folders)
                .map_err(|error| error.to_string())?;
            repository
                .set_folder_archived(collection, &folders, archived, loaded.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Set folder archived={archived}"))
        }
        "create_request" => {
            let target = workspace_relative_path(repository, part(1, "relative path")?)?;
            let id = StableId::parse(part(2, "id")?).map_err(|error| error.to_string())?;
            let name = part(3, "name")?;
            let url = part(4, "url")?;
            let mut request = default_request();
            request.id = id;
            request.name = name.to_owned();
            request.url = url.to_owned();
            repository
                .save_request(
                    &target,
                    &RequestDocument::new(request),
                    None,
                    &SecretLeakDetector::default(),
                )
                .map_err(|error| error.to_string())?;
            Ok(format!("Created request {}", target.display()))
        }
        "move_request" | "duplicate_request" => {
            let source = workspace_relative_path(repository, part(1, "source relative path")?)?;
            let target = workspace_relative_path(repository, part(2, "target relative path")?)?;
            let loaded = repository
                .load_request(&source)
                .map_err(|error| error.to_string())?;
            if operation == "move_request" {
                repository
                    .move_request(&source, &target, loaded.fingerprint)
                    .map_err(|error| error.to_string())?;
            } else {
                repository
                    .duplicate_request(&source, &target, loaded.fingerprint)
                    .map_err(|error| error.to_string())?;
            }
            Ok(format!("{operation} completed"))
        }
        "delete_request" => {
            let path = workspace_relative_path(repository, part(1, "relative path")?)?;
            let loaded = repository
                .load_request(&path)
                .map_err(|error| error.to_string())?;
            repository
                .delete_request(&path, loaded.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Deleted request {}", path.display()))
        }
        "set_order" => {
            let collection = part(1, "collection")?;
            let folders = workspace_segments(part(2, "folder path or '-' for collection root")?);
            let folders = if folders.as_slice() == ["-".to_owned()] {
                Vec::new()
            } else {
                folders
            };
            let items = part(3, "comma-separated items")?
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let expected = repository
                .load_order(Some(collection), &folders)
                .map_err(|error| error.to_string())?
                .map(|loaded| loaded.fingerprint);
            repository
                .save_order(Some(collection), &folders, &OrderDocument::new(items), expected)
                .map_err(|error| error.to_string())?;
            Ok("Updated collection ordering".to_owned())
        }
        _ => Err("unknown workspace operation; use create/rename/duplicate/delete/archive collection or folder, create/move/duplicate/delete request, or set_order".to_owned()),
    }
}

struct CollectionsPanel {
    focus_handle: FocusHandle,
    tree_state: Entity<TreeState>,
    request_panel: Entity<RequestPanel>,
    repository: Option<WorkspaceRepository>,
    request_paths: Arc<RwLock<HashMap<String, PathBuf>>>,
    mutation_state: Entity<InputState>,
    error: Option<String>,
}

impl CollectionsPanel {
    fn new(
        request_panel: Entity<RequestPanel>,
        browser: WorkspaceBrowser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let request_paths = Arc::new(RwLock::new(
            browser
                .requests
                .iter()
                .map(|entry| {
                    (
                        entry.relative_path.to_string_lossy().into_owned(),
                        entry.path.clone(),
                    )
                })
                .collect::<HashMap<_, _>>(),
        ));
        let items = workspace_browser_items(&browser.requests);
        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        let mutation_state = cx.new(|cx| {
            InputState::new(window, cx).placeholder("create_collection|users|users|Users")
        });
        Self {
            focus_handle: cx.focus_handle(),
            tree_state,
            request_panel,
            repository: browser.repository,
            request_paths,
            mutation_state,
            error: browser.error,
        }
    }

    fn apply_mutation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repository) = self.repository.clone() else {
            window.push_notification(Notification::error("No workspace is open"), cx);
            return;
        };
        let command = self.mutation_state.read(cx).value().to_string();
        match apply_workspace_operation(&repository, &command) {
            Ok(message) => {
                self.apply_request_index(
                    repository
                        .list_requests()
                        .map_err(|error| error.to_string()),
                    cx,
                );
                window.push_notification(Notification::success(message), cx);
            }
            Err(error) => {
                window.push_notification(
                    Notification::error(format!("Workspace operation failed: {error}")),
                    cx,
                );
            }
        }
    }

    fn apply_request_index(
        &mut self,
        requests: Result<Vec<WorkspaceRequestEntry>, String>,
        cx: &mut Context<Self>,
    ) {
        match requests {
            Ok(requests) => {
                if let Ok(mut request_paths) = self.request_paths.write() {
                    *request_paths = requests
                        .iter()
                        .map(|entry| {
                            (
                                entry.relative_path.to_string_lossy().into_owned(),
                                entry.path.clone(),
                            )
                        })
                        .collect();
                }
                let items = workspace_browser_items(&requests);
                self.tree_state
                    .update(cx, |state, cx| state.set_items(items, cx));
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
}

impl Focusable for CollectionsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for CollectionsPanel {}
impl Panel for CollectionsPanel {
    fn panel_name(&self) -> &'static str {
        "ApexCollectionsPanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Collections"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
}
impl Render for CollectionsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let request_paths = self.request_paths.clone();
        let repository = self.repository.clone();
        let request_panel = self.request_panel.clone();
        gpui_component::v_flex()
            .size_full()
            .gap_2()
            .child(
                Button::new("new-request")
                    .icon(IconName::Plus)
                    .label("New request")
                    .on_click({
                        let request_panel = self.request_panel.clone();
                        move |_, window, cx| {
                            request_panel.update(cx, |panel, cx| panel.new_draft(window, cx));
                            window.push_notification(
                                Notification::info("Created a new local request draft"),
                                cx,
                            );
                        }
                    }),
            )
            .child(
                gpui_component::v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Workspace operation: command|arg|arg. Operations use current fingerprints and fail on external changes."),
                    )
                    .child(Input::new(&self.mutation_state).cleanable(true))
                    .child(
                        Button::new("apply-workspace-operation")
                            .label("Apply workspace operation")
                            .disabled(self.repository.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.apply_mutation(window, cx);
                            })),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(div().flex_1().min_h_0().child(tree(
                &self.tree_state,
                move |ix, entry, selected, _, _| {
                    let icon = if entry.is_folder() {
                        if entry.is_expanded() {
                            IconName::FolderOpen
                        } else {
                            IconName::FolderClosed
                        }
                    } else {
                        IconName::File
                    };
                    let id = entry.item().id.to_string();
                    let mut item = ListItem::new(ix)
                        .pl(px(8. + entry.depth() as f32 * 16.))
                        .selected(selected)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Icon::new(icon).xsmall())
                                .child(entry.item().label.clone()),
                        );
                    let request_path = request_paths
                        .read()
                        .ok()
                        .and_then(|paths| paths.get(&id).cloned());
                    if !entry.is_folder()
                        && let (Some(repository), Some(path)) = (repository.clone(), request_path)
                    {
                        let request_panel = request_panel.clone();
                        item = item.on_click(move |_, window, cx| {
                            request_panel.update(cx, |panel, cx| {
                                match panel.open_document(
                                    DocumentStore::new(repository.clone(), path.clone()),
                                    window,
                                    cx,
                                ) {
                                    Ok(()) => window.push_notification(
                                        Notification::success(format!("Opened {}", path.display())),
                                        cx,
                                    ),
                                    Err(error) => window.push_notification(
                                        Notification::error(format!(
                                            "Request open failed: {error}"
                                        )),
                                        cx,
                                    ),
                                }
                            });
                        });
                    }
                    item
                },
            )))
            .border_color(cx.theme().border)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleSection {
    OpenApi,
    Mock,
    Git,
    Plugin,
    Ai,
}

impl LifecycleSection {
    const ALL: [Self; 5] = [Self::OpenApi, Self::Mock, Self::Git, Self::Plugin, Self::Ai];

    fn label(self) -> &'static str {
        match self {
            Self::OpenApi => "OpenAPI",
            Self::Mock => "Mock",
            Self::Git => "Git",
            Self::Plugin => "Plugins",
            Self::Ai => "AI",
        }
    }
}

struct UnavailablePluginExecutor;

impl apex_plugins::PluginExecutor for UnavailablePluginExecutor {
    fn invoke(&self, _: &[u8], _: ExtensionPoint, _: &[u8]) -> Result<Vec<u8>, String> {
        Err("no WebAssembly runtime adapter is configured in the desktop shell".to_owned())
    }
}

struct LocalConfirmationAiProvider;

impl apex_ai::AiProvider for LocalConfirmationAiProvider {
    fn send(&self, _: Option<&str>, request: &AiRequest) -> Result<serde_json::Value, String> {
        serde_json::to_value(request).map_err(|error| error.to_string())
    }
}

struct LifecyclePanel {
    focus_handle: FocusHandle,
    repository: Option<WorkspaceRepository>,
    request_panel: Entity<RequestPanel>,
    selected_section: LifecycleSection,
    command_state: Entity<InputState>,
    payload_state: Entity<InputState>,
    output: String,
    mock_server: Option<MockServer>,
    validated_plugin: Option<ValidatedPlugin>,
    ai_preview: Option<PayloadPreview>,
}

impl LifecyclePanel {
    fn new(
        repository: Option<WorkspaceRepository>,
        request_panel: Entity<RequestPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let command_state = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                "list, generate|operation|path|server, start, status, validate, preview",
            )
        });
        let payload_state = cx.new(|cx| InputState::new(window, cx).multi_line(true));
        Self {
            focus_handle: cx.focus_handle(),
            repository,
            request_panel,
            selected_section: LifecycleSection::OpenApi,
            command_state,
            payload_state,
            output: "Lifecycle tools operate through bounded, explicit backend APIs. Destructive Git and remote AI actions require direct commands and confirmation.".to_owned(),
            mock_server: None,
            validated_plugin: None,
            ai_preview: None,
        }
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.command_state.read(cx).value().to_string();
        let payload = self.payload_state.read(cx).value().to_string();
        let result = match self.selected_section {
            LifecycleSection::OpenApi => self.run_openapi(&command, &payload, window, cx),
            LifecycleSection::Mock => self.run_mock(&command, &payload),
            LifecycleSection::Git => self.run_git(&command),
            LifecycleSection::Plugin => self.run_plugin(&command, &payload),
            LifecycleSection::Ai => self.run_ai(&command, &payload),
        };
        match result {
            Ok(output) => self.output = output,
            Err(error) => window.push_notification(Notification::error(error), cx),
        }
        cx.notify();
    }

    fn run_openapi(
        &mut self,
        command: &str,
        payload: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let document =
            OpenApiDocument::parse(payload.as_bytes(), 8 * 1024 * 1024).map_err(|diagnostics| {
                diagnostics
                    .into_iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            })?;
        let parts = command.split('|').map(str::trim).collect::<Vec<_>>();
        match parts.first().copied().unwrap_or("") {
            "list" => serde_json::to_string_pretty(&document.operations())
                .map_err(|error| error.to_string()),
            "markdown" => Ok(document.markdown()),
            "generate" => {
                let repository = self
                    .repository
                    .clone()
                    .ok_or_else(|| "No workspace is open".to_owned())?;
                let operation = parts
                    .get(1)
                    .copied()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "generate requires operation id".to_owned())?;
                let relative_path = parts
                    .get(2)
                    .copied()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "generate requires destination request path".to_owned())?;
                let server = parts.get(3).copied().filter(|value| !value.is_empty());
                let generated = document.generate_request(operation, server)?;
                let mut request = default_request();
                let file_id = Path::new(relative_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".request.toml"))
                    .ok_or_else(|| "destination must end with .request.toml".to_owned())?;
                request.id = StableId::parse(file_id).map_err(|error| error.to_string())?;
                request.name = operation.to_owned();
                request.method =
                    HttpMethod::parse(&generated.method).map_err(|error| error.to_string())?;
                request.url = generated.url;
                request.headers = generated
                    .headers
                    .into_iter()
                    .map(|(name, value)| {
                        HeaderEntry::new(name, value).map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                request.body = generated.body.map_or(RequestBody::Empty, |body| {
                    RequestBody::Json(body.to_string())
                });
                request.documentation =
                    "Generated from OpenAPI. Review generated fields before committing.".to_owned();
                let path = workspace_relative_path(&repository, relative_path)?;
                let fingerprint = repository
                    .save_request(
                        &path,
                        &RequestDocument::new(request.clone()),
                        None,
                        &SecretLeakDetector::default(),
                    )
                    .map_err(|error| error.to_string())?;
                self.request_panel.update(cx, |panel, cx| {
                    panel.apply_request(
                        request,
                        DocumentStore::new(repository, path),
                        fingerprint,
                        window,
                        cx,
                    );
                });
                Ok(format!(
                    "Generated and opened OpenAPI operation '{operation}'"
                ))
            }
            other => Err(format!(
                "unknown OpenAPI command '{other}'; use list, markdown, or generate|operation|path|server"
            )),
        }
    }

    fn run_mock(&mut self, command: &str, payload: &str) -> Result<String, String> {
        match command.trim() {
            "start" => {
                if self.mock_server.is_some() {
                    return Err("mock server is already running".to_owned());
                }
                let routes: Vec<MockRoute> = serde_json::from_str(payload)
                    .map_err(|error| format!("invalid mock route JSON: {error}"))?;
                let server = MockServer::start(MockConfig::default(), routes)?;
                let address = server.address();
                self.mock_server = Some(server);
                Ok(format!("Mock server listening on http://{address}"))
            }
            "logs" => {
                let server = self
                    .mock_server
                    .as_ref()
                    .ok_or_else(|| "mock server is not running".to_owned())?;
                Ok(server
                    .logs()
                    .into_iter()
                    .map(|entry| format!("{} {} -> {}", entry.method, entry.path, entry.status))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "stop" => {
                let mut server = self
                    .mock_server
                    .take()
                    .ok_or_else(|| "mock server is not running".to_owned())?;
                server.shutdown();
                Ok("Mock server stopped".to_owned())
            }
            other => Err(format!(
                "unknown mock command '{other}'; use start, logs, or stop"
            )),
        }
    }

    fn run_git(&self, command: &str) -> Result<String, String> {
        let repository = self
            .repository
            .as_ref()
            .ok_or_else(|| "No workspace is open".to_owned())?;
        let git = GitRepository::discover(repository.root())?
            .ok_or_else(|| "workspace is not inside a Git repository".to_owned())?;
        let parts = command.split('|').map(str::trim).collect::<Vec<_>>();
        match parts.first().copied().unwrap_or("") {
            "status" => {
                let status = git.status()?;
                Ok(format!(
                    "branch={}\n{}",
                    status.branch.as_deref().unwrap_or("detached"),
                    status.entries.join("\n")
                ))
            }
            "diff" => git.diff(false),
            "diff staged" => git.diff(true),
            "stage" => {
                let path = parts
                    .get(1)
                    .copied()
                    .ok_or_else(|| "stage requires a relative path".to_owned())?;
                git.stage(Path::new(path))?;
                Ok(format!("Staged {path}"))
            }
            "commit" => {
                let message = parts
                    .get(1)
                    .copied()
                    .ok_or_else(|| "commit requires a message".to_owned())?;
                git.commit(message)
            }
            "switch" => {
                let branch = parts
                    .get(1)
                    .copied()
                    .ok_or_else(|| "switch requires a branch".to_owned())?;
                let allow_dirty = parts
                    .get(2)
                    .copied()
                    .unwrap_or("false")
                    .parse::<bool>()
                    .map_err(|_| "allow_dirty must be true or false".to_owned())?;
                git.switch(branch, allow_dirty)?;
                Ok(format!("Switched to {branch}"))
            }
            other => Err(format!("unknown Git command '{other}'")),
        }
    }

    fn run_plugin(&mut self, command: &str, payload: &str) -> Result<String, String> {
        let repository = self
            .repository
            .as_ref()
            .ok_or_else(|| "No workspace is open".to_owned())?;
        let parts = command.split('|').map(str::trim).collect::<Vec<_>>();
        match parts.first().copied().unwrap_or("") {
            "validate" => {
                let module_path = parts
                    .get(1)
                    .copied()
                    .ok_or_else(|| "validate requires module path".to_owned())?;
                let approved = parts
                    .get(2)
                    .copied()
                    .unwrap_or("")
                    .split(',')
                    .filter(|value| !value.trim().is_empty())
                    .map(parse_plugin_capability)
                    .collect::<Result<BTreeSet<_>, _>>()?;
                let manifest: PluginManifest = serde_json::from_str(payload)
                    .map_err(|error| format!("invalid plugin manifest JSON: {error}"))?;
                let path = workspace_relative_path(repository, module_path)?;
                let module = fs::read(&path).map_err(|error| error.to_string())?;
                let plugin =
                    validate_plugin(manifest, &module, &approved, &PluginLimits::default())?;
                let summary = format!(
                    "Validated plugin {} {} with {:?}",
                    plugin.manifest().id,
                    plugin.manifest().version,
                    plugin.manifest().capabilities
                );
                self.validated_plugin = Some(plugin);
                Ok(summary)
            }
            "invoke" => {
                let point = parts
                    .get(1)
                    .copied()
                    .ok_or_else(|| "invoke requires extension point".to_owned())?;
                let point = parse_extension_point(point)?;
                let plugin = self
                    .validated_plugin
                    .as_ref()
                    .ok_or_else(|| "validate a plugin first".to_owned())?;
                invoke_plugin(
                    plugin,
                    point,
                    payload.as_bytes(),
                    &PluginLimits::default(),
                    &UnavailablePluginExecutor,
                )
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            }
            other => Err(format!(
                "unknown plugin command '{other}'; use validate or invoke"
            )),
        }
    }

    fn run_ai(&mut self, command: &str, payload: &str) -> Result<String, String> {
        match command.trim() {
            "preview" => {
                let request: AiRequest = serde_json::from_str(payload)
                    .map_err(|error| format!("invalid AI request JSON: {error}"))?;
                let preview = preview_ai(&request, &[]);
                let output =
                    serde_json::to_string_pretty(&preview).map_err(|error| error.to_string())?;
                self.ai_preview = Some(preview);
                Ok(output)
            }
            confirmation if confirmation.starts_with("confirm:") => {
                let preview = self
                    .ai_preview
                    .as_ref()
                    .ok_or_else(|| "preview an AI payload first".to_owned())?;
                let config = AiConfig {
                    enabled: true,
                    provider: "local-confirmation-adapter".to_owned(),
                    endpoint: None,
                    allow_remote: false,
                };
                let response =
                    send_confirmed(&config, preview, confirmation, &LocalConfirmationAiProvider)?;
                serde_json::to_string_pretty(&response).map_err(|error| error.to_string())
            }
            other => Err(format!(
                "unknown AI command '{other}'; use preview, then the emitted confirm: token"
            )),
        }
    }
}

fn parse_plugin_capability(value: &str) -> Result<Capability, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "importer" => Ok(Capability::Importer),
        "exporter" => Ok(Capability::Exporter),
        "generator" => Ok(Capability::Generator),
        "assertion" => Ok(Capability::Assertion),
        "viewer" => Ok(Capability::Viewer),
        "authentication" => Ok(Capability::Authentication),
        other => Err(format!("unknown plugin capability '{other}'")),
    }
}

fn parse_extension_point(value: &str) -> Result<ExtensionPoint, String> {
    match parse_plugin_capability(value)? {
        Capability::Importer => Ok(ExtensionPoint::Importer),
        Capability::Exporter => Ok(ExtensionPoint::Exporter),
        Capability::Generator => Ok(ExtensionPoint::Generator),
        Capability::Assertion => Ok(ExtensionPoint::Assertion),
        Capability::Viewer => Ok(ExtensionPoint::Viewer),
        Capability::Authentication => Ok(ExtensionPoint::Authentication),
    }
}

impl Focusable for LifecyclePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for LifecyclePanel {}
impl Panel for LifecyclePanel {
    fn panel_name(&self) -> &'static str {
        "ApexLifecyclePanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Lifecycle"
    }
}
impl Render for LifecyclePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .gap_2()
            .child(
                TabBar::new("lifecycle-tabs")
                    .underline()
                    .selected_index(
                        LifecycleSection::ALL
                            .iter()
                            .position(|section| *section == self.selected_section)
                            .unwrap_or(0),
                    )
                    .children(
                        LifecycleSection::ALL
                            .into_iter()
                            .map(|section| Tab::new().label(section.label())),
                    )
                    .on_click(cx.listener(|this, index, _, cx| {
                        if let Some(section) = LifecycleSection::ALL.get(*index) {
                            this.selected_section = *section;
                            cx.notify();
                        }
                    })),
            )
            .child(Input::new(&self.command_state).cleanable(true))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Input::new(&self.payload_state).h_full()),
            )
            .child(
                Button::new("run-lifecycle-tool")
                    .label("Run lifecycle tool")
                    .on_click(cx.listener(|this, _, window, cx| this.run(window, cx))),
            )
            .child(div().text_sm().child(self.output.clone()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtocolToolSection {
    Graphql,
    Streams,
    Grpc,
}

impl ProtocolToolSection {
    const ALL: [Self; 3] = [Self::Graphql, Self::Streams, Self::Grpc];

    fn label(self) -> &'static str {
        match self {
            Self::Graphql => "GraphQL",
            Self::Streams => "WebSocket / SSE",
            Self::Grpc => "gRPC",
        }
    }
}

struct ProtocolPanel {
    focus_handle: FocusHandle,
    selected_section: ProtocolToolSection,
    command_state: Entity<InputState>,
    payload_state: Entity<InputState>,
    output: String,
    stream_log: Option<BoundedStreamLog>,
}

impl ProtocolPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let command_state = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                "validate, build, new websocket, connect, push incoming|message|data, export",
            )
        });
        let payload_state = cx.new(|cx| InputState::new(window, cx).multi_line(true));
        Self {
            focus_handle: cx.focus_handle(),
            selected_section: ProtocolToolSection::Graphql,
            command_state,
            payload_state,
            output: "Protocol tools validate and transform protocol documents. Live WebSocket/SSE/gRPC transports are not claimed by this UI surface.".to_owned(),
            stream_log: None,
        }
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.command_state.read(cx).value().to_string();
        let payload = self.payload_state.read(cx).value().to_string();
        let result = match self.selected_section {
            ProtocolToolSection::Graphql => self.run_graphql(&command, &payload),
            ProtocolToolSection::Streams => self.run_stream(&command),
            ProtocolToolSection::Grpc => self.run_grpc(&command, &payload),
        };
        match result {
            Ok(output) => self.output = output,
            Err(error) => window.push_notification(Notification::error(error), cx),
        }
        cx.notify();
    }

    fn run_graphql(&self, command: &str, payload: &str) -> Result<String, String> {
        let request: GraphqlRequest = serde_json::from_str(payload)
            .map_err(|error| format!("invalid GraphQL request JSON: {error}"))?;
        match command.trim() {
            "validate" => validate_graphql_request(&request)
                .map(|kind| format!("Valid GraphQL operation: {kind:?}"))
                .map_err(|errors| errors.join("\n")),
            "build" => build_graphql_http_request(&request)
                .map_err(|errors| errors.join("\n"))
                .and_then(|request| {
                    serde_json::to_string_pretty(&request).map_err(|error| error.to_string())
                }),
            other => Err(format!(
                "unknown GraphQL command '{other}'; use validate or build"
            )),
        }
    }

    fn run_stream(&mut self, command: &str) -> Result<String, String> {
        let parts = command.split('|').map(str::trim).collect::<Vec<_>>();
        match parts.first().copied().unwrap_or("") {
            "new websocket" => {
                self.stream_log = Some(BoundedStreamLog::new(
                    StreamProtocol::WebSocket,
                    1000,
                    1024 * 1024,
                )?);
                Ok("Created bounded WebSocket session log".to_owned())
            }
            "new sse" => {
                self.stream_log = Some(BoundedStreamLog::new(
                    StreamProtocol::ServerSentEvents,
                    1000,
                    1024 * 1024,
                )?);
                Ok("Created bounded SSE session log".to_owned())
            }
            "connect" => {
                self.stream_log
                    .as_mut()
                    .ok_or_else(|| "create a stream log first".to_owned())?
                    .connect(&CancellationToken::default())?;
                Ok("Stream session marked connected".to_owned())
            }
            "disconnect" => {
                let reason = parts.get(1).copied().unwrap_or("user requested");
                self.stream_log
                    .as_mut()
                    .ok_or_else(|| "create a stream log first".to_owned())?
                    .disconnect(reason)?;
                Ok("Stream session disconnected".to_owned())
            }
            "push incoming" | "push outgoing" | "push system" => {
                let direction = match parts[0] {
                    "push incoming" => StreamDirection::Incoming,
                    "push outgoing" => StreamDirection::Outgoing,
                    _ => StreamDirection::System,
                };
                let event_type = parts
                    .get(1)
                    .filter(|value| !value.is_empty())
                    .map(|value| (*value).to_owned());
                let data = parts.get(2).copied().unwrap_or("").as_bytes().to_vec();
                self.stream_log
                    .as_mut()
                    .ok_or_else(|| "create a stream log first".to_owned())?
                    .push(direction, event_type, data)?;
                Ok("Stream event appended".to_owned())
            }
            "filter" => {
                let needle = parts.get(1).copied().unwrap_or("");
                let log = self
                    .stream_log
                    .as_ref()
                    .ok_or_else(|| "create a stream log first".to_owned())?;
                serde_json::to_string_pretty(
                    &log.filtered(needle)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| error.to_string())
            }
            "export" => {
                let log = self
                    .stream_log
                    .as_ref()
                    .ok_or_else(|| "create a stream log first".to_owned())?;
                serde_json::to_string_pretty(&log.export()).map_err(|error| error.to_string())
            }
            other => Err(format!("unknown stream command '{other}'")),
        }
    }

    fn run_grpc(&self, command: &str, payload: &str) -> Result<String, String> {
        if let Some(service) = command.trim().strip_prefix("reflection|") {
            return reflection_request(service).and_then(|value| {
                serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
            });
        }
        let request: GrpcRequest = serde_json::from_str(payload)
            .map_err(|error| format!("invalid gRPC request JSON: {error}"))?;
        match command.trim() {
            "validate" => validate_grpc_request(&request)
                .map(|()| "Valid gRPC request descriptor".to_owned())
                .map_err(|errors| errors.join("\n")),
            other => Err(format!(
                "unknown gRPC command '{other}'; use validate or reflection|service"
            )),
        }
    }
}

impl Focusable for ProtocolPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for ProtocolPanel {}
impl Panel for ProtocolPanel {
    fn panel_name(&self) -> &'static str {
        "ApexProtocolPanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Protocols"
    }
}
impl Render for ProtocolPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .gap_2()
            .child(
                TabBar::new("protocol-tool-tabs")
                    .underline()
                    .selected_index(
                        ProtocolToolSection::ALL
                            .iter()
                            .position(|section| *section == self.selected_section)
                            .unwrap_or(0),
                    )
                    .children(
                        ProtocolToolSection::ALL
                            .into_iter()
                            .map(|section| Tab::new().label(section.label())),
                    )
                    .on_click(cx.listener(|this, index, _, cx| {
                        if let Some(section) = ProtocolToolSection::ALL.get(*index) {
                            this.selected_section = *section;
                            cx.notify();
                        }
                    })),
            )
            .child(Input::new(&self.command_state).cleanable(true))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Input::new(&self.payload_state).h_full()),
            )
            .child(
                Button::new("run-protocol-tool")
                    .label("Run protocol tool")
                    .on_click(cx.listener(|this, _, window, cx| this.run(window, cx))),
            )
            .child(div().text_sm().child(self.output.clone()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AutomationConfig {
    concurrency: usize,
    retries: usize,
    failure_policy: FailurePolicy,
    report: String,
}

impl AutomationConfig {
    fn parse(value: &str) -> Result<Self, String> {
        let fields = value
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                line.split_once('=')
                    .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
                    .ok_or_else(|| format!("automation setting '{line}' must use key=value"))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let required = |key: &str| {
            fields
                .get(key)
                .cloned()
                .ok_or_else(|| format!("automation settings are missing '{key}'"))
        };
        let concurrency = required("concurrency")?
            .parse::<usize>()
            .map_err(|_| "concurrency must be an integer".to_owned())?;
        if !(1..=64).contains(&concurrency) {
            return Err("concurrency must be between 1 and 64".to_owned());
        }
        let retries = required("retries")?
            .parse::<usize>()
            .map_err(|_| "retries must be an integer".to_owned())?
            .min(10);
        let failure_policy = match required("failure_policy")?.as_str() {
            "continue" => FailurePolicy::Continue,
            "stop" => FailurePolicy::Stop,
            other => return Err(format!("unknown failure_policy '{other}'")),
        };
        let report = required("report")?;
        if !matches!(report.as_str(), "json" | "junit" | "html") {
            return Err("report must be json, junit, or html".to_owned());
        }
        Ok(Self {
            concurrency,
            retries,
            failure_policy,
            report,
        })
    }
}

fn format_run_summary(summary: &RunSummary, format: &str) -> Result<String, String> {
    match format {
        "json" => summary.to_json().map_err(|error| error.to_string()),
        "junit" => Ok(summary.to_junit("ApexAPI Workspace Run")),
        "html" => Ok(summary.to_html("ApexAPI Workspace Run")),
        other => Err(format!("unsupported report format '{other}'")),
    }
}

struct IgnoreExecutionEvents;

impl ExecutionEventSink for IgnoreExecutionEvents {
    fn emit(&self, _: ExecutionEvent) {}
}

struct AutomationPanel {
    focus_handle: FocusHandle,
    repository: Option<WorkspaceRepository>,
    request_panel: Entity<RequestPanel>,
    config_state: Entity<InputState>,
    output: String,
    cancellation: Option<CancellationToken>,
}

impl AutomationPanel {
    fn new(
        repository: Option<WorkspaceRepository>,
        request_panel: Entity<RequestPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config_state = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value("concurrency=2\nretries=0\nfailure_policy=continue\nreport=json")
        });
        Self {
            focus_handle: cx.focus_handle(),
            repository,
            request_panel,
            config_state,
            output: "Run a workspace collection to inspect live progress and reports.".to_owned(),
            cancellation: None,
        }
    }

    fn start_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cancellation.is_some() {
            window.push_notification(Notification::info("A collection run is already active"), cx);
            return;
        }
        let Some(repository) = self.repository.clone() else {
            window.push_notification(Notification::error("No workspace is open"), cx);
            return;
        };
        let config = match AutomationConfig::parse(&self.config_state.read(cx).value()) {
            Ok(config) => config,
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        let (variable_context, variable_error) = {
            let panel = self.request_panel.read(cx);
            (panel.variable_context.clone(), panel.variable_error.clone())
        };
        if let Some(error) = variable_error {
            window.push_notification(Notification::error(error), cx);
            return;
        }
        let mut items = Vec::new();
        let mut requests = HashMap::new();
        let entries = match repository.list_requests() {
            Ok(entries) => entries,
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()), cx);
                return;
            }
        };
        for entry in entries {
            let loaded = match repository.load_request(&entry.path) {
                Ok(loaded) => loaded,
                Err(error) => {
                    window.push_notification(Notification::error(error.to_string()), cx);
                    return;
                }
            };
            let request = match resolve_http_request(
                &loaded.value.request,
                &variable_context,
                &SystemDynamicVariables,
                ResolverOptions::default(),
            ) {
                Ok(request) => request.request,
                Err(error) => {
                    window.push_notification(
                        Notification::error(format!("{}: {error}", entry.name)),
                        cx,
                    );
                    return;
                }
            };
            let id = entry.relative_path.to_string_lossy().into_owned();
            items.push(RunItem {
                id: id.clone(),
                name: entry.name,
                iteration_data: BTreeMap::new(),
            });
            requests.insert(id, request);
        }
        if items.is_empty() {
            window.push_notification(Notification::info("The workspace has no requests"), cx);
            return;
        }
        let cancellation = CancellationToken::default();
        self.cancellation = Some(cancellation.clone());
        self.output = format!("Running {} request(s)…", items.len());
        cx.notify();
        let (sender, receiver) = async_channel::bounded(1);
        let root = repository.root().to_owned();
        thread::spawn(move || {
            let requests = Arc::new(requests);
            let adapter = Arc::new(HttpAdapter::new());
            let executor: Arc<dyn apex_runner::ItemExecutor> =
                Arc::new(move |item: &RunItem, cancellation: &CancellationToken| {
                    let request = requests
                        .get(&item.id)
                        .cloned()
                        .ok_or_else(|| format!("request '{}' is unavailable", item.id))?;
                    let mut context = ExecutionContext::new(
                        request.settings.timeout,
                        request.settings.maximum_response_bytes,
                    );
                    context.resource_root = Some(root.clone());
                    context.cancellation = cancellation.clone();
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    let sink: Arc<dyn ExecutionEventSink> = Arc::new(IgnoreExecutionEvents);
                    let started = std::time::Instant::now();
                    let result = runtime
                        .block_on(adapter.execute(
                            ResolvedRequest {
                                request: ProtocolRequest::Http(request),
                                redacted_summary: item.name.clone(),
                            },
                            context,
                            sink,
                        ))
                        .map_err(|error| error.to_string())?;
                    let status = result.response.status.unwrap_or(0);
                    Ok(ItemExecution {
                        passed: (200..400).contains(&status),
                        message: format!("HTTP {status}"),
                        duration_ms: u64::try_from(started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                    })
                });
            let run_config = RunConfig {
                concurrency: config.concurrency,
                retries: config.retries,
                retry_backoff: std::time::Duration::from_millis(100),
                failure_policy: config.failure_policy,
                cookie_policy: CookiePolicy::Shared,
            };
            let events: Arc<dyn apex_runner::RunEventSink> =
                Arc::new(|_: apex_runner::RunEvent| {});
            let result = run_collection(items, run_config, cancellation, executor, events)
                .and_then(|summary| format_run_summary(&summary, &config.report));
            let _ = sender.send_blocking(result);
        });
        cx.spawn(async move |this, cx| {
            let result = receiver.recv().await;
            if let Some(this) = this.upgrade() {
                let _ = this.update(cx, |panel, cx| {
                    panel.cancellation = None;
                    panel.output = match result {
                        Ok(Ok(report)) => report,
                        Ok(Err(error)) => format!("Collection run failed: {error}"),
                        Err(error) => format!("Collection run channel failed: {error}"),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn cancel_run(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
            self.output = "Cancellation requested…".to_owned();
            cx.notify();
        }
    }
}

impl Focusable for AutomationPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for AutomationPanel {}
impl Panel for AutomationPanel {
    fn panel_name(&self) -> &'static str {
        "ApexAutomationPanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Automation"
    }
}
impl Render for AutomationPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .child("Real workspace runner: bounded concurrency, retries, cancellation, and JSON/JUnit/HTML reports."),
            )
            .child(Input::new(&self.config_state))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("run-workspace-collection")
                            .label(if self.cancellation.is_some() { "Running" } else { "Run" })
                            .disabled(self.cancellation.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_run(window, cx);
                            })),
                    )
                    .when(self.cancellation.is_some(), |row| {
                        row.child(
                            Button::new("cancel-workspace-collection")
                                .label("Cancel")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_run(cx);
                                })),
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().text_sm().child(self.output.clone()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceToolSection {
    Environments,
    Search,
    Import,
    Codegen,
    Settings,
}

impl WorkspaceToolSection {
    const ALL: [Self; 5] = [
        Self::Environments,
        Self::Search,
        Self::Import,
        Self::Codegen,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Environments => "Environments",
            Self::Search => "Search",
            Self::Import => "Import",
            Self::Codegen => "Codegen",
            Self::Settings => "Settings",
        }
    }
}

fn environment_sensitivity(value: &str) -> Result<ValueSensitivity, String> {
    match value {
        "public" => Ok(ValueSensitivity::Public),
        "sensitive" => Ok(ValueSensitivity::Sensitive),
        "secret" => Ok(ValueSensitivity::Secret),
        other => Err(format!("unknown sensitivity '{other}'")),
    }
}

fn apply_environment_operation(
    repository: &WorkspaceRepository,
    command: &str,
) -> Result<String, String> {
    let parts = command.split('|').map(str::trim).collect::<Vec<_>>();
    let operation = parts.first().copied().unwrap_or("");
    let part = |index: usize, name: &str| {
        parts
            .get(index)
            .copied()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{operation} is missing {name}"))
    };
    match operation {
        "create" => {
            let id = StableId::parse(part(1, "id")?).map_err(|error| error.to_string())?;
            let name = part(2, "name")?;
            repository
                .create_environment(&VariableSetDocument::new(id.clone(), name))
                .map_err(|error| error.to_string())?;
            Ok(format!("Created environment '{id}'"))
        }
        "rename" => {
            let id = StableId::parse(part(1, "id")?).map_err(|error| error.to_string())?;
            let name = part(2, "name")?;
            let loaded = repository
                .load_environment(&id)
                .map_err(|error| error.to_string())?;
            repository
                .rename_environment(&id, name, loaded.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Renamed environment '{id}'"))
        }
        "delete" => {
            let id = StableId::parse(part(1, "id")?).map_err(|error| error.to_string())?;
            let loaded = repository
                .load_environment(&id)
                .map_err(|error| error.to_string())?;
            repository
                .delete_environment(&id, loaded.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Deleted environment '{id}'"))
        }
        "default" => {
            let manifest = repository
                .load_manifest()
                .map_err(|error| error.to_string())?;
            let id = match part(1, "id or none")? {
                "none" => None,
                value => Some(StableId::parse(value).map_err(|error| error.to_string())?),
            };
            repository
                .set_default_environment(id.as_ref(), manifest.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "Default environment set to {}",
                id.as_ref().map_or("none", StableId::as_str)
            ))
        }
        "set" => {
            let id =
                StableId::parse(part(1, "environment id")?).map_err(|error| error.to_string())?;
            let variable_name = part(2, "variable name")?;
            let source_kind = part(3, "source kind")?;
            let source_value = part(4, "source value")?;
            let sensitivity = environment_sensitivity(part(5, "sensitivity")?)?;
            let enabled = part(6, "enabled flag")?
                .parse::<bool>()
                .map_err(|_| "enabled flag must be true or false".to_owned())?;
            let source = match source_kind {
                "literal" => {
                    if sensitivity == ValueSensitivity::Secret {
                        return Err(
                            "secret variables must use a secret reference, not a literal"
                                .to_owned(),
                        );
                    }
                    StoredVariableSource::Literal(VariableValue::String(source_value.to_owned()))
                }
                "env" => StoredVariableSource::ProcessEnvironment {
                    name: source_value.to_owned(),
                },
                "secret" => {
                    let (namespace, name) = source_value
                        .split_once('/')
                        .ok_or_else(|| "secret source must use namespace/name".to_owned())?;
                    StoredVariableSource::Secret(
                        SecretRef::new(namespace, name).map_err(|error| error.to_string())?,
                    )
                }
                other => return Err(format!("unknown variable source kind '{other}'")),
            };
            let loaded = repository
                .load_environment(&id)
                .map_err(|error| error.to_string())?;
            let mut document = loaded.value;
            document
                .variables
                .retain(|variable| variable.name != variable_name);
            document.variables.push(StoredVariable {
                name: variable_name.to_owned(),
                source,
                sensitivity,
                enabled,
                description: None,
            });
            repository
                .update_environment(&document, loaded.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Updated variable '{variable_name}' in '{id}'"))
        }
        "remove" => {
            let id =
                StableId::parse(part(1, "environment id")?).map_err(|error| error.to_string())?;
            let variable_name = part(2, "variable name")?;
            let loaded = repository
                .load_environment(&id)
                .map_err(|error| error.to_string())?;
            let mut document = loaded.value;
            let before = document.variables.len();
            document
                .variables
                .retain(|variable| variable.name != variable_name);
            if document.variables.len() == before {
                return Err(format!("variable '{variable_name}' was not found"));
            }
            repository
                .update_environment(&document, loaded.fingerprint)
                .map_err(|error| error.to_string())?;
            Ok(format!("Removed variable '{variable_name}' from '{id}'"))
        }
        _ => Err(
            "environment command must be create, rename, delete, default, set, or remove"
                .to_owned(),
        ),
    }
}

fn code_target(value: &str) -> Result<CodeTarget, String> {
    match value.trim() {
        "curl" => Ok(CodeTarget::Curl),
        "httpie" => Ok(CodeTarget::Httpie),
        "rust-reqwest" => Ok(CodeTarget::RustReqwest),
        "python-requests" => Ok(CodeTarget::PythonRequests),
        "go-net-http" => Ok(CodeTarget::GoNetHttp),
        other => Err(format!("unknown code target '{other}'")),
    }
}

struct WorkspaceToolsPanel {
    focus_handle: FocusHandle,
    repository: Option<WorkspaceRepository>,
    request_panel: Entity<RequestPanel>,
    selected_section: WorkspaceToolSection,
    environment_state: Entity<InputState>,
    search_state: Entity<InputState>,
    import_format_state: Entity<InputState>,
    import_destination_state: Entity<InputState>,
    import_source_state: Entity<InputState>,
    codegen_target_state: Entity<InputState>,
    codegen_destination_state: Entity<InputState>,
    settings_state: Entity<InputState>,
    search_results: Vec<SearchResult>,
    import_preview: Option<ImportPreview>,
    generated_code: Option<String>,
    output: String,
}

impl WorkspaceToolsPanel {
    fn new(
        repository: Option<WorkspaceRepository>,
        request_panel: Entity<RequestPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let import_source_state = cx.new(|cx| InputState::new(window, cx).multi_line(true));
        let settings_state = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value("maximum_visible_tabs=6\nreduced_motion=false\nhigh_contrast=false")
        });
        Self {
            focus_handle: cx.focus_handle(),
            repository,
            request_panel,
            selected_section: WorkspaceToolSection::Environments,
            environment_state: input("create|development|Development", window, cx),
            search_state: input("request name, URL, header, or body text", window, cx),
            import_format_state: input("curl or postman-v2.1", window, cx),
            import_destination_state: input("collections/imported", window, cx),
            import_source_state,
            codegen_target_state: input("curl", window, cx),
            codegen_destination_state: input(".apex/generated/request.sh", window, cx),
            settings_state,
            search_results: Vec::new(),
            import_preview: None,
            generated_code: None,
            output: String::new(),
        }
    }

    fn run_environment_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repository) = &self.repository else {
            window.push_notification(Notification::error("No workspace is open"), cx);
            return;
        };
        let command = self.environment_state.read(cx).value().to_string();
        match apply_environment_operation(repository, &command) {
            Ok(message) => {
                self.output = message.clone();
                window.push_notification(Notification::success(message), cx);
            }
            Err(error) => window.push_notification(Notification::error(error), cx),
        }
        cx.notify();
    }

    fn run_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repository) = &self.repository else {
            window.push_notification(Notification::error("No workspace is open"), cx);
            return;
        };
        let query = self.search_state.read(cx).value().to_string();
        let result = WorkspaceSearchIndex::open(repository, SearchIndexPolicy::default()).and_then(
            |mut index| {
                index.refresh(repository)?;
                index.search(&SearchQuery {
                    text: query,
                    limit: Some(100),
                    ..SearchQuery::default()
                })
            },
        );
        match result {
            Ok(results) => {
                self.output = format!("{} search result(s)", results.len());
                self.search_results = results;
            }
            Err(error) => {
                self.search_results.clear();
                window.push_notification(Notification::error(error.to_string()), cx);
            }
        }
        cx.notify();
    }

    fn open_search_result(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repository) = self.repository.clone() else {
            return;
        };
        let Some(result) = self.search_results.get(index) else {
            return;
        };
        let path = repository.root().join(&result.relative_path);
        self.request_panel.update(cx, |panel, cx| {
            if let Err(error) =
                panel.open_document(DocumentStore::new(repository, path), window, cx)
            {
                window.push_notification(Notification::error(error), cx);
            }
        });
    }

    fn preview_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let format = self.import_format_state.read(cx).value().to_string();
        let source = self.import_source_state.read(cx).value().to_string();
        let preview = match format.trim() {
            "curl" => parse_curl(&source),
            "postman-v2.1" => parse_postman_v21(source.as_bytes()),
            other => {
                window.push_notification(
                    Notification::error(format!("Unknown import format '{other}'")),
                    cx,
                );
                return;
            }
        };
        match preview {
            Ok(preview) => {
                let diagnostics = preview
                    .diagnostics
                    .iter()
                    .map(|diagnostic| {
                        format!(
                            "{:?} {}: {}",
                            diagnostic.severity, diagnostic.code, diagnostic.message
                        )
                    })
                    .chain(
                        preview
                            .unsupported_fields
                            .iter()
                            .map(|field| format!("Unsupported: {field}")),
                    )
                    .collect::<Vec<_>>()
                    .join("\n");
                self.output = format!(
                    "{} request(s) from {}\n{}",
                    preview.requests.len(),
                    preview.source_format,
                    diagnostics
                );
                self.import_preview = Some(preview);
            }
            Err(error) => window.push_notification(Notification::error(error.to_string()), cx),
        }
        cx.notify();
    }

    fn apply_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repository) = &self.repository else {
            window.push_notification(Notification::error("No workspace is open"), cx);
            return;
        };
        let Some(preview) = self.import_preview.clone() else {
            window.push_notification(Notification::error("Preview the import first"), cx);
            return;
        };
        if preview.has_errors() {
            window.push_notification(
                Notification::error("Import diagnostics contain errors; no files were written"),
                cx,
            );
            return;
        }
        let destination = match workspace_relative_path(
            repository,
            &self.import_destination_state.read(cx).value(),
        ) {
            Ok(destination) => destination,
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        if let Err(error) = fs::create_dir_all(&destination) {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        let mut written = 0usize;
        for document in preview.requests {
            let path = destination.join(format!("{}.request.toml", document.request.id.as_str()));
            match repository.save_request(&path, &document, None, &SecretLeakDetector::default()) {
                Ok(_) => written += 1,
                Err(error) => {
                    window.push_notification(Notification::error(error.to_string()), cx);
                    return;
                }
            }
        }
        self.output = format!(
            "Imported {written} request(s) into {}",
            destination.display()
        );
        window.push_notification(Notification::success(self.output.clone()), cx);
        cx.notify();
    }

    fn generate_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = match code_target(&self.codegen_target_state.read(cx).value()) {
            Ok(target) => target,
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        let request = match self.request_panel.read(cx).try_current_request(cx) {
            Ok(request) => request,
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        match generate_code(&request, target, CodegenOptions::default()) {
            Ok(snippet) => {
                self.output = snippet
                    .warnings
                    .iter()
                    .map(|warning| format!("Warning: {warning}"))
                    .chain(std::iter::once(snippet.code.clone()))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.generated_code = Some(snippet.code);
            }
            Err(error) => window.push_notification(Notification::error(error.to_string()), cx),
        }
        cx.notify();
    }

    fn save_generated_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repository) = &self.repository else {
            window.push_notification(Notification::error("No workspace is open"), cx);
            return;
        };
        let Some(code) = &self.generated_code else {
            window.push_notification(Notification::error("Generate code first"), cx);
            return;
        };
        let path = match workspace_relative_path(
            repository,
            &self.codegen_destination_state.read(cx).value(),
        ) {
            Ok(path) => path,
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        if path.exists() {
            window.push_notification(
                Notification::error(format!("Refusing to overwrite {}", path.display())),
                cx,
            );
            return;
        }
        if let Some(parent) = path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        match fs::write(&path, code) {
            Ok(()) => window.push_notification(
                Notification::success(format!("Saved generated code to {}", path.display())),
                cx,
            ),
            Err(error) => window.push_notification(Notification::error(error.to_string()), cx),
        }
    }

    fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = match tab_session_state_path() {
            Ok(path) => path.with_file_name("ui-preferences.txt"),
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        if let Some(parent) = path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        let preferences = match UiPreferences::parse(&self.settings_state.read(cx).value()) {
            Ok(preferences) => preferences,
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        match fs::write(&path, preferences.format()) {
            Ok(()) => {
                self.request_panel.update(cx, |panel, cx| {
                    panel.maximum_visible_tabs = preferences.maximum_visible_tabs;
                    cx.notify();
                });
                window.push_notification(
                    Notification::success(format!("Saved UI preferences to {}", path.display())),
                    cx,
                )
            }
            Err(error) => window.push_notification(Notification::error(error.to_string()), cx),
        }
    }

    fn section_content(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.selected_section {
            WorkspaceToolSection::Environments => gpui_component::v_flex()
                .gap_2()
                .child(div().text_sm().child("Commands: create|id|name; rename|id|name; delete|id; default|id|none; set|id|name|literal|env|secret|value|sensitivity|enabled; remove|id|name"))
                .child(Input::new(&self.environment_state).cleanable(true))
                .child(div().text_sm().child(self.output.clone()))
                .into_any_element(),
            WorkspaceToolSection::Search => gpui_component::v_flex()
                .gap_2()
                .child(Input::new(&self.search_state).cleanable(true))
                .children(self.search_results.clone().into_iter().enumerate().map(|(index, result)| {
                    Button::new(("workspace-search-result", index))
                        .label(format!("{} {} — {}", result.method, result.name, result.url))
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_search_result(index, window, cx);
                        }))
                }))
                .child(div().text_sm().child(self.output.clone()))
                .into_any_element(),
            WorkspaceToolSection::Import => gpui_component::v_flex()
                .gap_2()
                .child(Input::new(&self.import_format_state).cleanable(true))
                .child(Input::new(&self.import_destination_state).cleanable(true))
                .child(div().flex_1().min_h_0().child(Input::new(&self.import_source_state).h_full()))
                .child(div().text_sm().child(self.output.clone()))
                .into_any_element(),
            WorkspaceToolSection::Codegen => gpui_component::v_flex()
                .gap_2()
                .child(div().text_sm().child("Targets: curl, httpie, rust-reqwest, python-requests, go-net-http. Sensitive values are redacted."))
                .child(Input::new(&self.codegen_target_state).cleanable(true))
                .child(Input::new(&self.codegen_destination_state).cleanable(true))
                .child(div().text_sm().child(self.output.clone()))
                .into_any_element(),
            WorkspaceToolSection::Settings => gpui_component::v_flex()
                .gap_2()
                .child(div().text_sm().child("Persisted UI preferences. Settings unsupported by the current platform remain inert rather than being silently claimed."))
                .child(div().flex_1().min_h_0().child(Input::new(&self.settings_state).h_full()))
                .into_any_element(),
        }
    }
}

impl Focusable for WorkspaceToolsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for WorkspaceToolsPanel {}
impl Panel for WorkspaceToolsPanel {
    fn panel_name(&self) -> &'static str {
        "ApexWorkspaceToolsPanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Workspace Tools"
    }
}
impl Render for WorkspaceToolsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .gap_2()
            .child(
                TabBar::new("workspace-tools-tabs")
                    .underline()
                    .selected_index(
                        WorkspaceToolSection::ALL
                            .iter()
                            .position(|section| *section == self.selected_section)
                            .unwrap_or(0),
                    )
                    .children(
                        WorkspaceToolSection::ALL
                            .into_iter()
                            .map(|section| Tab::new().label(section.label())),
                    )
                    .on_click(cx.listener(|this, index, _, cx| {
                        if let Some(section) = WorkspaceToolSection::ALL.get(*index) {
                            this.selected_section = *section;
                            cx.notify();
                        }
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .when(
                        self.selected_section == WorkspaceToolSection::Environments,
                        |row| {
                            row.child(
                                Button::new("apply-environment-operation")
                                    .label("Apply")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.run_environment_operation(window, cx);
                                    })),
                            )
                        },
                    )
                    .when(
                        self.selected_section == WorkspaceToolSection::Search,
                        |row| {
                            row.child(
                                Button::new("run-workspace-search")
                                    .label("Search")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.run_search(window, cx);
                                    })),
                            )
                        },
                    )
                    .when(
                        self.selected_section == WorkspaceToolSection::Import,
                        |row| {
                            row.child(Button::new("preview-import").label("Preview").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.preview_import(window, cx);
                                }),
                            ))
                            .child(
                                Button::new("apply-import")
                                    .label("Import")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.apply_import(window, cx);
                                    })),
                            )
                        },
                    )
                    .when(
                        self.selected_section == WorkspaceToolSection::Codegen,
                        |row| {
                            row.child(
                                Button::new("generate-request-code")
                                    .label("Generate")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.generate_code(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("save-generated-request-code")
                                    .label("Save")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_generated_code(window, cx);
                                    })),
                            )
                        },
                    )
                    .when(
                        self.selected_section == WorkspaceToolSection::Settings,
                        |row| {
                            row.child(
                                Button::new("save-ui-settings")
                                    .label("Save settings")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_settings(window, cx);
                                    })),
                            )
                        },
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.section_content(cx)))
    }
}

const HISTORY_PANEL_LIMIT: usize = 200;
const DEFAULT_MAXIMUM_VISIBLE_REQUEST_TABS: usize = 6;

#[derive(Clone, Debug, Eq, PartialEq)]
struct UiPreferences {
    maximum_visible_tabs: usize,
    reduced_motion: bool,
    high_contrast: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            maximum_visible_tabs: DEFAULT_MAXIMUM_VISIBLE_REQUEST_TABS,
            reduced_motion: false,
            high_contrast: false,
        }
    }
}

impl UiPreferences {
    fn parse(value: &str) -> Result<Self, String> {
        let mut preferences = Self::default();
        for (index, line) in value.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("preference line {} must use key=value", index + 1))?;
            match key.trim() {
                "maximum_visible_tabs" => {
                    preferences.maximum_visible_tabs = value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| "maximum_visible_tabs must be an integer".to_owned())?
                        .clamp(1, 20);
                }
                "reduced_motion" => {
                    preferences.reduced_motion = value
                        .trim()
                        .parse::<bool>()
                        .map_err(|_| "reduced_motion must be true or false".to_owned())?;
                }
                "high_contrast" => {
                    preferences.high_contrast = value
                        .trim()
                        .parse::<bool>()
                        .map_err(|_| "high_contrast must be true or false".to_owned())?;
                }
                other => return Err(format!("unknown UI preference '{other}'")),
            }
        }
        Ok(preferences)
    }

    fn format(&self) -> String {
        format!(
            "maximum_visible_tabs={}
reduced_motion={}
high_contrast={}",
            self.maximum_visible_tabs, self.reduced_motion, self.high_contrast
        )
    }

    fn load() -> Self {
        tab_session_state_path()
            .ok()
            .map(|path| path.with_file_name("ui-preferences.txt"))
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|value| Self::parse(&value).ok())
            .unwrap_or_default()
    }
}

fn request_tab_window(
    tab_count: usize,
    active_index: Option<usize>,
    maximum_visible_tabs: usize,
) -> std::ops::Range<usize> {
    let visible_count = tab_count.min(maximum_visible_tabs.clamp(1, 20));
    let mut start = active_index
        .unwrap_or(0)
        .min(tab_count.saturating_sub(1))
        .saturating_sub(visible_count.saturating_sub(1) / 2);
    if start + visible_count > tab_count {
        start = tab_count.saturating_sub(visible_count);
    }
    start..start + visible_count
}

struct HistoryPanel {
    focus_handle: FocusHandle,
    tree_state: Entity<TreeState>,
    entries: Arc<RwLock<HashMap<String, HistoryEntry>>>,
    order: Vec<String>,
    database_path: Option<PathBuf>,
    request_panel: Entity<RequestPanel>,
    loading: bool,
    error: Option<String>,
    diff_summary: Option<String>,
}

impl HistoryPanel {
    fn new(
        request_panel: Entity<RequestPanel>,
        repository: Option<&WorkspaceRepository>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entries = Arc::new(RwLock::new(HashMap::new()));
        let tree_state = cx.new(|cx| {
            TreeState::new(cx).items(vec![TreeItem::new("__history-loading", "Loading history…")])
        });
        let mut panel = Self {
            focus_handle: cx.focus_handle(),
            tree_state,
            entries,
            order: Vec::new(),
            database_path: repository
                .map(|repository| repository.root().join(".apex").join("history.sqlite")),
            request_panel,
            loading: false,
            error: None,
            diff_summary: None,
        };
        panel.refresh(cx);
        panel
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.database_path.clone() else {
            self.loading = false;
            self.error = Some("Open a workspace to view request history.".to_owned());
            self.tree_state.update(cx, |state, cx| {
                state.set_items(
                    vec![TreeItem::new("__history-no-workspace", "No workspace")],
                    cx,
                );
            });
            cx.notify();
            return;
        };
        self.loading = true;
        self.error = None;
        self.diff_summary = None;
        self.tree_state.update(cx, |state, cx| {
            state.set_items(
                vec![TreeItem::new("__history-loading", "Loading history…")],
                cx,
            );
        });
        let receiver = match start_history_panel_load(path) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.loading = false;
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(this) = this.upgrade() else {
                return;
            };
            let _ = this.update(cx, |panel, cx| {
                panel.loading = false;
                match result {
                    Ok(entries) => panel.apply_entries(entries, cx),
                    Err(error) => {
                        panel.error = Some(error);
                        panel.tree_state.update(cx, |state, cx| {
                            state.set_items(
                                vec![TreeItem::new("__history-error", "History unavailable")],
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_entries(&mut self, entries: Vec<HistoryEntry>, cx: &mut Context<Self>) {
        self.order = entries
            .iter()
            .map(|entry| entry.record.execution_id.clone())
            .collect();
        if let Ok(mut stored) = self.entries.write() {
            *stored = entries
                .iter()
                .cloned()
                .map(|entry| (entry.record.execution_id.clone(), entry))
                .collect();
        }
        let items = if entries.is_empty() {
            vec![TreeItem::new("__history-empty", "No history entries")]
        } else {
            entries
                .into_iter()
                .map(|entry| {
                    let record = entry.record;
                    let status = record
                        .status
                        .map_or_else(|| "ERR".to_owned(), |value| value.to_string());
                    let snapshot = entry.snapshot.as_ref().is_some_and(|snapshot| {
                        snapshot.request_toml.is_some() || snapshot.response_body.is_some()
                    });
                    TreeItem::new(
                        record.execution_id,
                        format!(
                            "{} {} · {}ms{}",
                            status,
                            record.request_name,
                            record.duration_ms,
                            if snapshot { " · snapshot" } else { "" }
                        ),
                    )
                })
                .collect()
        };
        self.tree_state
            .update(cx, |state, cx| state.set_items(items, cx));
        self.error = None;
    }

    fn compare_latest(&mut self, cx: &mut Context<Self>) {
        let Some((left_id, right_id)) = self.order.get(1).zip(self.order.first()) else {
            self.diff_summary = Some("At least two history entries are required.".to_owned());
            cx.notify();
            return;
        };
        let entries = match self.entries.read() {
            Ok(entries) => entries,
            Err(_) => {
                self.diff_summary = Some("History state is temporarily unavailable.".to_owned());
                cx.notify();
                return;
            }
        };
        let (Some(left), Some(right)) = (entries.get(left_id), entries.get(right_id)) else {
            self.diff_summary = Some("History entries changed; refresh and try again.".to_owned());
            cx.notify();
            return;
        };
        let diff = semantic_response_diff(left, right, &SemanticDiffPolicy::default());
        self.diff_summary = Some(format!(
            "Latest vs previous: status {} · {} header change(s) · {} cookie change(s) · {}",
            if diff.status.changed {
                "changed"
            } else {
                "same"
            },
            diff.headers.len(),
            diff.cookies.len(),
            history_body_diff_label(&diff.body)
        ));
        cx.notify();
    }
}

fn start_history_panel_load(
    path: PathBuf,
) -> Result<async_channel::Receiver<Result<Vec<HistoryEntry>, String>>, String> {
    let (sender, receiver) = async_channel::bounded(1);
    thread::Builder::new()
        .name("apex-history-panel-load".to_owned())
        .spawn(move || {
            let result = HistoryDatabase::open(path)
                .and_then(|database| {
                    database.query(&HistoryQuery {
                        limit: HISTORY_PANEL_LIMIT,
                        ..HistoryQuery::default()
                    })
                })
                .map_err(|error| error.to_string());
            let _ = sender.send_blocking(result);
        })
        .map_err(|error| format!("failed to spawn history loader: {error}"))?;
    Ok(receiver)
}

fn history_body_diff_label(body: &BodyDifference) -> &'static str {
    match body {
        BodyDifference::Unavailable => "body unavailable",
        BodyDifference::Unchanged => "body unchanged",
        BodyDifference::Json(_) => "JSON changed",
        BodyDifference::Text(_) => "text changed",
        BodyDifference::Binary(_) => "binary changed",
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for HistoryPanel {}

impl Panel for HistoryPanel {
    fn panel_name(&self) -> &'static str {
        "HistoryPanel"
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "History"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.entries.clone();
        let request_panel = self.request_panel.clone();
        gpui_component::v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .child(
                        Button::new("history-refresh")
                            .label(if self.loading { "Loading…" } else { "Refresh" })
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        Button::new("history-compare-latest")
                            .label("Compare latest")
                            .disabled(self.order.len() < 2)
                            .on_click(cx.listener(|this, _, _, cx| this.compare_latest(cx))),
                    ),
            )
            .when_some(self.error.clone(), |panel, error| {
                panel.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when_some(self.diff_summary.clone(), |panel, summary| {
                panel.child(div().px_2().py_1().text_sm().child(summary))
            })
            .child(div().flex_1().min_h_0().child(tree(
                &self.tree_state,
                move |ix, entry, selected, _, _| {
                    let id = entry.item().id.to_string();
                    let mut item = ListItem::new(ix)
                        .selected(selected)
                        .child(entry.item().label.clone());
                    if !id.starts_with("__") {
                        let entries = entries.clone();
                        let request_panel = request_panel.clone();
                        item = item.on_click(move |_, window, cx| {
                            let entry = entries
                                .read()
                                .ok()
                                .and_then(|entries| entries.get(&id).cloned());
                            let Some(entry) = entry else {
                                window.push_notification(
                                    Notification::warning(
                                        "History entry changed; refresh and try again.",
                                    ),
                                    cx,
                                );
                                return;
                            };
                            let Some(request_toml) = entry
                                .snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.request_toml.as_deref())
                            else {
                                window.push_notification(
                                    Notification::warning(
                                        "This entry has no request snapshot. Enable request snapshots when sending.",
                                    ),
                                    cx,
                                );
                                return;
                            };
                            match apex_workspace::parse_request(request_toml) {
                                Ok(document) => request_panel.update(cx, |panel, cx| {
                                    match panel.open_history_document(document, window, cx) {
                                        Ok(()) => window.push_notification(
                                            Notification::success(
                                                "Restored history entry as an unsaved draft. Use Send to resend it.",
                                            ),
                                            cx,
                                        ),
                                        Err(error) => window.push_notification(
                                            Notification::error(format!(
                                                "History restore failed: {error}"
                                            )),
                                            cx,
                                        ),
                                    }
                                }),
                                Err(error) => window.push_notification(
                                    Notification::error(format!(
                                        "Stored request snapshot is invalid: {error}"
                                    )),
                                    cx,
                                ),
                            }
                        });
                    }
                    item
                },
            )))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestSection {
    Params,
    Authorization,
    Headers,
    Body,
    Scripts,
    Tests,
    Settings,
    Documentation,
}
impl RequestSection {
    const ALL: [Self; 8] = [
        Self::Params,
        Self::Authorization,
        Self::Headers,
        Self::Body,
        Self::Scripts,
        Self::Tests,
        Self::Settings,
        Self::Documentation,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Params => "Params",
            Self::Authorization => "Authorization",
            Self::Headers => "Headers",
            Self::Body => "Body",
            Self::Scripts => "Scripts",
            Self::Tests => "Tests",
            Self::Settings => "Settings",
            Self::Documentation => "Documentation",
        }
    }
}

#[derive(Clone, Debug)]
struct DocumentStore {
    repository: WorkspaceRepository,
    path: PathBuf,
}

impl DocumentStore {
    fn new(repository: WorkspaceRepository, path: PathBuf) -> Self {
        Self { repository, path }
    }

    fn open_draft() -> Result<(Self, Option<(HttpRequest, FileFingerprint)>), String> {
        let id = StableId::parse("gui-draft").expect("static identifier is valid");
        Self::open_draft_for(&id)
    }

    fn open_draft_for(
        id: &StableId,
    ) -> Result<(Self, Option<(HttpRequest, FileFingerprint)>), String> {
        let root = draft_state_root()?.join("apex-api").join("draft-workspace");
        let repository =
            WorkspaceRepository::new(root.clone()).map_err(|error| error.to_string())?;
        let path = root
            .join("collections")
            .join("local-drafts")
            .join(format!("{}.request.toml", id.as_str()));
        let loaded = if path.exists() {
            let loaded = repository
                .load_request(&path)
                .map_err(|error| error.to_string())?;
            Some((loaded.value.request, loaded.fingerprint))
        } else {
            None
        };
        Ok((Self { repository, path }, loaded))
    }

    fn load(&self) -> Result<(HttpRequest, FileFingerprint), String> {
        let loaded = self
            .repository
            .load_request(&self.path)
            .map_err(|error| error.to_string())?;
        Ok((loaded.value.request, loaded.fingerprint))
    }

    fn save(
        &self,
        request: &HttpRequest,
        expected: Option<FileFingerprint>,
    ) -> Result<FileFingerprint, String> {
        self.repository
            .save_request(
                &self.path,
                &RequestDocument::new(request.clone()),
                expected,
                &SecretLeakDetector::default(),
            )
            .map_err(|error| error.to_string())
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn resource_root(&self) -> PathBuf {
        self.repository.root().to_owned()
    }

    fn relative_path_in(&self, repository: &WorkspaceRepository) -> Option<PathBuf> {
        if self.repository.root() != repository.root() {
            return None;
        }
        self.path
            .strip_prefix(repository.root())
            .ok()
            .map(Path::to_owned)
    }

    fn from_relative(repository: WorkspaceRepository, relative_path: &Path) -> Self {
        let path = repository.root().join(relative_path);
        Self { repository, path }
    }
}

#[derive(Clone, Debug)]
struct PendingExternalDocument {
    store: DocumentStore,
    request: HttpRequest,
    fingerprint: FileFingerprint,
    reason: ExternalChangeReason,
}

#[derive(Clone, Debug)]
enum RequestExternalState {
    InSync,
    Checking { reason: ExternalChangeReason },
    ReloadAvailable(PendingExternalDocument),
    Conflict(PendingExternalDocument),
    Missing { message: String, conflict: bool },
    Failed { message: String, conflict: bool },
}

impl RequestExternalState {
    fn blocks_save(&self) -> Option<String> {
        match self {
            Self::InSync => None,
            Self::Checking { .. } => Some(
                "the request is being checked after an external workspace change".to_owned(),
            ),
            Self::ReloadAvailable(_) => Some(
                "a newer disk version is available; reload it before saving".to_owned(),
            ),
            Self::Conflict(_) => Some(
                "the request changed externally while local edits were present; reload the disk version or preserve the local content elsewhere before saving"
                    .to_owned(),
            ),
            Self::Missing { message, .. } | Self::Failed { message, .. } => Some(message.clone()),
        }
    }

    fn has_attention(&self) -> bool {
        !matches!(self, Self::InSync)
    }
}

type ExternalDocumentLoad = Result<(DocumentStore, HttpRequest, FileFingerprint), String>;

fn load_document_off_thread(
    store: DocumentStore,
) -> Result<async_channel::Receiver<ExternalDocumentLoad>, String> {
    let (sender, receiver) = async_channel::bounded(1);
    thread::Builder::new()
        .name("apex-workspace-document-load".to_owned())
        .spawn(move || {
            let result = store
                .load()
                .map(|(request, fingerprint)| (store, request, fingerprint));
            let _ = sender.send_blocking(result);
        })
        .map_err(|error| format!("failed to spawn workspace document loader: {error}"))?;
    Ok(receiver)
}

fn draft_state_root() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path));
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local").join("state"))
        .ok_or_else(|| "neither XDG_STATE_HOME nor HOME is set".to_owned())
}

fn tab_session_state_path() -> Result<PathBuf, String> {
    Ok(draft_state_root()?.join("apex-api").join("ui-session.json"))
}

fn load_tab_session() -> Result<WorkspaceSession, String> {
    let path = tab_session_state_path()?;
    let content = fs::read(&path).map_err(|error| error.to_string())?;
    let value = serde_json::from_slice(&content).map_err(|error| error.to_string())?;
    WorkspaceSession::from_json(&value, 20)
}

fn workspace_store_for_request(path: &Path) -> Result<DocumentStore, String> {
    let root = path
        .ancestors()
        .find(|ancestor| ancestor.join("apex.toml").is_file())
        .ok_or_else(|| format!("could not find apex.toml for {}", path.display()))?;
    let repository =
        WorkspaceRepository::new(root.to_owned()).map_err(|error| error.to_string())?;
    Ok(DocumentStore::new(repository, path.to_owned()))
}

fn default_request() -> HttpRequest {
    HttpRequest {
        id: StableId::parse("gui-draft").expect("static identifier is valid"),
        name: "GUI Draft".to_owned(),
        method: HttpMethod::Get,
        url: "https://httpbin.org/get".to_owned(),
        query: Vec::new(),
        headers: vec![
            HeaderEntry::new("Accept", "application/json").expect("static header is valid"),
        ],
        authentication: Authentication::None,
        body: RequestBody::Empty,
        settings: RequestSettings::default(),
        documentation: String::new(),
    }
}

fn editor_body(body: &RequestBody) -> String {
    match body {
        RequestBody::Empty => String::new(),
        RequestBody::Json(value) | RequestBody::Xml(value) => value.clone(),
        RequestBody::Text { text, .. } => text.clone(),
        RequestBody::GraphQl { query, .. } => query.clone(),
        RequestBody::FormUrlEncoded(fields) => format_form_fields(fields),
        RequestBody::Multipart(fields) => format_multipart_fields(fields),
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            relative_path.clone()
        }
    }
}

fn sensitivity_label(value: ValueSensitivity) -> &'static str {
    match value {
        ValueSensitivity::Public => "public",
        ValueSensitivity::Sensitive => "sensitive",
        ValueSensitivity::Secret => "secret",
    }
}

fn parse_sensitivity(value: &str, line: usize) -> Result<ValueSensitivity, String> {
    match value.trim() {
        "public" => Ok(ValueSensitivity::Public),
        "sensitive" => Ok(ValueSensitivity::Sensitive),
        "secret" => Ok(ValueSensitivity::Secret),
        other => Err(format!("line {line}: unknown sensitivity '{other}'")),
    }
}

fn parse_enabled(value: &str, line: usize) -> Result<bool, String> {
    match value.trim() {
        "enabled" | "on" | "true" | "1" => Ok(true),
        "disabled" | "off" | "false" | "0" => Ok(false),
        other => Err(format!(
            "line {line}: expected enabled or disabled, found '{other}'"
        )),
    }
}

fn format_form_fields(fields: &[FormField]) -> String {
    fields
        .iter()
        .map(|field| {
            format!(
                "{};{};{}={}",
                if field.enabled { "enabled" } else { "disabled" },
                sensitivity_label(field.sensitivity),
                field.name,
                field.value
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_form_fields(value: &str) -> Result<Vec<FormField>, String> {
    value
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let line_number = index + 1;
            let mut parts = line.splitn(3, ';');
            let enabled = parse_enabled(
                parts
                    .next()
                    .ok_or_else(|| format!("line {line_number}: missing enabled state"))?,
                line_number,
            )?;
            let sensitivity = parse_sensitivity(
                parts
                    .next()
                    .ok_or_else(|| format!("line {line_number}: missing sensitivity"))?,
                line_number,
            )?;
            let pair = parts
                .next()
                .ok_or_else(|| format!("line {line_number}: missing name=value pair"))?;
            let (name, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("line {line_number}: expected name=value"))?;
            if name.trim().is_empty() {
                return Err(format!("line {line_number}: name cannot be empty"));
            }
            Ok(FormField {
                name: name.trim().to_owned(),
                value: value.to_owned(),
                enabled,
                sensitivity,
            })
        })
        .collect()
}

fn format_headers(headers: &[HeaderEntry]) -> String {
    headers
        .iter()
        .map(|header| {
            format!(
                "{};{};{}={}",
                if header.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                sensitivity_label(header.sensitivity),
                header.name,
                header.value
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_headers(value: &str) -> Result<Vec<HeaderEntry>, String> {
    value
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let line_number = index + 1;
            let mut parts = line.splitn(3, ';');
            let enabled = parse_enabled(
                parts
                    .next()
                    .ok_or_else(|| format!("line {line_number}: missing enabled state"))?,
                line_number,
            )?;
            let sensitivity = parse_sensitivity(
                parts
                    .next()
                    .ok_or_else(|| format!("line {line_number}: missing sensitivity"))?,
                line_number,
            )?;
            let pair = parts
                .next()
                .ok_or_else(|| format!("line {line_number}: missing name=value pair"))?;
            let (name, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("line {line_number}: expected name=value"))?;
            let mut header = HeaderEntry::new(name.trim(), value)
                .map_err(|error| format!("line {line_number}: {error}"))?;
            header.enabled = enabled;
            header.sensitivity = sensitivity;
            Ok(header)
        })
        .collect()
}

fn format_authentication(authentication: &Authentication) -> String {
    match authentication {
        Authentication::None => "none".to_owned(),
        Authentication::Basic { username, password } => {
            format!("basic\nusername={username}\npassword={password}")
        }
        Authentication::Bearer { token } => format!("bearer\ntoken={token}"),
        Authentication::ApiKey {
            name,
            value,
            placement,
        } => format!(
            "api_key\nplacement={}\nname={name}\nvalue={value}",
            match placement {
                ApiKeyPlacement::Header => "header",
                ApiKeyPlacement::Query => "query",
            }
        ),
    }
}

fn parse_authentication(value: &str) -> Result<Authentication, String> {
    let mut lines = value.lines().filter(|line| !line.trim().is_empty());
    let kind = lines.next().unwrap_or("none").trim();
    let fields = lines
        .map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.trim().to_owned(), value.to_owned()))
                .ok_or_else(|| format!("authentication field '{line}' must use key=value"))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    let required = |key: &str| {
        fields
            .get(key)
            .cloned()
            .ok_or_else(|| format!("authentication is missing '{key}'"))
    };
    match kind {
        "none" => Ok(Authentication::None),
        "basic" => Ok(Authentication::Basic {
            username: required("username")?,
            password: required("password")?,
        }),
        "bearer" => Ok(Authentication::Bearer {
            token: required("token")?,
        }),
        "api_key" => Ok(Authentication::ApiKey {
            name: required("name")?,
            value: required("value")?,
            placement: match required("placement")?.as_str() {
                "header" => ApiKeyPlacement::Header,
                "query" => ApiKeyPlacement::Query,
                other => return Err(format!("unknown API-key placement '{other}'")),
            },
        }),
        other => Err(format!("unknown authentication kind '{other}'")),
    }
}

fn format_request_settings(settings: &RequestSettings) -> String {
    format!(
        "timeout_seconds={}\nconnection_timeout_seconds={}\nidle_timeout_seconds={}\nmaximum_response_bytes={}\nmaximum_wire_response_bytes={}\nredirect_limit={}\nfollow_redirects={}\nverify_certificates={}\ncookie_jar={}\ndecompress_response={}",
        settings.timeout.as_secs(),
        settings.connection_timeout.as_secs(),
        settings.idle_timeout.as_secs(),
        settings.maximum_response_bytes,
        settings.maximum_wire_response_bytes,
        settings.redirect_limit,
        settings.follow_redirects,
        settings.verify_certificates,
        settings.cookie_jar,
        settings.decompress_response,
    )
}

fn parse_bool_setting(key: &str, value: &str) -> Result<bool, String> {
    value
        .parse::<bool>()
        .map_err(|_| format!("setting '{key}' must be true or false"))
}

fn parse_request_settings(value: &str) -> Result<RequestSettings, String> {
    let fields = value
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
                .ok_or_else(|| format!("setting '{line}' must use key=value"))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    let required = |key: &str| {
        fields
            .get(key)
            .cloned()
            .ok_or_else(|| format!("request settings are missing '{key}'"))
    };
    Ok(RequestSettings {
        timeout: std::time::Duration::from_secs(
            required("timeout_seconds")?
                .parse()
                .map_err(|_| "timeout_seconds must be an integer".to_owned())?,
        ),
        connection_timeout: std::time::Duration::from_secs(
            required("connection_timeout_seconds")?
                .parse()
                .map_err(|_| "connection_timeout_seconds must be an integer".to_owned())?,
        ),
        idle_timeout: std::time::Duration::from_secs(
            required("idle_timeout_seconds")?
                .parse()
                .map_err(|_| "idle_timeout_seconds must be an integer".to_owned())?,
        ),
        maximum_response_bytes: required("maximum_response_bytes")?
            .parse()
            .map_err(|_| "maximum_response_bytes must be an integer".to_owned())?,
        maximum_wire_response_bytes: required("maximum_wire_response_bytes")?
            .parse()
            .map_err(|_| "maximum_wire_response_bytes must be an integer".to_owned())?,
        redirect_limit: required("redirect_limit")?
            .parse()
            .map_err(|_| "redirect_limit must be an integer".to_owned())?,
        follow_redirects: parse_bool_setting("follow_redirects", &required("follow_redirects")?)?,
        verify_certificates: parse_bool_setting(
            "verify_certificates",
            &required("verify_certificates")?,
        )?,
        cookie_jar: parse_bool_setting("cookie_jar", &required("cookie_jar")?)?,
        decompress_response: parse_bool_setting(
            "decompress_response",
            &required("decompress_response")?,
        )?,
    })
}

fn format_multipart_fields(fields: &[MultipartField]) -> String {
    fields
        .iter()
        .map(|field| {
            let (kind, value) = match &field.value {
                MultipartValue::Text(value) => ("text", value.as_str()),
                MultipartValue::File { relative_path } => ("file", relative_path.as_str()),
            };
            format!(
                "{};{};{};{};{}={}",
                if field.enabled { "enabled" } else { "disabled" },
                sensitivity_label(field.sensitivity),
                kind,
                field.content_type.as_deref().unwrap_or(""),
                field.name,
                value
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_multipart_fields(value: &str) -> Result<Vec<MultipartField>, String> {
    value
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let line_number = index + 1;
            let mut parts = line.splitn(5, ';');
            let enabled = parse_enabled(parts.next().unwrap_or(""), line_number)?;
            let sensitivity = parse_sensitivity(parts.next().unwrap_or(""), line_number)?;
            let kind = parts
                .next()
                .ok_or_else(|| format!("line {line_number}: missing multipart kind"))?;
            let content_type = parts
                .next()
                .ok_or_else(|| format!("line {line_number}: missing content type column"))?;
            let pair = parts
                .next()
                .ok_or_else(|| format!("line {line_number}: missing name=value pair"))?;
            let (name, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("line {line_number}: expected name=value"))?;
            let value = match kind {
                "text" => MultipartValue::Text(value.to_owned()),
                "file" => MultipartValue::File {
                    relative_path: value.to_owned(),
                },
                other => {
                    return Err(format!(
                        "line {line_number}: unknown multipart kind '{other}'"
                    ));
                }
            };
            Ok(MultipartField {
                name: name.trim().to_owned(),
                value,
                content_type: (!content_type.trim().is_empty())
                    .then(|| content_type.trim().to_owned()),
                enabled,
                sensitivity,
            })
        })
        .collect()
}

fn body_is_editable(body: &RequestBody) -> bool {
    matches!(
        body,
        RequestBody::Empty
            | RequestBody::Json(_)
            | RequestBody::Xml(_)
            | RequestBody::Text { .. }
            | RequestBody::GraphQl { .. }
            | RequestBody::FormUrlEncoded(_)
            | RequestBody::Multipart(_)
            | RequestBody::BinaryFile { .. }
            | RequestBody::StreamFile { .. }
    )
}

fn body_from_editor(original: &RequestBody, value: String) -> RequestBody {
    match original {
        RequestBody::Empty if value.is_empty() => RequestBody::Empty,
        RequestBody::Empty => RequestBody::Json(value),
        RequestBody::Json(_) => RequestBody::Json(value),
        RequestBody::Xml(_) => RequestBody::Xml(value),
        RequestBody::Text { content_type, .. } => RequestBody::Text {
            content_type: content_type.clone(),
            text: value,
        },
        RequestBody::GraphQl {
            variables_json,
            operation_name,
            ..
        } => RequestBody::GraphQl {
            query: value,
            variables_json: variables_json.clone(),
            operation_name: operation_name.clone(),
        },
        RequestBody::FormUrlEncoded(_) => parse_form_fields(&value)
            .map(RequestBody::FormUrlEncoded)
            .unwrap_or_else(|_| original.clone()),
        RequestBody::Multipart(_) => parse_multipart_fields(&value)
            .map(RequestBody::Multipart)
            .unwrap_or_else(|_| original.clone()),
        RequestBody::BinaryFile { .. } => RequestBody::BinaryFile {
            relative_path: value,
        },
        RequestBody::StreamFile { .. } => RequestBody::StreamFile {
            relative_path: value,
        },
    }
}

struct RequestPanelInit {
    response_panel: Entity<ResponsePanel>,
    network: Arc<NetworkEngine>,
    initial_document: Option<(DocumentStore, HttpRequest, FileFingerprint)>,
    variable_context: VariableContext,
    environment_label: String,
    variable_error: Option<String>,
}

#[derive(Clone, Debug)]
struct OpenRequestDocument {
    request: HttpRequest,
    store: Option<DocumentStore>,
    fingerprint: Option<FileFingerprint>,
    document_error: Option<String>,
    external_state: RequestExternalState,
    selected_section: RequestSection,
    dirty: bool,
}

struct RequestPanel {
    focus_handle: FocusHandle,
    url_state: Entity<InputState>,
    query_state: Entity<InputState>,
    headers_state: Entity<InputState>,
    authentication_state: Entity<InputState>,
    body_state: Entity<InputState>,
    settings_state: Entity<InputState>,
    documentation_state: Entity<InputState>,
    selected_section: RequestSection,
    method: HttpMethod,
    base_request: HttpRequest,
    response_panel: Entity<ResponsePanel>,
    network: Arc<NetworkEngine>,
    cancellation: Option<CancellationToken>,
    request_generation: u64,
    document_store: Option<DocumentStore>,
    document_fingerprint: Option<FileFingerprint>,
    document_error: Option<String>,
    external_state: RequestExternalState,
    external_generation: u64,
    variable_context: VariableContext,
    environment_label: String,
    variable_error: Option<String>,
    dirty: bool,
    maximum_visible_tabs: usize,
    session: WorkspaceSession,
    documents: HashMap<ResourceIdentity, OpenRequestDocument>,
    _subscriptions: Vec<Subscription>,
}

impl RequestPanel {
    fn new(init: RequestPanelInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let RequestPanelInit {
            response_panel,
            network,
            initial_document,
            variable_context,
            environment_label,
            variable_error,
        } = init;
        let is_workspace_document = initial_document.is_some();
        let (initial_store, initial_request, initial_fingerprint, initial_error) =
            if let Some((store, request, fingerprint)) = initial_document {
                (Some(store), request, Some(fingerprint), None)
            } else {
                match DocumentStore::open_draft() {
                    Ok((store, loaded)) => {
                        let (request, fingerprint) = loaded.map_or_else(
                            || (default_request(), None),
                            |(request, fingerprint)| (request, Some(fingerprint)),
                        );
                        (Some(store), request, fingerprint, None)
                    }
                    Err(error) => (None, default_request(), None, Some(error)),
                }
            };
        let initial_resource = if is_workspace_document {
            ResourceIdentity::WorkspaceRequest(
                initial_store
                    .as_ref()
                    .map(|store| store.path().to_owned())
                    .unwrap_or_default(),
            )
        } else {
            ResourceIdentity::Draft(initial_request.id.clone())
        };
        let initial_snapshot = OpenRequestDocument {
            request: initial_request.clone(),
            store: initial_store.clone(),
            fingerprint: initial_fingerprint,
            document_error: initial_error.clone(),
            external_state: RequestExternalState::InSync,
            selected_section: RequestSection::Params,
            dirty: false,
        };

        let mut session = WorkspaceSession::default();
        let mut documents = HashMap::new();
        let mut persisted_active = None;
        if let Ok(persisted) = load_tab_session() {
            persisted_active = persisted.active().map(|tab| tab.resource.clone());
            for tab in persisted.tabs().iter().cloned() {
                let loaded = if tab.resource == initial_resource {
                    Some(initial_snapshot.clone())
                } else {
                    match &tab.resource {
                        ResourceIdentity::Draft(id) => DocumentStore::open_draft_for(id)
                            .ok()
                            .and_then(|(store, loaded)| {
                                loaded.map(|(request, fingerprint)| OpenRequestDocument {
                                    request,
                                    store: Some(store),
                                    fingerprint: Some(fingerprint),
                                    document_error: None,
                                    external_state: RequestExternalState::InSync,
                                    selected_section: RequestSection::Params,
                                    dirty: tab.dirty,
                                })
                            }),
                        ResourceIdentity::WorkspaceRequest(path) => {
                            workspace_store_for_request(path).ok().and_then(|store| {
                                store.load().ok().map(|(request, fingerprint)| {
                                    OpenRequestDocument {
                                        request,
                                        store: Some(store),
                                        fingerprint: Some(fingerprint),
                                        document_error: None,
                                        external_state: RequestExternalState::InSync,
                                        selected_section: RequestSection::Params,
                                        dirty: tab.dirty,
                                    }
                                })
                            })
                        }
                    }
                };
                if let Some(document) = loaded {
                    documents.insert(tab.resource.clone(), document);
                    session.open(tab);
                }
            }
        }
        if !documents.contains_key(&initial_resource) {
            documents.insert(initial_resource.clone(), initial_snapshot);
            session.open(RequestTabState::saved(
                initial_resource.clone(),
                initial_request.name.clone(),
            ));
        }
        let active_resource = persisted_active
            .filter(|resource| documents.contains_key(resource))
            .unwrap_or_else(|| initial_resource.clone());
        if let Some(index) = session
            .tabs()
            .iter()
            .position(|tab| tab.resource == active_resource)
        {
            let _ = session.activate(index);
        }
        let active_document =
            documents
                .get(&active_resource)
                .cloned()
                .unwrap_or_else(|| OpenRequestDocument {
                    request: initial_request.clone(),
                    store: initial_store.clone(),
                    fingerprint: initial_fingerprint,
                    document_error: initial_error.clone(),
                    external_state: RequestExternalState::InSync,
                    selected_section: RequestSection::Params,
                    dirty: false,
                });
        let request = active_document.request.clone();
        let document_store = active_document.store.clone();
        let document_fingerprint = active_document.fingerprint;
        let document_error = active_document.document_error.clone();
        let dirty = active_document.dirty;
        let selected_section = active_document.selected_section;
        let external_state = active_document.external_state.clone();

        let url_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.example.com/v1/resource")
                .default_value(request.url.clone())
        });
        let query_state = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value(format_form_fields(&request.query))
        });
        let headers_state = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value(format_headers(&request.headers))
        });
        let authentication_state = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value(format_authentication(&request.authentication))
        });
        let body_state = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("json")
                .multi_line(true)
                .default_value(editor_body(&request.body))
        });
        let settings_state = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value(format_request_settings(&request.settings))
        });
        let documentation_state = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value(request.documentation.clone())
        });
        let url_subscription = cx.subscribe(&url_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.mark_dirty_from_editor(cx);
            }
        });
        let query_subscription = cx.subscribe(&query_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.mark_dirty_from_editor(cx);
            }
        });
        let headers_subscription = cx.subscribe(&headers_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.mark_dirty_from_editor(cx);
            }
        });
        let authentication_subscription =
            cx.subscribe(&authentication_state, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.mark_dirty_from_editor(cx);
                }
            });
        let body_subscription = cx.subscribe(&body_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.mark_dirty_from_editor(cx);
            }
        });
        let settings_subscription = cx.subscribe(&settings_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.mark_dirty_from_editor(cx);
            }
        });
        let documentation_subscription =
            cx.subscribe(&documentation_state, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.mark_dirty_from_editor(cx);
                }
            });
        Self {
            focus_handle: cx.focus_handle(),
            url_state,
            query_state,
            headers_state,
            authentication_state,
            body_state,
            settings_state,
            documentation_state,
            selected_section,
            method: request.method.clone(),
            base_request: request,
            response_panel,
            network,
            cancellation: None,
            request_generation: 0,
            document_store,
            document_fingerprint,
            document_error,
            external_state,
            external_generation: 0,
            variable_context,
            environment_label,
            variable_error,
            dirty,
            maximum_visible_tabs: UiPreferences::load().maximum_visible_tabs,
            session,
            documents,
            _subscriptions: vec![
                url_subscription,
                query_subscription,
                headers_subscription,
                authentication_subscription,
                body_subscription,
                settings_subscription,
                documentation_subscription,
            ],
        }
    }

    fn active_resource(&self) -> Option<ResourceIdentity> {
        self.session.active().map(|tab| tab.resource.clone())
    }

    fn snapshot_active(&mut self, cx: &App) {
        let Some(resource) = self.active_resource() else {
            return;
        };
        self.documents.insert(
            resource,
            OpenRequestDocument {
                request: self.current_request(cx),
                store: self.document_store.clone(),
                fingerprint: self.document_fingerprint,
                document_error: self.document_error.clone(),
                external_state: self.external_state.clone(),
                selected_section: self.selected_section,
                dirty: self.dirty,
            },
        );
    }

    fn restore_document(
        &mut self,
        document: OpenRequestDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel(cx);
        self.method = document.request.method.clone();
        self.url_state.update(cx, |state, cx| {
            state.set_value(&document.request.url, window, cx);
        });
        self.query_state.update(cx, |state, cx| {
            state.set_value(format_form_fields(&document.request.query), window, cx);
        });
        self.headers_state.update(cx, |state, cx| {
            state.set_value(format_headers(&document.request.headers), window, cx);
        });
        self.authentication_state.update(cx, |state, cx| {
            state.set_value(
                format_authentication(&document.request.authentication),
                window,
                cx,
            );
        });
        self.body_state.update(cx, |state, cx| {
            state.set_value(editor_body(&document.request.body), window, cx);
        });
        self.settings_state.update(cx, |state, cx| {
            state.set_value(
                format_request_settings(&document.request.settings),
                window,
                cx,
            );
        });
        self.documentation_state.update(cx, |state, cx| {
            state.set_value(&document.request.documentation, window, cx);
        });
        self.base_request = document.request;
        self.document_store = document.store;
        self.document_fingerprint = document.fingerprint;
        self.document_error = document.document_error;
        self.external_state = document.external_state;
        self.selected_section = document.selected_section;
        self.dirty = document.dirty;
        if let Some(index) = self.session.active_index() {
            let _ = self.session.mark_dirty(index, self.dirty);
        }
        self.response_panel.update(cx, |panel, cx| {
            panel.reset();
            cx.notify();
        });
        cx.notify();
    }

    fn activate_relative_tab(
        &mut self,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_count = self.session.tabs().len();
        if tab_count < 2 {
            return;
        }
        let current = self.session.active_index().unwrap_or(0);
        let next = if direction >= 0 {
            (current + 1) % tab_count
        } else {
            current.checked_sub(1).unwrap_or(tab_count - 1)
        };
        if let Err(error) = self.activate_tab(next, window, cx) {
            window.push_notification(Notification::error(error), cx);
        }
    }

    fn activate_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.session.active_index() == Some(index) {
            return Ok(());
        }
        self.snapshot_active(cx);
        self.session
            .activate(index)
            .map_err(|error| error.to_string())?;
        let resource = self
            .active_resource()
            .ok_or_else(|| "the selected request tab is unavailable".to_owned())?;
        let document = self
            .documents
            .get(&resource)
            .cloned()
            .ok_or_else(|| "the selected request document is unavailable".to_owned())?;
        self.restore_document(document, window, cx);
        self.persist_session()?;
        Ok(())
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active(cx);
        let previous_active = self.active_resource();
        match self.session.close(index) {
            Ok(closed) => {
                self.documents.remove(&closed.resource);
                let next_active = self.active_resource();
                if next_active != previous_active
                    && let Some(resource) = next_active
                    && let Some(document) = self.documents.get(&resource).cloned()
                {
                    self.restore_document(document, window, cx);
                }
                if let Err(error) = self.persist_session() {
                    window.push_notification(Notification::error(error), cx);
                }
            }
            Err(CloseTabError::UnsavedChanges { title, .. }) => {
                window.push_notification(
                    Notification::error(format!(
                        "Save or discard changes in '{title}' before closing the tab"
                    )),
                    cx,
                );
            }
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()), cx);
            }
        }
    }

    fn persist_session(&self) -> Result<(), String> {
        let path = tab_session_state_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let temporary = path.with_extension("json.tmp");
        let content = serde_json::to_vec_pretty(&self.session.to_json())
            .map_err(|error| error.to_string())?;
        fs::write(&temporary, content).map_err(|error| error.to_string())?;
        fs::rename(&temporary, &path).map_err(|error| error.to_string())?;
        Ok(())
    }

    fn toggle_pin(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.session.tabs().get(index) else {
            return;
        };
        let pinned = !tab.pinned;
        if let Err(error) = self.session.set_pinned(index, pinned) {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        if let Err(error) = self.persist_session() {
            window.push_notification(Notification::error(error), cx);
        }
        cx.notify();
    }

    fn move_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.session.reorder(from, to) {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        if let Err(error) = self.persist_session() {
            window.push_notification(Notification::error(error), cx);
        }
        cx.notify();
    }

    fn promote_preview(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.session.set_preview(index, false) {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        if let Err(error) = self.persist_session() {
            window.push_notification(Notification::error(error), cx);
        }
        cx.notify();
    }

    fn reload_document_for_resource(
        &self,
        resource: &ResourceIdentity,
        dirty: bool,
    ) -> Result<OpenRequestDocument, String> {
        match resource {
            ResourceIdentity::Draft(id) => {
                let (store, loaded) = DocumentStore::open_draft_for(id)?;
                let (request, fingerprint) =
                    loaded.ok_or_else(|| format!("draft '{}' no longer exists", id.as_str()))?;
                Ok(OpenRequestDocument {
                    request,
                    store: Some(store),
                    fingerprint: Some(fingerprint),
                    document_error: None,
                    external_state: RequestExternalState::InSync,
                    selected_section: RequestSection::Params,
                    dirty,
                })
            }
            ResourceIdentity::WorkspaceRequest(path) => {
                let store = workspace_store_for_request(path)?;
                let (request, fingerprint) = store.load()?;
                Ok(OpenRequestDocument {
                    request,
                    store: Some(store),
                    fingerprint: Some(fingerprint),
                    document_error: None,
                    external_state: RequestExternalState::InSync,
                    selected_section: RequestSection::Params,
                    dirty,
                })
            }
        }
    }

    fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active(cx);
        let Some(index) = self.session.reopen_closed() else {
            window.push_notification(Notification::info("No recently closed request tabs"), cx);
            return;
        };
        let Some(tab) = self.session.tabs().get(index).cloned() else {
            return;
        };
        match self.reload_document_for_resource(&tab.resource, tab.dirty) {
            Ok(document) => {
                self.documents
                    .insert(tab.resource.clone(), document.clone());
                self.restore_document(document, window, cx);
                if let Err(error) = self.persist_session() {
                    window.push_notification(Notification::error(error), cx);
                }
            }
            Err(error) => {
                let _ = self.session.force_close(index);
                window.push_notification(
                    Notification::error(format!("Could not reopen '{}': {error}", tab.title)),
                    cx,
                );
            }
        }
    }

    fn retain_open_documents(&mut self) {
        self.documents.retain(|resource, _| {
            self.session
                .tabs()
                .iter()
                .any(|tab| &tab.resource == resource)
        });
    }

    fn close_other_tabs(&mut self, keep: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active(cx);
        match self.session.close_others(keep) {
            Ok(()) => {
                self.retain_open_documents();
                if let Some(resource) = self.active_resource()
                    && let Some(document) = self.documents.get(&resource).cloned()
                {
                    self.restore_document(document, window, cx);
                }
                if let Err(error) = self.persist_session() {
                    window.push_notification(Notification::error(error), cx);
                }
            }
            Err(error) => window.push_notification(Notification::error(error.to_string()), cx),
        }
    }

    fn close_tabs_to_right(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active(cx);
        match self.session.close_to_right(index) {
            Ok(()) => {
                self.retain_open_documents();
                if let Some(resource) = self.active_resource()
                    && let Some(document) = self.documents.get(&resource).cloned()
                {
                    self.restore_document(document, window, cx);
                }
                if let Err(error) = self.persist_session() {
                    window.push_notification(Notification::error(error), cx);
                }
            }
            Err(error) => window.push_notification(Notification::error(error.to_string()), cx),
        }
    }

    fn mark_dirty_from_editor(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        if let Some(index) = self.session.active_index() {
            let _ = self.session.mark_dirty(index, true);
        }
        if let RequestExternalState::ReloadAvailable(pending) = self.external_state.clone() {
            self.external_state = RequestExternalState::Conflict(pending);
        }
        cx.notify();
    }

    fn observe_workspace_change(
        &mut self,
        repository: WorkspaceRepository,
        change: &WorkspaceChange,
        cx: &mut Context<Self>,
    ) {
        let current_path = self
            .document_store
            .as_ref()
            .and_then(|store| store.relative_path_in(&repository));
        let reconciliation =
            reconcile_workspace_change(current_path.as_deref(), self.dirty, change);
        match reconciliation.document {
            DocumentReconcileAction::None => {}
            DocumentReconcileAction::Verify {
                path,
                reason,
                conflict_if_changed,
            } => {
                let store = DocumentStore::from_relative(repository, &path);
                self.start_external_verification(store, reason, conflict_if_changed, cx);
            }
            DocumentReconcileAction::Missing {
                path,
                reason,
                had_unsaved_changes,
            } => {
                self.external_generation = self.external_generation.wrapping_add(1);
                let local_detail = if had_unsaved_changes {
                    " Local edits remain in memory and were not overwritten."
                } else {
                    ""
                };
                self.external_state = RequestExternalState::Missing {
                    message: format!("{}: {}.{}", reason.summary(), path.display(), local_detail),
                    conflict: had_unsaved_changes,
                };
                cx.notify();
            }
        }
    }

    fn start_external_verification(
        &mut self,
        store: DocumentStore,
        reason: ExternalChangeReason,
        conflict_if_changed: bool,
        cx: &mut Context<Self>,
    ) {
        self.external_generation = self.external_generation.wrapping_add(1);
        let generation = self.external_generation;
        self.external_state = RequestExternalState::Checking {
            reason: reason.clone(),
        };
        let receiver = match load_document_off_thread(store) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.external_state = RequestExternalState::Failed {
                    message: error,
                    conflict: conflict_if_changed || self.dirty,
                };
                cx.notify();
                return;
            }
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(this) = this.upgrade() else {
                return;
            };
            let _ = this.update(cx, |panel, cx| {
                if panel.external_generation != generation {
                    return;
                }
                match result {
                    Ok((store, request, fingerprint)) => {
                        let unchanged = panel.document_fingerprint == Some(fingerprint)
                            && panel
                                .document_store
                                .as_ref()
                                .is_some_and(|current| current.path() == store.path());
                        if unchanged {
                            panel.external_state = RequestExternalState::InSync;
                        } else {
                            let pending = PendingExternalDocument {
                                store,
                                request,
                                fingerprint,
                                reason,
                            };
                            if conflict_if_changed || panel.dirty {
                                panel.external_state = RequestExternalState::Conflict(pending);
                            } else {
                                panel.external_state =
                                    RequestExternalState::ReloadAvailable(pending);
                            }
                        }
                    }
                    Err(error) => {
                        let conflict = conflict_if_changed || panel.dirty;
                        panel.external_state = RequestExternalState::Failed {
                            message: format!(
                                "{}; disk verification failed: {error}",
                                reason.summary()
                            ),
                            conflict,
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reload_external(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = match &self.external_state {
            RequestExternalState::ReloadAvailable(pending)
            | RequestExternalState::Conflict(pending) => pending.clone(),
            _ => return,
        };
        self.apply_request(
            pending.request,
            pending.store,
            pending.fingerprint,
            window,
            cx,
        );
    }

    fn has_external_attention(&self) -> bool {
        self.external_state.has_attention()
    }

    fn is_running(&self) -> bool {
        self.cancellation.is_some()
    }

    fn set_environment(
        &mut self,
        context: VariableContext,
        label: String,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.variable_context = context;
        self.environment_label = label;
        self.variable_error = error;
        cx.notify();
    }

    fn try_current_request(&self, cx: &App) -> Result<HttpRequest, String> {
        let mut request = self.base_request.clone();
        request.method = self.method.clone();
        request.url = self.url_state.read(cx).value().to_string();
        request.query = parse_form_fields(&self.query_state.read(cx).value())?;
        request.headers = parse_headers(&self.headers_state.read(cx).value())?;
        request.authentication = parse_authentication(&self.authentication_state.read(cx).value())?;
        request.settings = parse_request_settings(&self.settings_state.read(cx).value())?;
        request.documentation = self.documentation_state.read(cx).value().to_string();
        if body_is_editable(&request.body) {
            let value = self.body_state.read(cx).value().to_string();
            request.body = match &request.body {
                RequestBody::FormUrlEncoded(_) => {
                    RequestBody::FormUrlEncoded(parse_form_fields(&value)?)
                }
                RequestBody::Multipart(_) => {
                    RequestBody::Multipart(parse_multipart_fields(&value)?)
                }
                _ => body_from_editor(&request.body, value),
            };
        }
        Ok(request)
    }

    fn current_request(&self, cx: &App) -> HttpRequest {
        self.try_current_request(cx)
            .unwrap_or_else(|_| self.base_request.clone())
    }

    fn apply_request(
        &mut self,
        request: HttpRequest,
        store: DocumentStore,
        fingerprint: FileFingerprint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel(cx);
        self.method = request.method.clone();
        self.url_state
            .update(cx, |state, cx| state.set_value(&request.url, window, cx));
        self.query_state.update(cx, |state, cx| {
            state.set_value(format_form_fields(&request.query), window, cx);
        });
        self.headers_state.update(cx, |state, cx| {
            state.set_value(format_headers(&request.headers), window, cx);
        });
        self.authentication_state.update(cx, |state, cx| {
            state.set_value(format_authentication(&request.authentication), window, cx);
        });
        self.body_state.update(cx, |state, cx| {
            state.set_value(editor_body(&request.body), window, cx);
        });
        self.settings_state.update(cx, |state, cx| {
            state.set_value(format_request_settings(&request.settings), window, cx);
        });
        self.documentation_state.update(cx, |state, cx| {
            state.set_value(&request.documentation, window, cx);
        });
        self.base_request = request;
        self.document_store = Some(store);
        self.document_fingerprint = Some(fingerprint);
        self.document_error = None;
        self.external_generation = self.external_generation.wrapping_add(1);
        self.external_state = RequestExternalState::InSync;
        self.selected_section = RequestSection::Params;
        self.dirty = false;
        if let Some(index) = self.session.active_index() {
            let _ = self.session.mark_dirty(index, false);
        }
        self.snapshot_active(cx);
        self.response_panel.update(cx, |panel, cx| {
            panel.reset();
            cx.notify();
        });
        cx.notify();
    }

    fn open_document(
        &mut self,
        store: DocumentStore,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.snapshot_active(cx);
        let resource = ResourceIdentity::WorkspaceRequest(store.path().to_owned());
        if let Some(index) = self
            .session
            .tabs()
            .iter()
            .position(|tab| tab.resource == resource)
        {
            return self.activate_tab(index, window, cx);
        }
        let (request, fingerprint) = store.load()?;
        let title = request.name.clone();
        let document = OpenRequestDocument {
            request: request.clone(),
            store: Some(store),
            fingerprint: Some(fingerprint),
            document_error: None,
            external_state: RequestExternalState::InSync,
            selected_section: RequestSection::Params,
            dirty: false,
        };
        self.documents.insert(resource.clone(), document.clone());
        let mut tab = RequestTabState::saved(resource, title);
        tab.preview = true;
        self.session.open(tab);
        self.restore_document(document, window, cx);
        self.persist_session()?;
        Ok(())
    }

    fn cycle_method(&mut self, cx: &mut Context<Self>) {
        self.method = match self.method {
            HttpMethod::Get => HttpMethod::Post,
            HttpMethod::Post => HttpMethod::Put,
            HttpMethod::Put => HttpMethod::Patch,
            HttpMethod::Patch => HttpMethod::Delete,
            _ => HttpMethod::Get,
        };
        self.mark_dirty_from_editor(cx);
    }

    fn open_history_document(
        &mut self,
        document: RequestDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.snapshot_active(cx);
        let request = document.request;
        let resource = ResourceIdentity::Draft(request.id.clone());
        let (store, loaded) = DocumentStore::open_draft_for(&request.id)?;
        if let Some(index) = self
            .session
            .tabs()
            .iter()
            .position(|tab| tab.resource == resource)
            && self.session.tabs()[index].dirty
        {
            return self.activate_tab(index, window, cx);
        }
        let restored = OpenRequestDocument {
            request: request.clone(),
            store: Some(store),
            fingerprint: loaded.map(|(_, fingerprint)| fingerprint),
            document_error: None,
            external_state: RequestExternalState::InSync,
            selected_section: RequestSection::Params,
            dirty: true,
        };
        self.documents.insert(resource.clone(), restored.clone());
        if let Some(index) = self
            .session
            .tabs()
            .iter()
            .position(|tab| tab.resource == resource)
        {
            self.session
                .activate(index)
                .map_err(|error| error.to_string())?;
            self.session
                .mark_dirty(index, true)
                .map_err(|error| error.to_string())?;
        } else {
            self.session.open(RequestTabState::draft(
                request.id.clone(),
                format!("{} (history)", request.name),
            ));
        }
        self.restore_document(restored, window, cx);
        Ok(())
    }

    fn new_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.snapshot_active(cx);
        let request = default_request();
        let resource = ResourceIdentity::Draft(request.id.clone());
        if let Some(index) = self
            .session
            .tabs()
            .iter()
            .position(|tab| tab.resource == resource)
        {
            let _ = self.activate_tab(index, window, cx);
            return;
        }
        let (store, fingerprint, store_error) = match DocumentStore::open_draft() {
            Ok((store, loaded)) => (
                Some(store),
                loaded.map(|(_, fingerprint)| fingerprint),
                None,
            ),
            Err(error) => (None, None, Some(error)),
        };
        let document = OpenRequestDocument {
            request: request.clone(),
            store,
            fingerprint,
            document_error: store_error,
            external_state: RequestExternalState::InSync,
            selected_section: RequestSection::Params,
            dirty: true,
        };
        self.documents.insert(resource.clone(), document.clone());
        self.session.open(RequestTabState::draft(
            request.id.clone(),
            request.name.clone(),
        ));
        self.restore_document(document, window, cx);
    }

    fn save_draft(&mut self, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        if let Some(error) = self.external_state.blocks_save() {
            return Err(error);
        }
        if let Some(error) = &self.document_error {
            return Err(error.clone());
        }
        let request = self.try_current_request(cx)?;
        let store = self
            .document_store
            .as_ref()
            .ok_or_else(|| "document store is unavailable".to_owned())?;
        let fingerprint = store.save(&request, self.document_fingerprint)?;
        let path = store.path().to_owned();
        self.document_fingerprint = Some(fingerprint);
        self.base_request = request;
        self.dirty = false;
        if let Some(index) = self.session.active_index() {
            let _ = self.session.mark_dirty(index, false);
        }
        self.snapshot_active(cx);
        cx.notify();
        Ok(path)
    }

    fn save_with_notification(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.save_draft(cx) {
            Ok(path) => window.push_notification(
                Notification::success(format!("Saved request to {}", path.display())),
                cx,
            ),
            Err(error) => window.push_notification(
                Notification::error(format!("Request save failed: {error}")),
                cx,
            ),
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(token) = self.cancellation.take() {
            token.cancel();
            self.response_panel.update(cx, |panel, cx| {
                panel.state = ResponseState::Cancelled;
                panel.console.push("Cancellation requested".to_owned());
                cx.notify();
            });
            cx.notify();
        }
    }

    fn start_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let unresolved_request = match self.try_current_request(cx) {
            Ok(request) => request,
            Err(error) => {
                window.push_notification(
                    Notification::error(format!("Request validation failed: {error}")),
                    cx,
                );
                return;
            }
        };
        if let Some(error) = &self.variable_error {
            let error = ExecutionError::UnresolvedVariable(error.clone());
            self.response_panel.update(cx, |panel, cx| {
                panel.state = ResponseState::Failed(error.clone());
                panel.console.push(error.to_string());
                cx.notify();
            });
            window.push_notification(
                Notification::error(format!("Variable loading failed: {error}")),
                cx,
            );
            return;
        }
        let request = match resolve_http_request(
            &unresolved_request,
            &self.variable_context,
            &SystemDynamicVariables,
            ResolverOptions::default(),
        ) {
            Ok(resolved) => resolved.request,
            Err(error) => {
                let execution_error = ExecutionError::UnresolvedVariable(error.to_string());
                self.response_panel.update(cx, |panel, cx| {
                    panel.state = ResponseState::Failed(execution_error.clone());
                    panel.console.push(execution_error.to_string());
                    cx.notify();
                });
                window.push_notification(
                    Notification::error(format!("Request was not sent: {error}")),
                    cx,
                );
                return;
            }
        };
        let mut context = ExecutionContext::new(
            request.settings.timeout,
            request.settings.maximum_response_bytes,
        );
        context.resource_root = self
            .document_store
            .as_ref()
            .map(DocumentStore::resource_root);
        self.cancellation = Some(context.cancellation.clone());
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        let receiver = self.network.execute(request, context);
        self.response_panel.update(cx, |panel, cx| {
            panel.begin();
            cx.notify();
        });
        window.push_notification(Notification::info("Request started"), cx);

        let response_panel = self.response_panel.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(message) = receiver.recv().await {
                let Some(this) = this.upgrade() else {
                    break;
                };
                let completed = matches!(message, NetworkMessage::Finished(_));
                let _ = this.update(cx, |panel, cx| {
                    if panel.request_generation != generation {
                        return;
                    }
                    if completed {
                        panel.cancellation = None;
                    }
                    response_panel.update(cx, |response, cx| {
                        response.apply_message(message);
                        cx.notify();
                    });
                    cx.notify();
                });
                if completed {
                    break;
                }
            }
        })
        .detach();
    }

    fn external_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (message, conflict, reload_label) = match &self.external_state {
            RequestExternalState::InSync => return None,
            RequestExternalState::Checking { reason } => (
                format!("Checking disk state because {}.", reason.summary()),
                false,
                None,
            ),
            RequestExternalState::ReloadAvailable(pending) => (
                format!(
                    "{}; a newer version is available at {}.",
                    pending.reason.summary(),
                    pending.store.path().display()
                ),
                false,
                Some("Reload from disk"),
            ),
            RequestExternalState::Conflict(pending) => (
                format!(
                    "{}; local edits were preserved and the disk version at {} was not applied.",
                    pending.reason.summary(),
                    pending.store.path().display()
                ),
                true,
                Some("Discard local edits and reload"),
            ),
            RequestExternalState::Missing { message, conflict } => (
                if *conflict {
                    format!("{message} Resolve the conflict before saving.")
                } else {
                    message.clone()
                },
                *conflict,
                None,
            ),
            RequestExternalState::Failed { message, conflict } => (
                if *conflict {
                    format!("{message}. Local edits remain in memory.")
                } else {
                    message.clone()
                },
                *conflict,
                None,
            ),
        };
        let border_color = if conflict {
            cx.theme().danger
        } else {
            cx.theme().accent
        };
        let mut row = h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(border_color)
            .child(div().flex_1().text_sm().child(message));
        if let Some(label) = reload_label {
            row = row.child(
                Button::new("reload-external-request")
                    .label(label)
                    .when(conflict, |button| button.danger())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reload_external(window, cx);
                    })),
            );
        }
        Some(row.into_any_element())
    }

    fn section_content(&self, cx: &App) -> gpui::AnyElement {
        match self.selected_section {
            RequestSection::Body if body_is_editable(&self.base_request.body) => {
                Input::new(&self.body_state).h_full().into_any_element()
            }
            RequestSection::Body => info_state(
                "Body viewer",
                format!(
                    "Body kind '{}' is preserved exactly. A structured editor for this body kind is not active yet.",
                    self.base_request.body.kind()
                ),
                cx,
            ),
            RequestSection::Params => structured_editor(
                "Query parameters",
                "Format: enabled|disabled; public|sensitive|secret; name=value. Duplicate names and order are preserved.",
                &self.query_state,
                parse_form_fields(&self.query_state.read(cx).value()).err(),
                cx,
            ),
            RequestSection::Authorization => structured_editor(
                "Authentication",
                "First line: none, basic, bearer, or api_key. Following lines use key=value. Store credentials as variable or secret references.",
                &self.authentication_state,
                parse_authentication(&self.authentication_state.read(cx).value()).err(),
                cx,
            ),
            RequestSection::Headers => structured_editor(
                "Headers",
                "Format: enabled|disabled; public|sensitive|secret; name=value. Duplicate headers and order are preserved.",
                &self.headers_state,
                parse_headers(&self.headers_state.read(cx).value()).err(),
                cx,
            ),
            RequestSection::Scripts => info_state(
                "Scripts unavailable",
                "Scripts remain disabled until the sandbox and workspace-trust phase is implemented.",
                cx,
            ),
            RequestSection::Tests => info_state(
                "Assertions unavailable",
                "No assertion result is fabricated; this panel activates with the automation phase.",
                cx,
            ),
            RequestSection::Settings => structured_editor(
                "Request settings",
                format!(
                    "Environment: {}. Edit numeric limits and true/false transport controls using key=value.",
                    self.environment_label
                ),
                &self.settings_state,
                parse_request_settings(&self.settings_state.read(cx).value()).err(),
                cx,
            ),
            RequestSection::Documentation => structured_editor(
                "Documentation",
                "Markdown-compatible request documentation persisted in the Git-friendly request file.",
                &self.documentation_state,
                None,
                cx,
            ),
        }
    }
}

impl Focusable for RequestPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for RequestPanel {}
impl Panel for RequestPanel {
    fn panel_name(&self) -> &'static str {
        "ApexRequestPanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        format!(
            "{} {}{}{}",
            self.method.as_str(),
            self.base_request.name,
            if self.dirty { " •" } else { "" },
            if self.has_external_attention() {
                " ⚠"
            } else {
                ""
            }
        )
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
}
impl Render for RequestPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.is_running();
        let external_banner = self.external_banner(cx);
        let active_tab = self.session.active_index();
        let tab_count = self.session.tabs().len();
        let visible_range = request_tab_window(tab_count, active_tab, self.maximum_visible_tabs);
        let visible_start = visible_range.start;
        let visible_end = visible_range.end;
        let hidden_left = visible_start;
        let hidden_right = tab_count.saturating_sub(visible_end);
        let tabs = self.session.tabs()[visible_start..visible_end]
            .iter()
            .cloned()
            .enumerate()
            .map(|(offset, tab)| (visible_start + offset, tab))
            .collect::<Vec<_>>();
        let recently_closed = self.session.recently_closed_count();
        let active_for_actions = active_tab.unwrap_or(0);
        let tab_strip = h_flex()
            .w_full()
            .gap_1()
            .when(hidden_left > 0, |row| {
                row.child(
                    Button::new("show-previous-request-tabs")
                        .label(format!("◀ {hidden_left}"))
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Err(error) = this.activate_tab(visible_start - 1, window, cx) {
                                window.push_notification(Notification::error(error), cx);
                            }
                        })),
                )
            })
            .children(tabs.into_iter().map(|(index, tab)| {
                let label = format!(
                    "{}{}{}{}",
                    if tab.pinned { "📌 " } else { "" },
                    tab.title,
                    if tab.preview { " ◇" } else { "" },
                    if tab.dirty { " •" } else { "" }
                );
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(("request-document-tab", index))
                            .label(label)
                            .when(active_tab == Some(index), |button| button.primary())
                            .when(active_tab != Some(index), |button| button.ghost())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Err(error) = this.activate_tab(index, window, cx) {
                                    window.push_notification(Notification::error(error), cx);
                                }
                            })),
                    )
                    .when(tab.preview, |row| {
                        row.child(
                            Button::new(("keep-request-document-tab", index))
                                .label("Keep")
                                .ghost()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.promote_preview(index, window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new(("pin-request-document-tab", index))
                            .label(if tab.pinned { "Unpin" } else { "Pin" })
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_pin(index, window, cx);
                            })),
                    )
                    .when(index > 0, |row| {
                        row.child(
                            Button::new(("move-request-document-tab-left", index))
                                .label("←")
                                .ghost()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.move_tab(index, index - 1, window, cx);
                                })),
                        )
                    })
                    .when(index + 1 < tab_count, |row| {
                        row.child(
                            Button::new(("move-request-document-tab-right", index))
                                .label("→")
                                .ghost()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.move_tab(index, index + 1, window, cx);
                                })),
                        )
                    })
                    .when(tab_count > 1 && !tab.pinned, |row| {
                        row.child(
                            Button::new(("close-request-document-tab", index))
                                .label("×")
                                .ghost()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.close_tab(index, window, cx);
                                })),
                        )
                    })
            }))
            .when(hidden_right > 0, |row| {
                row.child(
                    Button::new("show-next-request-tabs")
                        .label(format!("{hidden_right} ▶"))
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Err(error) = this.activate_tab(visible_end, window, cx) {
                                window.push_notification(Notification::error(error), cx);
                            }
                        })),
                )
            })
            .child(
                Button::new("reopen-closed-request-tab")
                    .label(format!("Reopen ({recently_closed})"))
                    .ghost()
                    .disabled(recently_closed == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reopen_closed_tab(window, cx);
                    })),
            )
            .when(tab_count > 1, |row| {
                row.child(
                    Button::new("close-other-request-tabs")
                        .label("Close others")
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_other_tabs(active_for_actions, window, cx);
                        })),
                )
                .when(active_for_actions + 1 < tab_count, |row| {
                    row.child(
                        Button::new("close-request-tabs-to-right")
                            .label("Close right")
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_tabs_to_right(active_for_actions, window, cx);
                            })),
                    )
                })
            });
        gpui_component::v_flex()
            .size_full()
            .gap_2()
            .child(tab_strip)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("method")
                            .label(self.method.as_str().to_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.cycle_method(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.url_state).cleanable(true)),
                    )
                    .child(
                        Button::new("save")
                            .icon(IconName::Check)
                            .label(if self.dirty { "Save" } else { "Saved" })
                            .disabled(!self.dirty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_with_notification(window, cx);
                            })),
                    )
                    .child(
                        Button::new("send")
                            .label(if running { "Running" } else { "Send" })
                            .primary()
                            .loading(running)
                            .disabled(running)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_send(window, cx);
                            })),
                    )
                    .when(running, |row| {
                        row.child(
                            Button::new("cancel")
                                .label("Cancel")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                    }),
            )
            .when_some(external_banner, |panel, banner| panel.child(banner))
            .child(
                TabBar::new("request-sections")
                    .underline()
                    .selected_index(
                        RequestSection::ALL
                            .iter()
                            .position(|section| *section == self.selected_section)
                            .unwrap_or(0),
                    )
                    .children(
                        RequestSection::ALL
                            .into_iter()
                            .map(|section| Tab::new().label(section.label())),
                    )
                    .on_click(cx.listener(|this, index, _, cx| {
                        if let Some(section) = RequestSection::ALL.get(*index) {
                            this.selected_section = *section;
                            cx.notify();
                        }
                    })),
            )
            .child(div().flex_1().min_h_0().child(self.section_content(cx)))
    }
}

fn structured_editor(
    title: impl Into<String>,
    detail: impl Into<String>,
    state: &Entity<InputState>,
    error: Option<String>,
    cx: &App,
) -> gpui::AnyElement {
    gpui_component::v_flex()
        .size_full()
        .gap_2()
        .child(div().font_semibold().child(title.into()))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(detail.into()),
        )
        .when_some(error, |this, error| {
            this.child(div().text_sm().text_color(cx.theme().danger).child(error))
        })
        .child(div().flex_1().min_h_0().child(Input::new(state).h_full()))
        .into_any_element()
}

fn info_state(title: impl Into<String>, detail: impl Into<String>, cx: &App) -> gpui::AnyElement {
    gpui_component::v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().font_semibold().child(title.into()))
        .child(
            div()
                .max_w(px(560.))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(detail.into()),
        )
        .into_any_element()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponseTab {
    Pretty,
    Raw,
    Headers,
    Timeline,
    Tests,
    Console,
}
impl ResponseTab {
    const ALL: [Self; 6] = [
        Self::Pretty,
        Self::Raw,
        Self::Headers,
        Self::Timeline,
        Self::Tests,
        Self::Console,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Pretty => "Pretty",
            Self::Raw => "Raw",
            Self::Headers => "Headers",
            Self::Timeline => "Timeline",
            Self::Tests => "Tests",
            Self::Console => "Console",
        }
    }
}

#[derive(Debug)]
enum ResponseState {
    Idle,
    Running,
    Completed(Box<ExecutionResult>),
    Cancelled,
    Failed(ExecutionError),
}

struct ResponsePanel {
    focus_handle: FocusHandle,
    selected_tab: ResponseTab,
    state: ResponseState,
    console: Vec<String>,
}
impl ResponsePanel {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            selected_tab: ResponseTab::Pretty,
            state: ResponseState::Idle,
            console: Vec::new(),
        }
    }
    fn reset(&mut self) {
        self.selected_tab = ResponseTab::Pretty;
        self.state = ResponseState::Idle;
        self.console.clear();
    }

    fn begin(&mut self) {
        self.state = ResponseState::Running;
        self.console.clear();
    }
    fn apply_message(&mut self, message: NetworkMessage) {
        match message {
            NetworkMessage::Event(event) => self.console.push(format_event(&event)),
            NetworkMessage::Finished(Ok(result)) => self.state = ResponseState::Completed(result),
            NetworkMessage::Finished(Err(ExecutionError::Cancelled)) => {
                self.state = ResponseState::Cancelled;
            }
            NetworkMessage::Finished(Err(error)) => self.state = ResponseState::Failed(error),
        }
    }
    fn summary(&self) -> String {
        match &self.state {
            ResponseState::Idle => "No response yet".to_owned(),
            ResponseState::Running => "Waiting for response…".to_owned(),
            ResponseState::Cancelled => "Cancelled".to_owned(),
            ResponseState::Failed(error) => format!("{:?}: {error}", error.category()),
            ResponseState::Completed(result) => format!(
                "{} {} · {} bytes · {}",
                result.response.status.unwrap_or_default(),
                result.response.status_text.as_deref().unwrap_or(""),
                result.response.received_bytes,
                result.response.protocol_version
            ),
        }
    }
    fn body_preview(&self) -> String {
        let ResponseState::Completed(result) = &self.state else {
            return self.summary();
        };
        match &result.response.stored_body {
            StoredBody::Empty => "<empty response body>".to_owned(),
            StoredBody::InMemory(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            StoredBody::File { path, .. } => format!("Response streamed to {}", path.display()),
            StoredBody::StreamLog(path) => format!("Stream log: {}", path.display()),
        }
    }
    fn content(&self) -> String {
        match self.selected_tab {
            ResponseTab::Pretty | ResponseTab::Raw => self.body_preview(),
            ResponseTab::Headers => match &self.state {
                ResponseState::Completed(result) => result
                    .response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => self.summary(),
            },
            ResponseTab::Timeline => match &self.state {
                ResponseState::Completed(result) => result
                    .timing
                    .iter()
                    .map(|entry| format!("{:?}: {:?}", entry.phase, entry.value))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => self.summary(),
            },
            ResponseTab::Tests => "Assertions have not run for this request.".to_owned(),
            ResponseTab::Console => self.console.join("\n"),
        }
    }
}
impl Focusable for ResponsePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for ResponsePanel {}
impl Panel for ResponsePanel {
    fn panel_name(&self) -> &'static str {
        "ApexResponsePanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Response"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
}
impl Render for ResponsePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .font_semibold()
                    .child(self.summary()),
            )
            .child(
                TabBar::new("response-tabs")
                    .underline()
                    .selected_index(
                        ResponseTab::ALL
                            .iter()
                            .position(|tab| *tab == self.selected_tab)
                            .unwrap_or(0),
                    )
                    .children(
                        ResponseTab::ALL
                            .into_iter()
                            .map(|tab| Tab::new().label(tab.label())),
                    )
                    .on_click(cx.listener(|this, index, _, cx| {
                        if let Some(tab) = ResponseTab::ALL.get(*index) {
                            this.selected_tab = *tab;
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .overflow_hidden()
                    .font_family("monospace")
                    .text_sm()
                    .child(self.content()),
            )
    }
}

struct InspectorPanel {
    focus_handle: FocusHandle,
}
impl InspectorPanel {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}
impl Focusable for InspectorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<PanelEvent> for InspectorPanel {}
impl Panel for InspectorPanel {
    fn panel_name(&self) -> &'static str {
        "ApexInspectorPanel"
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Inspector"
    }
}
impl Render for InspectorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::v_flex()
            .size_full()
            .gap_2()
            .child(div().font_semibold().child("Effective Configuration"))
            .child(
                gpui_component::v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Environment: none")
                    .child("Authentication: none")
                    .child("Timeout: 30 seconds")
                    .child("Redirects: enabled, maximum 10")
                    .child("Cookie jar: session")
                    .child("TLS: certificate verification enabled"),
            )
    }
}

#[derive(Debug)]
enum NetworkMessage {
    Event(ExecutionEvent),
    Finished(Result<Box<ExecutionResult>, ExecutionError>),
}
struct UiEventSink {
    sender: async_channel::Sender<NetworkMessage>,
}
impl ExecutionEventSink for UiEventSink {
    fn emit(&self, event: ExecutionEvent) {
        let _ = self.sender.send_blocking(NetworkMessage::Event(event));
    }
}
struct NetworkEngine {
    adapter: Arc<HttpAdapter>,
}
impl NetworkEngine {
    fn new() -> Self {
        Self {
            adapter: Arc::new(HttpAdapter::new()),
        }
    }
    fn execute(
        &self,
        request: HttpRequest,
        context: ExecutionContext,
    ) -> async_channel::Receiver<NetworkMessage> {
        let (sender, receiver) = async_channel::unbounded();
        let adapter = self.adapter.clone();
        let event_sink: Arc<dyn ExecutionEventSink> = Arc::new(UiEventSink {
            sender: sender.clone(),
        });
        let failure_sender = sender.clone();
        let spawn_result = thread::Builder::new()
            .name("apex-http-execution".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                let result = match runtime {
                    Ok(runtime) => runtime
                        .block_on(adapter.execute(
                            ResolvedRequest {
                                request: ProtocolRequest::Http(request),
                                redacted_summary: "GUI request".to_owned(),
                            },
                            context,
                            event_sink,
                        ))
                        .map(Box::new),
                    Err(error) => Err(ExecutionError::Internal(format!(
                        "failed to create network runtime: {error}"
                    ))),
                };
                let _ = sender.send_blocking(NetworkMessage::Finished(result));
            });
        if let Err(error) = spawn_result {
            let _ = failure_sender.send_blocking(NetworkMessage::Finished(Err(
                ExecutionError::Internal(format!("failed to spawn network worker: {error}")),
            )));
        }
        receiver
    }
}

fn format_event(event: &ExecutionEvent) -> String {
    match event {
        ExecutionEvent::Started { execution_id } => format!("Started {execution_id}"),
        ExecutionEvent::PhaseStarted(phase) => format!("Phase: {phase:?}"),
        ExecutionEvent::UploadProgress {
            sent_bytes,
            total_bytes,
        } => format!(
            "Upload: {sent_bytes}/{}",
            total_bytes.map_or_else(|| "?".to_owned(), |value| value.to_string())
        ),
        ExecutionEvent::ResponseHeaders {
            status,
            http_version,
        } => format!("Response: {status} {http_version}"),
        ExecutionEvent::DownloadProgress {
            received_bytes,
            total_bytes,
        } => format!(
            "Download: {received_bytes}/{}",
            total_bytes.map_or_else(|| "?".to_owned(), |value| value.to_string())
        ),
        ExecutionEvent::StreamItem {
            sequence,
            kind,
            preview,
        } => format!("Stream {sequence} [{kind}]: {preview}"),
        ExecutionEvent::Completed => "Completed".to_owned(),
        ExecutionEvent::Cancelled => "Cancelled".to_owned(),
        ExecutionEvent::Failed {
            category,
            redacted_summary,
        } => format!("Failed [{category:?}]: {redacted_summary}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_workspace(name: &str) -> (WorkspaceRepository, PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("apex-ui-{name}-{nonce}"));
        let repository = WorkspaceRepository::new(&root).expect("repository");
        let manifest = apex_workspace::WorkspaceManifest::new(
            StableId::parse("workspace-test").expect("id"),
            "Test",
        );
        repository.initialize(&manifest).expect("initialize");
        (repository, root)
    }

    #[test]
    fn protocol_backends_validate_and_bound_state() {
        let graphql = GraphqlRequest {
            endpoint: "https://example.test/graphql".to_owned(),
            query: "query Viewer { viewer { id } }".to_owned(),
            operation_name: Some("Viewer".to_owned()),
            variables: serde_json::json!({}),
            headers: BTreeMap::new(),
            persisted_query: false,
            allow_experimental_subscription: false,
        };
        assert!(validate_graphql_request(&graphql).is_ok());
        assert!(build_graphql_http_request(&graphql).is_ok());

        let grpc: GrpcRequest = serde_json::from_value(serde_json::json!({
            "endpoint": "https://grpc.example.test",
            "method": {
                "service": "example.Users",
                "method": "GetUser",
                "input_type": "GetUserRequest",
                "output_type": "User",
                "mode": "Unary"
            },
            "metadata": {},
            "messages": [{"id": "1"}],
            "tls": true,
            "deadline_ms": 1000
        }))
        .expect("grpc request");
        assert!(validate_grpc_request(&grpc).is_ok());

        let mut log = BoundedStreamLog::new(StreamProtocol::WebSocket, 2, 32).expect("log");
        log.connect(&CancellationToken::default()).expect("connect");
        log.push(
            StreamDirection::Incoming,
            Some("message".to_owned()),
            b"one".to_vec(),
        )
        .expect("first");
        log.push(
            StreamDirection::Incoming,
            Some("message".to_owned()),
            b"two".to_vec(),
        )
        .expect("second");
        assert!(log.dropped_events() >= 1);
        assert_eq!(log.filtered("two").len(), 1);
    }

    #[test]
    fn lifecycle_backends_keep_approval_boundaries_explicit() {
        let spec = br#"{
          "openapi":"3.1.0",
          "servers":[{"url":"https://api.example.test"}],
          "paths":{"/users":{"get":{"operationId":"listUsers"}}}
        }"#;
        let document = OpenApiDocument::parse(spec, 1024 * 1024).expect("openapi");
        let generated = document
            .generate_request("listUsers", None)
            .expect("generated");
        assert_eq!(generated.method, "GET");
        assert_eq!(generated.url, "https://api.example.test/users");

        assert_eq!(parse_plugin_capability("viewer"), Ok(Capability::Viewer));
        assert!(parse_plugin_capability("filesystem").is_err());

        let request = AiRequest {
            task: "summarize".to_owned(),
            payload: serde_json::json!({"token":"secret", "value":"public"}),
            metadata: BTreeMap::new(),
        };
        let preview = preview_ai(&request, &["secret".to_owned()]);
        assert!(
            preview
                .redacted_request
                .payload
                .to_string()
                .contains("REDACTED")
        );
        let config = AiConfig {
            enabled: true,
            provider: "local".to_owned(),
            endpoint: None,
            allow_remote: false,
        };
        assert!(
            send_confirmed(
                &config,
                &preview,
                "wrong-token",
                &LocalConfirmationAiProvider,
            )
            .is_err()
        );
        assert!(
            send_confirmed(
                &config,
                &preview,
                &preview.confirmation_token,
                &LocalConfirmationAiProvider,
            )
            .is_ok()
        );
    }

    #[test]
    fn automation_config_and_reports_are_bounded_and_explicit() {
        let config = AutomationConfig::parse(
            "concurrency=4
retries=2
failure_policy=stop
report=junit",
        )
        .expect("config");
        assert_eq!(config.concurrency, 4);
        assert_eq!(config.retries, 2);
        assert_eq!(config.failure_policy, FailurePolicy::Stop);
        assert!(
            AutomationConfig::parse(
                "concurrency=0
retries=0
failure_policy=continue
report=json"
            )
            .is_err()
        );
        assert!(
            AutomationConfig::parse(
                "concurrency=1
retries=0
failure_policy=continue
report=unknown"
            )
            .is_err()
        );
        let summary = RunSummary {
            results: vec![apex_runner::ItemRunResult {
                id: "one".to_owned(),
                name: "One".to_owned(),
                passed: true,
                attempts: 1,
                duration_ms: 10,
                message: "HTTP 200".to_owned(),
            }],
            cancelled: false,
            exit_code: 0,
        };
        assert!(
            format_run_summary(&summary, "json")
                .unwrap()
                .contains("HTTP 200")
        );
        assert!(
            format_run_summary(&summary, "junit")
                .unwrap()
                .contains("testsuite")
        );
        assert!(
            format_run_summary(&summary, "html")
                .unwrap()
                .contains("<html>")
        );
    }

    #[test]
    fn ui_preferences_validate_clamp_and_round_trip() {
        let preferences = UiPreferences::parse(
            "maximum_visible_tabs=100
reduced_motion=true
high_contrast=true",
        )
        .expect("preferences");
        assert_eq!(preferences.maximum_visible_tabs, 20);
        assert!(preferences.reduced_motion);
        assert!(preferences.high_contrast);
        assert_eq!(UiPreferences::parse(&preferences.format()), Ok(preferences));
        assert!(UiPreferences::parse("maximum_visible_tabs=abc").is_err());
        assert!(UiPreferences::parse("unknown=true").is_err());
    }

    #[test]
    fn environment_operations_preserve_secret_boundaries() {
        let (repository, root) = test_workspace("environment-operations");
        apply_environment_operation(&repository, "create|development|Development").expect("create");
        apply_environment_operation(
            &repository,
            "set|development|host|literal|api.example.test|public|true",
        )
        .expect("set literal");
        assert!(
            apply_environment_operation(
                &repository,
                "set|development|token|literal|plaintext|secret|true",
            )
            .is_err()
        );
        apply_environment_operation(
            &repository,
            "set|development|token|secret|env/APEX_TOKEN|secret|true",
        )
        .expect("set secret reference");
        apply_environment_operation(&repository, "default|development").expect("default");
        let loaded = repository
            .load_environment(&StableId::parse("development").expect("id"))
            .expect("load");
        assert_eq!(loaded.value.variables.len(), 2);
        assert!(matches!(
            loaded.value.variables[1].source,
            StoredVariableSource::Secret(_)
        ));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn workspace_output_paths_reject_escape_and_symlink_ancestors() {
        let (repository, root) = test_workspace("output-containment");
        assert!(workspace_relative_path(&repository, "../escape").is_err());
        assert!(workspace_relative_path(&repository, "").is_err());
        let safe =
            workspace_relative_path(&repository, ".apex/generated/code.txt").expect("safe path");
        assert!(safe.starts_with(&root));
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = std::env::temp_dir().join(format!(
                "apex-ui-outside-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::create_dir_all(&outside).expect("outside");
            symlink(&outside, root.join("linked")).expect("symlink");
            assert!(workspace_relative_path(&repository, "linked/file.txt").is_err());
            fs::remove_dir_all(outside).expect("outside cleanup");
        }
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn import_preview_and_codegen_targets_are_explicit() {
        let preview = parse_curl("curl https://example.test/users").expect("preview");
        assert_eq!(preview.requests.len(), 1);
        assert!(!preview.has_errors());
        assert_eq!(code_target("curl"), Ok(CodeTarget::Curl));
        assert_eq!(code_target("go-net-http"), Ok(CodeTarget::GoNetHttp));
        assert!(code_target("unknown").is_err());
        let snippet = generate_code(
            &preview.requests[0].request,
            CodeTarget::Curl,
            CodegenOptions::default(),
        )
        .expect("codegen");
        assert!(snippet.code.contains("curl"));
    }

    #[test]
    fn command_ids_are_stable_and_unique() {
        let mut ids = COMMANDS
            .iter()
            .map(|command| command.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), COMMANDS.len());
        assert!(COMMANDS.iter().all(|command| command.id.contains('.')));
        assert!(
            COMMANDS
                .iter()
                .any(|command| command.id == "environment.cycle")
        );
    }

    #[test]
    fn request_and_response_tabs_have_unique_labels() {
        let mut request = RequestSection::ALL
            .iter()
            .map(|section| section.label())
            .collect::<Vec<_>>();
        request.sort_unstable();
        request.dedup();
        assert_eq!(request.len(), RequestSection::ALL.len());

        let mut response = ResponseTab::ALL
            .iter()
            .map(|tab| tab.label())
            .collect::<Vec<_>>();
        response.sort_unstable();
        response.dedup();
        assert_eq!(response.len(), ResponseTab::ALL.len());
    }

    #[test]
    fn default_draft_contains_no_secret_material() {
        let request = default_request();
        let serialized = apex_workspace::format_request(&RequestDocument::new(request));
        assert!(SecretLeakDetector::default().scan(&serialized).is_empty());
    }

    #[test]
    fn graphql_body_edit_preserves_variables_and_operation_name() {
        let original = RequestBody::GraphQl {
            query: "query Old { viewer { id } }".to_owned(),
            variables_json: "{\"limit\":10}".to_owned(),
            operation_name: Some("Old".to_owned()),
        };
        let edited = body_from_editor(&original, "query New { viewer { name } }".to_owned());
        assert_eq!(
            edited,
            RequestBody::GraphQl {
                query: "query New { viewer { name } }".to_owned(),
                variables_json: "{\"limit\":10}".to_owned(),
                operation_name: Some("Old".to_owned()),
            }
        );
    }

    #[test]
    fn request_tab_window_is_bounded_and_contains_active_tab() {
        for tab_count in 0..32 {
            for active in 0..tab_count.max(1) {
                let range = request_tab_window(
                    tab_count,
                    Some(active),
                    DEFAULT_MAXIMUM_VISIBLE_REQUEST_TABS,
                );
                assert!(range.len() <= DEFAULT_MAXIMUM_VISIBLE_REQUEST_TABS);
                assert!(range.end <= tab_count);
                if tab_count > 0 {
                    assert!(range.contains(&active.min(tab_count - 1)));
                }
            }
        }
    }

    #[test]
    fn request_tab_window_centers_then_clamps_at_edges() {
        assert_eq!(request_tab_window(10, Some(0), 6), 0..6);
        assert_eq!(request_tab_window(10, Some(5), 6), 3..9);
        assert_eq!(request_tab_window(10, Some(9), 6), 4..10);
        assert_eq!(request_tab_window(3, Some(1), 6), 0..3);
    }

    #[test]
    fn structured_field_formats_round_trip_duplicates_and_metadata() {
        let fields = vec![
            FormField {
                name: "tag".to_owned(),
                value: "one".to_owned(),
                enabled: true,
                sensitivity: ValueSensitivity::Public,
            },
            FormField {
                name: "tag".to_owned(),
                value: "two".to_owned(),
                enabled: false,
                sensitivity: ValueSensitivity::Sensitive,
            },
        ];
        assert_eq!(parse_form_fields(&format_form_fields(&fields)), Ok(fields));

        let mut first = HeaderEntry::new("X-Test", "one").expect("valid header");
        first.sensitivity = ValueSensitivity::Secret;
        let mut second = HeaderEntry::new("X-Test", "two").expect("valid header");
        second.enabled = false;
        let headers = vec![first, second];
        assert_eq!(parse_headers(&format_headers(&headers)), Ok(headers));
    }

    #[test]
    fn structured_authentication_and_settings_round_trip() {
        let authentication = Authentication::ApiKey {
            name: "X-Api-Key".to_owned(),
            value: "{{secret.api_key}}".to_owned(),
            placement: ApiKeyPlacement::Header,
        };
        assert_eq!(
            parse_authentication(&format_authentication(&authentication)),
            Ok(authentication)
        );

        let settings = RequestSettings::default();
        assert_eq!(
            parse_request_settings(&format_request_settings(&settings)),
            Ok(settings)
        );
    }

    #[test]
    fn multipart_editor_round_trip_preserves_file_and_text_fields() {
        let fields = vec![
            MultipartField {
                name: "metadata".to_owned(),
                value: MultipartValue::Text("{}".to_owned()),
                content_type: Some("application/json".to_owned()),
                enabled: true,
                sensitivity: ValueSensitivity::Public,
            },
            MultipartField {
                name: "upload".to_owned(),
                value: MultipartValue::File {
                    relative_path: "fixtures/file.bin".to_owned(),
                },
                content_type: None,
                enabled: false,
                sensitivity: ValueSensitivity::Sensitive,
            },
        ];
        assert_eq!(
            parse_multipart_fields(&format_multipart_fields(&fields)),
            Ok(fields)
        );
    }

    #[test]
    fn invalid_structured_editor_lines_fail_closed() {
        assert!(parse_form_fields("enabled;public;missing-pair").is_err());
        assert!(parse_headers("enabled;public;bad header=value").is_err());
        assert!(parse_authentication("bearer").is_err());
        assert!(parse_request_settings("timeout_seconds=abc").is_err());
        assert!(parse_multipart_fields("enabled;public;unknown;;name=value").is_err());
    }

    #[test]
    fn structured_form_body_is_editable_and_round_trips() {
        let original = RequestBody::FormUrlEncoded(vec![FormField {
            name: "tag".to_owned(),
            value: "one".to_owned(),
            enabled: true,
            sensitivity: ValueSensitivity::Public,
        }]);
        assert!(body_is_editable(&original));
        assert_eq!(
            body_from_editor(&original, editor_body(&original)),
            original
        );
    }

    #[test]
    fn collection_tree_groups_nested_workspace_requests() {
        let entries = vec![WorkspaceRequestEntry {
            path: PathBuf::from("/workspace/collections/users/admin/get.request.toml"),
            relative_path: PathBuf::from("collections/users/admin/get.request.toml"),
            collection: "users".to_owned(),
            folders: vec!["admin".to_owned()],
            slug: "get".to_owned(),
            id: StableId::parse("get-user").expect("valid id"),
            name: "Get user".to_owned(),
            method: HttpMethod::Get,
            url: "https://example.test/users/1".to_owned(),
        }];
        let tree = workspace_tree(&entries);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].label.as_str(), "users");
        assert_eq!(tree[0].children[0].label.as_str(), "admin");
        assert_eq!(
            tree[0].children[0].children[0].label.as_str(),
            "GET  Get user"
        );
    }
}
