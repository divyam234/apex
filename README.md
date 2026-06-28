# ApexAPI

ApexAPI is a local-first API development client written in Rust. It combines a native GPUI desktop application, a headless CLI, and shared execution libraries so requests behave consistently across interactive use, automation, runners, and monitors.

The project is currently published as `0.1.0-alpha.1`. Core libraries and the portable CLI are extensively validated. The native desktop application compiles and links, but final desktop smoke testing, accessibility review, signed installers, and distribution-specific package builds remain release-environment gates.

## Why ApexAPI

- Native Rust UI with GPUI instead of Electron, a browser shell, or a JavaScript frontend.
- Local-first, Git-friendly workspaces with one request per human-readable file.
- Shared request resolution and execution across GUI, CLI, runners, and monitors.
- Secret references remain separate from ordinary workspace data.
- Streaming uploads, bounded response storage, cancellation, redirects, cookies, and atomic downloads.
- Explicit conflict handling when files change outside the application.
- Bounded scripting, plugin, mock-server, search, history, and protocol foundations.
- Honest capability labels for stable, experimental, adapter-owned, and release-host-dependent features.

## Current capabilities

### Native desktop application

The GPUI application includes:

- integrated title bar, activity bar, status bar, and notifications;
- docked collection, request, response, inspector, and history surfaces;
- virtualized collection navigation;
- URL and body editing;
- real Send, Cancel, Save, Reload, and conflict-resolution flows;
- command palette and keyboard shortcuts;
- environment selection;
- background HTTP execution and filesystem monitoring outside the render thread;
- clean-document reload prompts and dirty-document conflict protection.

The underlying tab/session model supports preview tabs, pinning, reordering, close guards, reopen-closed behavior, and stable resource identity. Full multi-document visual wiring and real-desktop visual polish remain ongoing UI work.

### CLI and automation

The CLI shares the same workspace, variable-resolution, authentication, HTTP, history, import, export, and runner layers as the desktop application.

Implemented workflows include:

- request execution with environment selection;
- environment create, rename, delete, default, and inspection operations;
- indexed workspace search;
- Postman Collection v2.1 import previews with unsupported-field diagnostics;
- redacted code generation;
- history filtering, restoration, resend, snapshots, and semantic response diff;
- bounded collection execution with retries, cancellation, live events, and deterministic reports;
- headless monitor execution and systemd user-unit generation.

### Protocol and lifecycle foundations

ApexAPI includes tested foundations for:

- HTTP/1.1 and HTTP/2 over Rustls;
- GraphQL query, mutation, introspection, persisted-query, and response models;
- bounded WebSocket and SSE session logs;
- gRPC unary and streaming interaction models;
- OpenAPI 3.0 and 3.1 parsing, browsing, request generation, validation, diff, and Markdown documentation;
- secure loopback-default mock servers;
- shell-free Git workflow helpers and workspace trust enforcement;
- constrained Rhai scripting;
- constrained WebAssembly plugin validation and capability approval;
- disabled-by-default provider-neutral AI integration boundaries.

Some advanced transports remain adapter-owned rather than fully integrated end-to-end. See [release status](docs/release-status.md) and the [feature matrix](docs/feature-matrix.md) for precise boundaries.

## Security model

Security-sensitive behavior is designed to fail closed. Implemented controls include:

- plaintext secret rejection in durable workspace files;
- redacted diagnostics and generated snippets;
- session and process-environment secret sources;
- path traversal and symlink rejection for protected operations;
- bounded reads, bodies, logs, plugin output, and decompression ratios;
- atomic saves and downloads;
- external-change fingerprints;
- cross-origin credential stripping;
- URL-userinfo rejection;
- untrusted-workspace capability restrictions;
- deterministic malformed-input and fuzz-smoke coverage.

Read the full [security model](docs/security-model.md) and [security policy](SECURITY.md).

## Architecture

