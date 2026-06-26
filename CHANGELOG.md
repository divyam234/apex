# Changelog

## Unreleased — Phase 4B environments and shared resolution — 2026-06-24

### Added

- Durable workspace, environment, and ignored local-override variable documents.
- Literal nested values, secret references, and process-environment variable sources.
- Deterministic workspace/environment/local-override loading shared by GUI and CLI.
- Full HTTP-request variable resolution for URL, duplicate query/header fields, authentication,
  every body representation, multipart metadata, and resource paths.
- Exact field-level unresolved-variable diagnostics and sensitivity traces.
- Native environment switcher plus `--environment/-e` and `--no-local-environment` CLI controls.
- `apex env list` and redacted `apex env inspect` commands.
- Variable-set JSON Schema and environment fixtures.

### Security

- Plaintext secret literals are rejected.
- Secret sources must remain marked secret.
- Environment inspection never prints sensitive, secret, or process-environment values.
- The request is not sent when strict resolution fails.

## Unreleased — Phase 2B checkpoint — 2026-06-24

### Added

- Shared Basic, Bearer, and header/query API-key authentication engine.
- Secret-reference-only durable authentication fields with plaintext credential rejection.
- RFC-aware in-memory session cookie jar, redirect-cookie replay, opt-out, and invalid-cookie diagnostics.
- Bounded gzip, Brotli, and zstd decoding with separate wire and decoded response limits.
- Decompression timing and response metadata for wire size, content encoding, and decoded state.
- Metadata-only SQLite history with redacted query values, bounded retention, pins, and clear/list operations.
- `apex history list/clear`, `apex send --history-db`, and `apex send --no-history`.
- `.apex/` ignore creation for new workspaces without silently editing existing `.gitignore` files.
- Authentication, cookie, compression, history, and workspace regression tests.

### Verification

The final checkpoint gate records formatting, check, Clippy with warnings denied, the full workspace
test suite, documentation tests, and independent CLI smoke results in `docs/build-status.md`.

## Unreleased — Phase 2A checkpoint — 2026-06-24

### Added

- Real Tokio/Hyper/hyper-util/Rustls HTTP adapter independent of GPUI.
- HTTP/1.1 and HTTP/2 negotiation path, HTTPS native roots, arbitrary methods, redirects, trailers,
  duplicate query/header handling, and typed validation.
- Streaming file/multipart uploads, bounded response bodies, temporary-file spill, and atomic direct
  downloads.
- Cancellation, total/connection/idle timeouts, redirect limits, and maximum response size.
- Cross-origin credential removal and safe redirect method rewriting.
- `apex send` with variable resolution, environment secret variables, JSON/human/quiet output,
  Ctrl+C cancellation, and deterministic execution exit codes.
- Real loopback fixture server and 15 HTTP integration tests.
- Durable ordered query, URL-encoded form, and multipart field serialization.

### Verification

Formatting, check, Clippy with warnings denied, 42 workspace tests, doc tests, all HTTP integration
tests, and an independent CLI HTTP/1.1 smoke test pass with Rust 1.96.0.

## 0.1.0-alpha.1 — Phase 1 checkpoint — 2026-06-24

### Added

- Rust 2024 workspace and shared domain/execution contracts.
- Stable IDs, custom HTTP methods, ordered duplicate headers, typed bodies/errors/timings, and
  cancellable execution state.
- Ten-scope variable resolution with defaults, nested access, cycles, traces, and sensitivity.
- Session/environment secret stores, redaction, buffer clearing, and leak detection.
- Stable workspace/request formatting, atomic writes, fingerprints, merge conflict detection,
  path safety, bounded reads, and JSON Schemas.
- cURL import preview and explicit test-only protocol fixtures.
- Foundation CLI (`doctor`, `init`, `validate`, `resolve`, `import-curl`).
- Competitor, GPUI, architecture, security, format, scripting, plugin, packaging, and phase audits.

## Native shell and Phase 4 foundation — 2026-06-24

- Replaced the obsolete Git-only GPUI probe with registry-backed GPUI packages from Artifactory.
- Added the real native `apex` workspace member and linked Linux binary.
- Added native title bar, Dock panels, virtualized collection tree, editors, request/response tabs,
  command palette, notifications, keyboard actions, and status bar.
- Connected GUI Send/Cancel to the shared HTTP adapter outside the render thread.
- Added workspace-path launch, nested request indexing, collection-tree opening, atomic field-
  preserving saves, dirty guards, and workspace resource-root propagation.
- Added a tested resource-tab/session state model.
