#![forbid(unsafe_code)]

pub mod session;

use apex_domain::{
    Authentication, CancellationToken, ExecutionError, ExecutionEvent, HeaderEntry, HttpMethod,
    HttpRequest, RequestBody, RequestSettings, StableId,
};
use apex_http::HttpAdapter;
use apex_runner::{
    ExecutionContext, ExecutionEventSink, ExecutionResult, ProtocolAdapter, ProtocolRequest,
    ResolvedRequest, StoredBody,
};
use apex_secrets::{EnvironmentSecretStore, SecretLeakDetector, SecretStoreChain};
use apex_variables::{
    ResolverOptions, SystemDynamicVariables, VariableContext, WorkspaceVariableSelection,
    load_workspace_variables, resolve_http_request,
};
use apex_workspace::{
    EnvironmentSummary, FileFingerprint, RequestDocument, WorkspaceRepository,
    WorkspaceRequestEntry,
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
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

actions!(
    apex,
    [
        NewRequest,
        SendRequest,
        CancelRequest,
        SaveRequest,
        OpenCommandPalette,
        FocusUrl,
        CycleEnvironment
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
    request_panel: Entity<RequestPanel>,
    workspace_label: String,
    repository: Option<WorkspaceRepository>,
    environments: Vec<EnvironmentSummary>,
    selected_environment: Option<String>,
    environment_label: String,
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
        let inspector_panel = cx.new(InspectorPanel::new);
        let dock_area = cx.new(|cx| {
            DockArea::new("apex-main-dock", Some(1), window, cx).panel_style(PanelStyle::TabBar)
        });
        let dock_weak = dock_area.downgrade();

        let center = DockItem::tab(request_panel.clone(), &dock_weak, window, cx);
        let left = DockItem::tab(collections_panel, &dock_weak, window, cx);
        let right = DockItem::tab(inspector_panel, &dock_weak, window, cx);
        let bottom = DockItem::tab(response_panel, &dock_weak, window, cx);

        dock_area.update(cx, |dock, cx| {
            dock.set_center(center, window, cx);
            dock.set_left_dock(left, Some(px(292.)), true, window, cx);
            dock.set_right_dock(right, Some(px(300.)), false, window, cx);
            dock.set_bottom_dock(bottom, Some(px(300.)), true, window, cx);
        });

        Self {
            dock_area,
            request_panel,
            workspace_label: browser.label,
            repository: browser.repository,
            environments: browser.environments,
            selected_environment,
            environment_label,
        }
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

struct CollectionsPanel {
    focus_handle: FocusHandle,
    tree_state: Entity<TreeState>,
    request_panel: Entity<RequestPanel>,
    repository: Option<WorkspaceRepository>,
    request_paths: Arc<HashMap<String, PathBuf>>,
    error: Option<String>,
}

impl CollectionsPanel {
    fn new(
        request_panel: Entity<RequestPanel>,
        browser: WorkspaceBrowser,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let request_paths = Arc::new(
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
        );
        let items = if browser.requests.is_empty() {
            vec![
                TreeItem::new("local-drafts", "Local Drafts")
                    .expanded(true)
                    .child(TreeItem::new("gui-draft", "GUI Draft")),
            ]
        } else {
            workspace_tree(&browser.requests)
        };
        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        Self {
            focus_handle: cx.focus_handle(),
            tree_state,
            request_panel,
            repository: browser.repository,
            request_paths,
            error: browser.error,
        }
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
                    if !entry.is_folder()
                        && let (Some(repository), Some(path)) =
                            (repository.clone(), request_paths.get(&id).cloned())
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
        let root = draft_state_root()?.join("apex-api").join("draft-workspace");
        let repository =
            WorkspaceRepository::new(root.clone()).map_err(|error| error.to_string())?;
        let path = root
            .join("collections")
            .join("local-drafts")
            .join("gui-draft.request.toml");
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
        _ => String::new(),
    }
}

fn body_is_editable(body: &RequestBody) -> bool {
    matches!(
        body,
        RequestBody::Empty
            | RequestBody::Json(_)
            | RequestBody::Xml(_)
            | RequestBody::Text { .. }
            | RequestBody::GraphQl { .. }
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
        other => other.clone(),
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

struct RequestPanel {
    focus_handle: FocusHandle,
    url_state: Entity<InputState>,
    body_state: Entity<InputState>,
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
    variable_context: VariableContext,
    environment_label: String,
    variable_error: Option<String>,
    dirty: bool,
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
        let (document_store, request, document_fingerprint, document_error) =
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
        let url_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.example.com/v1/resource")
                .default_value(request.url.clone())
        });
        let body_state = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("json")
                .multi_line(true)
                .default_value(editor_body(&request.body))
        });
        let url_subscription = cx.subscribe(&url_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.dirty = true;
                cx.notify();
            }
        });
        let body_subscription = cx.subscribe(&body_state, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.dirty = true;
                cx.notify();
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            url_state,
            body_state,
            selected_section: RequestSection::Params,
            method: request.method.clone(),
            base_request: request,
            response_panel,
            network,
            cancellation: None,
            request_generation: 0,
            document_store,
            document_fingerprint,
            document_error,
            variable_context,
            environment_label,
            variable_error,
            dirty: false,
            _subscriptions: vec![url_subscription, body_subscription],
        }
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

    fn current_request(&self, cx: &App) -> HttpRequest {
        let mut request = self.base_request.clone();
        request.method = self.method.clone();
        request.url = self.url_state.read(cx).value().to_string();
        if body_is_editable(&request.body) {
            request.body =
                body_from_editor(&request.body, self.body_state.read(cx).value().to_string());
        }
        request
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
        self.body_state.update(cx, |state, cx| {
            state.set_value(editor_body(&request.body), window, cx);
        });
        self.base_request = request;
        self.document_store = Some(store);
        self.document_fingerprint = Some(fingerprint);
        self.document_error = None;
        self.selected_section = RequestSection::Params;
        self.dirty = false;
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
        if self.dirty {
            return Err(
                "save or discard the current request before opening another file".to_owned(),
            );
        }
        let (request, fingerprint) = store.load()?;
        self.apply_request(request, store, fingerprint, window, cx);
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
        self.dirty = true;
        cx.notify();
    }

    fn new_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel(cx);
        let request = default_request();
        let draft_store = DocumentStore::open_draft();
        let (store, fingerprint, store_error) = match draft_store {
            Ok((store, loaded)) => (
                Some(store),
                loaded.map(|(_, fingerprint)| fingerprint),
                None,
            ),
            Err(error) => (None, None, Some(error)),
        };
        self.method = request.method.clone();
        self.url_state
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.body_state
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.base_request = request;
        self.document_store = store;
        self.document_fingerprint = fingerprint;
        self.document_error = store_error;
        self.selected_section = RequestSection::Params;
        self.dirty = true;
        self.response_panel.update(cx, |panel, cx| {
            panel.reset();
            cx.notify();
        });
        cx.notify();
    }

    fn save_draft(&mut self, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        if let Some(error) = &self.document_error {
            return Err(error.clone());
        }
        let request = self.current_request(cx);
        let store = self
            .document_store
            .as_ref()
            .ok_or_else(|| "document store is unavailable".to_owned())?;
        let fingerprint = store.save(&request, self.document_fingerprint)?;
        self.document_fingerprint = Some(fingerprint);
        self.base_request = request;
        self.dirty = false;
        let path = store.path().to_owned();
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
        let unresolved_request = self.current_request(cx);
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
            RequestSection::Params => info_state(
                "Query parameters",
                "Ordered duplicate query parameters are supported by the execution model and workspace format.",
                cx,
            ),
            RequestSection::Authorization => info_state(
                "Authentication",
                "Basic, Bearer and API-key authentication are engine-backed; durable values must use secret references.",
                cx,
            ),
            RequestSection::Headers => info_state(
                "Headers",
                "Ordered duplicate and disabled headers are preserved by the domain model.",
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
            RequestSection::Settings => info_state(
                "Effective defaults",
                format!(
                    "Environment: {} · 30s request timeout · 10 redirects · 64 MiB decoded limit · cookies and decompression enabled.",
                    self.environment_label
                ),
                cx,
            ),
            RequestSection::Documentation => info_state(
                "Documentation",
                "Request documentation is persisted in the Git-friendly request format when a workspace request is loaded.",
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
            "{} {}{}",
            self.method.as_str(),
            self.base_request.name,
            if self.dirty { " •" } else { "" }
        )
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
}
impl Render for RequestPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.is_running();
        gpui_component::v_flex()
            .size_full()
            .gap_2()
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
    fn structured_non_text_body_is_preserved_by_editor_projection() {
        let original = RequestBody::FormUrlEncoded(vec![apex_domain::FormField {
            name: "tag".to_owned(),
            value: "one".to_owned(),
            enabled: true,
            sensitivity: apex_domain::ValueSensitivity::Public,
        }]);
        assert!(!body_is_editable(&original));
        assert_eq!(body_from_editor(&original, "ignored".to_owned()), original);
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