ApexAPI keeps durable models and execution logic independent of GPUI.

```text
apps/
├── apex/                 Native GPUI desktop application
└── apex-cli/             Headless CLI

crates/
├── apex-domain/          Core request and response types
├── apex-workspace/       Workspace files, indexing, mutations, watch/reconcile
├── apex-variables/       Deterministic variable resolution and source tracing
├── apex-secrets/         Secret handling and redaction
├── apex-auth/            Authentication application
├── apex-http/            Real Tokio/Hyper/Rustls HTTP adapter
├── apex-runner/          Execution, assertions, collections, monitors, reports
├── apex-history/         SQLite history, snapshots, restore, semantic diff
├── apex-import/          cURL and Postman import
├── apex-export/          Redacted code generation
├── apex-protocols/       GraphQL, stream, and gRPC models
├── apex-contracts/       OpenAPI workflows
├── apex-mock/            Local mock server
├── apex-git/             Git and trust controls
├── apex-scripting/       Constrained script runtime
├── apex-plugins/         Constrained WASM plugin boundary
├── apex-ai/              Optional provider-neutral AI boundary
├── apex-quality/         Accessibility and performance primitives
├── apex-security/        Security regression suite
└── apex-ui/              GPUI shell and testable UI/session models
```

More detail is available in [architecture](docs/architecture.md), [HTTP engine](docs/http-engine.md), [UI shell](docs/ui-shell.md), and [file format](docs/file-format.md).

## Requirements

- Rust 1.96
- Cargo with the repository lockfile
- Linux desktop builds: normal X11 or Wayland development libraries and `libxkbcommon`

The portable CLI builds on Linux, macOS, and Windows in CI. Native desktop packaging is currently prepared primarily around Linux, with macOS and Windows release notes documenting the remaining signing and packaging work.

## Build

```bash
cargo build --workspace --locked
```

Build the optimized CLI:

```bash
cargo build --release --locked -p apex-cli
```

Build the desktop application:

```bash
cargo build -p apex-gui
```

## Run

Start the desktop application with a draft workspace:

```bash
cargo run -p apex-gui
```

Open an existing workspace:

```bash
cargo run -p apex-gui -- --workspace /path/to/workspace --environment staging
```

Inspect CLI commands:

```bash
cargo run -p apex-cli -- --help
```

Example workspace operations:

```bash
cargo run -p apex-cli -- env list /path/to/workspace
cargo run -p apex-cli -- doctor
```

## Validation

The main automated quality gates are:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude apex-ui --exclude apex-gui
cargo build --release --locked -p apex-cli
./scripts/validate-release-metadata.sh
git diff --check
```

The non-GPUI workspace suite currently contains 183 passing unit tests plus documentation tests. UI-independent session and reconciliation behavior is tested separately. Native desktop rendering, assistive-technology testing, package signing, clean distribution builds, and long-running fuzz campaigns require dedicated release hosts.

## Packaging

Repository metadata is provided for:

- AppImage
- Arch Linux
- Debian
- RPM-based distributions
- Nix
- macOS preparation
- Windows preparation

See [packaging documentation](docs/packaging.md) and the [release checklist](docs/release-checklist.md).

## Documentation

- [Architecture](docs/architecture.md)
- [CLI](docs/cli.md)
- [Feature matrix](docs/feature-matrix.md)
- [File format](docs/file-format.md)
- [Import and export](docs/import-export.md)
- [Migrations](docs/migrations.md)
- [Release status](docs/release-status.md)
- [Security model](docs/security-model.md)
- [Troubleshooting](docs/troubleshooting.md)

## Project status

The implementation roadmap through the current hardening and release-foundation phase is complete. That means the code, tests, CI definitions, release metadata, documentation, and portable CLI release build are in place.

It does not mean every platform-specific release gate has been executed. The remaining external gates are listed explicitly in [docs/release-checklist.md](docs/release-checklist.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
