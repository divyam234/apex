# Phase 4G history/snapshot/semantic-diff checkpoint manifest

Version: `0.1.0-alpha.1`
Date: 2026-06-26
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
- Recursive workspace observation with typed relative resource events, a bounded queue, explicit
  rescan-on-overflow behavior, temporary/internal-path filtering, and traversal rejection.
- Deterministic watcher normalization tests plus a real bounded Linux filesystem observation test.
- A bounded native-shell monitor bridge, live request-tree refresh, off-thread fingerprint checks,
  visible clean reload and dirty-conflict actions, and save blocking while disk state is unresolved.
- Stable collection/folder metadata and explicit order files with guarded create, rename, move,
  duplicate, archive, and delete operations.
- Staged duplication with stable-ID re-keying, symlink rejection, bounded directory fingerprints, and
  rollback tests.
- Atomic environment create/update/rename/default/delete operations, ignored local-override lifecycle,
  redacted effective-source inspection, and CLI administration commands.
- Bounded incremental request search in local SQLite with sensitive/auth exclusion and CLI filters.
- Strict Postman Collection v2.1 preview with supported request/body conversion, explicit
  unsupported-field diagnostics, byte/item/depth limits, and no credential retention.
- Redacted code generation for five targets with duplicate-header preservation or warnings and
  explicit unsupported-assembly diagnostics.
- CLI `search`, `import-postman`, and `codegen` workflows using the shared core crates.
- History schema v2 with transactional, default-off, bounded request/response snapshots and v1
  migration coverage.
- Parameterized history filters, request restoration, CLI restore/diff commands, and opt-in send
  snapshot controls.
- Deterministic bounded semantic response comparison for status, timing, size, duplicate headers,
  cookies, JSON pointers, text lines, and binary bodies.
- Native History dock with off-thread loading, refresh, latest comparison, and draft restore/resend.
- Updated architecture, build, feature, checkpoint, and phase documents.

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

Visual desktop smoke, native collection/environment mutation dialogs, polling fallback for
unreliable filesystems, Phase 2C networking/auth, remaining Phase 4 productivity, scripts/runner,
additional protocols,
OpenAPI/mocks, Git/WASM/AI, accessibility, benchmarks, and distributable packages are not claimed
complete.

## Phase 9 checkpoint

Added workspace crates: `apex-scripting`, `apex-protocols`, `apex-contracts`, `apex-mock`, `apex-git`, `apex-plugins`, `apex-ai`, `apex-quality`, and `apex-security`. Added cargo-fuzz targets, release metadata validation, cross-platform portable CLI CI, Linux package templates, migration guidance, and an explicit release-boundary document. No signed installer or unsupported native transport is claimed.
