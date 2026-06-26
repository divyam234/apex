# GPUI and gpui-component audit

## Resolved versions

The build uses Cargo-registry releases resolved through Artifactory and locked in `Cargo.lock`:

```text
gpui = 0.2.2
gpui-component = 0.5.1
gpui-component-assets = 0.5.1
```

This replaced the earlier unnecessary Git-only probe. The package lockfile provides exact package
checksums and transitive versions. Upgrades must remain explicit compatibility changes.

## Verified initialization

The real application follows the upstream sequence:

1. construct `gpui::Application` and attach `gpui_component_assets::Assets`;
2. call `gpui_component::init(cx)` before creating component entities;
3. open a native window with `TitleBar::title_bar_options()`;
4. place `gpui_component::Root` at the first window level.

## APIs inspected and used

- Dock: `DockArea`, `DockItem`, `Panel`, `PanelEvent`, `PanelStyle`.
- Editor/input: `InputState::new(window, cx)`, `code_editor`, `multi_line`, `Input`.
- Virtual data: `TreeState`, `TreeItem`, `tree`, `ListItem`.
- Tabs: `TabBar`, `Tab`.
- Overlays: `WindowExt::open_dialog`, notifications through `push_notification`.
- Native chrome: `TitleBar`.
- Actions/focus: `actions!`, `KeyBinding`, key contexts, action listeners, `FocusHandle`.

The published crate package contains the component source but not the upstream story/gallery crate.
The component implementations and documented examples were inspected directly. ApexAPI's actual
shell is a stronger compile check than the former one-view probe.

## Build result

`cargo check --workspace --all-targets` passes and `cargo build -p apex-gui` links a native Linux
ELF binary. The container's headless X server has no GPU device accepted by GPUI's renderer, so a
visual launch is not claimed. Wayland/X11 desktop smoke tests remain a release-host gate.

## Upgrade controls

- Keep all GPUI types inside `apex-ui` and `apps/apex`.
- Upgrade package versions only on a compatibility branch.
- Compile the smallest initialization path first.
- Compile each added panel/editor/tree behavior before expanding the shell.
- Run model tests and Linux Wayland/X11 launch tests before accepting an upgrade.
