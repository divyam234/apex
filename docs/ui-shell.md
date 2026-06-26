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
- request and response tab bars;
- command palette dialog and stable command IDs;
- keyboard bindings for New, Send, Save, Focus URL, command palette, and Cancel;
- notifications and a bottom status bar.

The UI never owns the HTTP implementation. `NetworkEngine` launches execution on a named worker
thread with a Tokio runtime and sends `ExecutionEvent`/`ExecutionResult` messages back over a
channel. Cancellation uses the same shared token as the CLI.

## Workspace behavior

The GUI can open a real workspace, load its manifest, index nested `*.request.toml` files, group
them by collection/folder, and open them from the tree. A loaded request retains fields that do not
currently have a structured editor. Atomic saves use the workspace repository and its loaded-file
fingerprint; a changed file is rejected rather than overwritten.

New unsaved requests use the application-state draft workspace. The current checkpoint has one
visible request editor; the pure tab/session model already covers resource identity, preview tabs,
pinning, reorder, close-other/right, unsaved-change guards, and reopen-closed behavior, but the
multi-document visual tab strip is not yet wired to that model.

## Validation boundary

The native binary compiles and links on Linux. A launch under Xvfb reached GPUI platform
initialization but the container exposes no supported GPU device, so a visual render smoke test is
not claimed. This is distinct from compilation or linking failure; desktop Wayland/X11 smoke tests
remain a release-host gate.
