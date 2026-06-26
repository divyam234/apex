# Phase 4B environment and shared-resolution checkpoint manifest

Version: `0.1.0-alpha.1`  
Date: 2026-06-24  
Toolchain: Rust 1.96.0, edition 2024

## Included

- Phase 1 domain, variables, secrets, workspace, import, and runner foundations.
- Phase 2B Tokio/Hyper/Rustls HTTP adapter, auth, cookies, decompression, history, CLI, and tests.
- Registry-backed `gpui 0.2.2`, `gpui-component 0.5.1`, and assets locked in `Cargo.lock`.
- Real native `apps/apex` application and `apex-ui` shell.
- Native title bar, Dock panels, virtualized collection tree, editors, tabs, command palette,
  notifications, status bar, keyboard actions, and shared-engine Send/Cancel.
- Workspace-path launch, nested request indexing, collection navigation, atomic request editing, and
  active-workspace resource roots.
- Pure tab/session model with unsaved-change guards and lifecycle tests.
- Durable workspace/environment/local-override variable documents and schema.
- Shared GUI/CLI request resolution with strict pre-send errors and environment selection.
- Redacted `env list`/`env inspect` CLI workflows and environment fixtures.
- Updated architecture, UI, build, feature, troubleshooting, release, and phase documents.

## Excluded

- `target/` build products and linked debug binary.
- Uploaded/local Rust toolchains.
- Cargo registry/source caches and Cargo credentials.
- Build-only native-library aliases.
- Git metadata.
- `.apex/` history/local state.
- Temporary response/download/compressed-spool files.
- User workspace content or secret values.

## Known incomplete gates

Visual desktop smoke, Phase 2C networking/auth, remaining Phase 4 productivity, scripts/runner,
additional protocols, OpenAPI/mocks, Git/WASM/AI, accessibility, benchmarks, and distributable
packages are not claimed complete.
