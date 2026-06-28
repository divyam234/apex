# Phased execution plan

Every checkpoint runs formatting, all-target checks, Clippy with warnings denied, relevant tests,
honest feature-matrix updates, architecture updates, secret scanning, and source-archive
verification.

## Phase 1 — research and foundation

Status: **implemented checkpoint**.

Competitor/GPUI audits, architecture, typed domain and execution contracts, native workspace format,
variables, secret abstraction, cURL preview, test support, and initial CLI.

## Phase 2 — HTTP core and CLI

Status: **Phase 2B implemented; Phase 2C remains**.

Delivered: Tokio/Hyper/Rustls execution, HTTP/1.1, HTTP/2 negotiation, arbitrary methods, duplicate
fields, redirects, trailers, streamed bodies/downloads, cancellation/timeouts, Basic/Bearer/API-key
auth, session cookies, bounded gzip/Brotli/zstd decoding, local SQLite history, and shared CLI send.

Remaining Phase 2C: proxy/`NO_PROXY`/SOCKS5, dedicated TLS and HTTP/2 fixtures, custom CA/mTLS,
DNS/IP controls, Digest, OAuth/OIDC, OAuth 1.0a, AWS SigV4, JWT generation, cookie inspection, and
resumable-download policy.

## Phase 3 — GPUI shell

Status: **compiled and linked foundation**.

Delivered: real native application, Root/title bar/activity bar, GPUI Dock shell, virtualized
collection tree, request and response panels, component editors, command palette, notifications,
status bar, keyboard actions, worker-to-GPUI event bridge, and shared-engine Send/Cancel.

Remaining: settings pages, theme editor, persisted dock layout, detached response windows, complete
focus/accessibility audit, and real desktop Wayland/X11 visual smoke tests.

## Phase 4 — workspace productivity

Status: **Phase 4G history snapshots, restore/resend, and semantic response diff implemented; broader productivity work remains**.

Delivered: workspace-path launch, manifest load, stable nested request index, collection-tree
navigation, dirty-open guard, field-preserving request editing, atomic saves, resource-root
propagation, tested resource-tab/session lifecycle, durable workspace/environment/local-override
variable files, shared full-request resolution, environment selection in GUI and CLI, redacted
environment inspection, strict pre-send unresolved-variable failures, recursive workspace change
observation with typed relative paths, bounded buffering, explicit rescan signals, and noise/path
filtering, plus a dedicated monitor thread, live collection-tree refresh, fingerprint verification,
clean reload prompts, dirty-document conflict preservation, rename/removal handling, visible
watcher status, stable collection/folder metadata, explicit Git-friendly ordering, guarded
create/rename/move/duplicate/archive/delete operations, duplicate identity re-keying, atomic
environment CRUD/default handling, ignored local-override lifecycle, redacted effective-source
inspection, real CLI environment administration, a bounded incremental SQLite search index under
`.apex`, sensitive-value exclusion, exact/fuzzy field filters, strict Postman Collection v2.1 preview
with unsupported-field diagnostics, redacted code generation for cURL, HTTPie, Rust reqwest, Python
requests, and Go net/http, CLI search/import/codegen workflows, privacy-governed history schema v2,
opt-in bounded request/response snapshots, filtered history queries, request restore, shared resend via
the request editor, deterministic status/timing/header/cookie/JSON/text/binary comparison, CLI history
restore/diff, and a native History dock panel with off-thread loading and latest-response comparison.

Remaining: native collection/folder mutation dialogs and drag-and-drop wiring, visual multi-document
tab strip, inheritance inspector UI, variable autocomplete/hover/go-to-definition, OS
keyring/encrypted vault, polling fallback for unreliable filesystems, additional importer/exporter
formats, and native search/import/codegen panels.

## Phase 5 — automation

Status: **planned**. QuickJS sandbox, optional Rhai, assertion engine, bounded collection runner,
reports, CLI parity, headless monitors, and Linux systemd user timer generation.

## Phase 6 — protocols

Status: **planned**. GraphQL tooling, WebSocket, SSE, and all gRPC interaction modes. HTTP/3,
Socket.IO, MQTT, raw TCP, and Unix-socket HTTP remain experimental modules.

## Phase 7 — API lifecycle

Status: **planned**. OpenAPI 3.0/3.1, Swagger 2 import, schema browser, validation, request
generation, static docs, and a loopback-safe local mock server.

## Phase 8 — collaboration and extensions

Status: **planned**. Narrow Git workflows, local workspace trust enforcement, WASM Component Model
plugins, and the optional provider-independent AI boundary.

## Phase 9 — hardening

Status: **planned**. Accessibility, benchmarks, profiling, fuzzing, security suite, packaging,
migrations, documentation, and release checklist.

## Completion record — 2026-06-26

Phases 5 through 9 have implemented and validated their shared-library and portable-CLI foundations. Packaging metadata and cross-platform CLI CI are prepared. Desktop signing, notarization, native GPUI smoke testing, assistive-technology auditing, clean-distribution package builds, and long-duration fuzzing remain release-environment gates documented in `docs/release-checklist.md`; they are not represented as completed in-product capabilities.
