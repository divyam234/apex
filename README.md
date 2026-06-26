# ApexAPI

ApexAPI is a local-first native API development client being built in Rust with GPUI and
`longbridge/gpui-component`. Branding is isolated behind `apex_domain::branding`; durable workspace
files do not depend on the product display name.

## 1. Competitor feature audit

The source-linked audit is in [`docs/competitor-audit.md`](docs/competitor-audit.md). ApexAPI keeps
useful workflow ideas—collections, environments, runners, protocol tooling, API schemas, and
Git-oriented ownership—without cloning a competitor's interface.

## 2. Important competitor weaknesses to avoid

ApexAPI avoids mandatory accounts, opaque synchronization, monolithic collection files, plaintext
secret persistence, GUI-only execution behavior, unbounded body buffering, ambiguous inheritance,
ambient-capability plugins, fake network responses, and success messages for operations that did
not happen.

## 3. Product differentiators

- Native GPUI interaction without Electron, WebView, HTML, CSS, or a JavaScript UI.
- One request per stable, human-readable workspace file.
- Strict separation of secret references from ordinary workspace data.
- One execution engine and error model shared by GUI, CLI, runners, and monitors.
- Explicit variable-resolution traces and typed execution diagnostics.
- Streaming uploads, bounded response storage, and atomic direct-to-disk downloads.
- Honest stable, experimental, foundation, and unsupported capability labels.

## 4. Architecture overview

Durable models and protocol execution do not depend on GPUI. `apex-http` implements the first real
adapter over Tokio, Hyper, hyper-util, and Rustls. Both `apex-cli` and the native `apex` GUI call it
through `apex-runner`. The GUI receives structured progress over a channel and performs networking
outside the GPUI render thread. See [`docs/architecture.md`](docs/architecture.md),
[`docs/http-engine.md`](docs/http-engine.md), and [`docs/ui-shell.md`](docs/ui-shell.md).

## 5. Security model

Implemented controls include redacted secret values, zeroed secret buffers, session/process
environment secret stores, plaintext-secret leak checks, path traversal rejection, canonical
workspace containment for uploaded files, bounded reads and responses, atomic writes/downloads,
external-change detection, credential stripping on cross-origin redirects, URL-userinfo rejection,
and untrusted workspace defaults. See [`docs/security-model.md`](docs/security-model.md).

## 6. Workspace tree

```text
apex-api/
├── apps/
│   ├── apex/                 # real native GPUI application
│   └── apex-cli/             # headless client using the same execution engine
├── crates/
│   ├── apex-domain/
│   ├── apex-workspace/       # files, request index, atomic saves, conflict detection
│   ├── apex-variables/
│   ├── apex-secrets/
│   ├── apex-auth/
│   ├── apex-runner/
│   ├── apex-http/
│   ├── apex-history/
│   ├── apex-import/
│   ├── apex-ui/              # GPUI shell and testable UI/session models
│   └── apex-test-support/
├── docs/
├── fixtures/
└── packaging/
```

## 7. Phased execution plan

Phase 1 and the implemented Phase 2 HTTP foundation remain intact. Phase 3 now has a compiled and
linked native shell. Phase 4 now includes real workspace opening, nested request indexing,
collection-tree navigation, atomic request editing, resource-root propagation, tested tab lifecycle
models, durable environments, and one shared GUI/CLI request resolver. Later protocol, automation, contract, Git/plugin, accessibility, and packaging
phases remain explicitly incomplete. See [`docs/phase-plan.md`](docs/phase-plan.md).

## Current implementation

- Native title bar, activity bar, GPUI Dock panels, virtualized collection tree, request/response
  tabs, editors, command palette, notifications, keyboard actions, and status bar.
- Real GUI Send/Cancel using the same `HttpAdapter` as `apex send`.
- GUI request work runs off the GPUI render thread and streams structured progress back to the UI.
- Open a workspace by positional path or `--workspace`; nested request files are indexed and opened
  from the collection tree.
- Loaded requests preserve query fields, headers, auth, settings, documentation, and non-text body
  variants when only URL/method/body text is edited.
- Dirty documents are not replaced silently, and saves retain external-change fingerprint checks.
- Tested resource-tab/session model for preview tabs, pinning, reorder, close-right, dirty guards,
  and reopening closed tabs.
- Git-friendly `variables.toml`, environment files, and ignored local overrides with nested values,
  secret references, process-environment sources, deterministic precedence, and redacted inspection.
- Native environment switcher and `--environment/-e` CLI parity. URL, query, headers, auth, all body
  variants, and multipart paths are resolved by one shared strict resolver before execution.
- HTTP/1.1, HTTP/2 negotiation, Rustls HTTPS, redirects, trailers, streamed uploads/downloads,
  Basic/Bearer/API-key auth, session cookies, decompression, cancellation, and local SQLite history.

## Build and run

The workspace uses stable Rust 1.96, edition 2024. Dependencies resolve from the configured Cargo
registry mirror and are pinned by `Cargo.lock`.

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p apex-gui

# local draft workspace
cargo run -p apex-gui

# open an Apex workspace
cargo run -p apex-gui -- --workspace /path/to/workspace --environment staging
cargo run -p apex-cli -- env list /path/to/workspace
cargo run -p apex-cli -- doctor
```

Linux build hosts need the normal X11/Wayland and `libxkbcommon` development libraries. The supplied
container had runtime `libxkbcommon.so.0` files but lacked unversioned development linker names; the
application linked after exposing those runtime libraries through a local build-only library path.
No host-library symlink or Cargo credential is included in source checkpoints.
