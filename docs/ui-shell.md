# Native UI shell

## Resolved native stack

The workspace resolves through the configured Cargo registry mirror to:

- `gpui 0.2.2`
- `gpui-component 0.5.1`
- `gpui-component-assets 0.5.1`

Exact package checksums and transitive versions are pinned in `Cargo.lock`. The application uses
`gpui::Application`, calls `gpui_component::init`, and places `gpui_component::Root` at the first
window level.

## Implemented shell

`apps/apex` creates the native process and accepts a positional workspace path or
`--workspace/-w`. `apex-ui` owns:

- integrated native title bar;
- activity bar;
- `DockArea` with collection, request, response, and inspector panels;
- GPUI Component editor/input state for URL and body text;
- virtualized `TreeState` collection navigation;
- visual multi-document request tabs and response tab bars;
- command palette dialog and stable command IDs;
- keyboard bindings for New, Send, Save, Focus URL, command palette, and Cancel;
- notifications and a bottom status bar.

The UI never owns the HTTP implementation. `NetworkEngine` launches execution on a named worker
thread with a Tokio runtime and sends `ExecutionEvent`/`ExecutionResult` messages back over a
channel. Cancellation uses the same shared token as the CLI.

## Workspace behavior

The GUI can open a real workspace, load its manifest, index nested `*.request.toml` files, group
them by collection/folder, and open them from the tree. The request surface provides validated
editors for URL, ordered query fields, duplicate headers, authentication, all body variants, request
settings, and documentation. Form and header rows preserve enabled state and sensitivity; multipart
rows preserve text/file kind and content type. Invalid editor content blocks save and send rather
than silently dropping fields. Atomic saves use the workspace repository and its loaded-file
fingerprint; a changed file is rejected rather than overwritten.

New unsaved requests use the application-state draft workspace. The request surface now wires the
resource-aware session model into a visual multi-document tab strip. Opening a workspace request
creates or activates its tab, switching tabs restores editor and conflict state, saved tabs can be
closed, dirty tabs are guarded, and history restores open as separate draft tabs. Pin/unpin,
preview promotion, deterministic left/right reorder, reopen-closed, close-other/right, bounded
overflow navigation, and startup restoration are exposed in the native tab strip. Drag-and-drop
reordering remains intentionally outside the current phase.

## Validation boundary

The native binary compiles and links on Linux. A launch under Xvfb reached GPUI platform
initialization but the container exposes no supported GPU device, so a visual render smoke test is
not claimed. This is distinct from compilation or linking failure; desktop Wayland/X11 smoke tests
remain a release-host gate.

## Workspace tools, automation, protocols, and lifecycle

The right dock includes Workspace Tools, Protocols, and Lifecycle panels. Workspace Tools manages environments, validated preferences, indexed search, import previews, and redacted code generation. The Automation panel executes real workspace HTTP requests with bounded concurrency, retries, cancellation, and deterministic reports. Protocols exposes GraphQL validation/building, bounded WebSocket/SSE session logs, and gRPC descriptor validation/reflection requests. Lifecycle exposes OpenAPI generation, loopback mock control, guarded Git operations, capability-approved plugin validation, and redacted AI preview/confirmation.

Live WebSocket/SSE transports, production gRPC networking, bundled WebAssembly execution, and remote AI provider adapters remain explicit external boundaries.

## Keyboard and bounded rendering

Request tabs support Ctrl/Cmd+PageUp and Ctrl/Cmd+PageDown navigation plus Ctrl/Cmd+Shift+T reopen. The visible-tab limit is persisted and clamped to 1–20. History, search results, response previews, collection execution, and protocol event logs use explicit bounds to avoid unbounded desktop rendering or memory growth.
