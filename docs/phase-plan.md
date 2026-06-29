# Phased execution plan

Every checkpoint runs formatting, all-target checks, Clippy with warnings denied, relevant tests, honest feature-matrix and architecture updates, secret-safety checks, and release metadata validation where applicable.

## Phase 1 — research and foundation

Status: **implemented**.

Delivered: competitor and GPUI audits, architecture, typed domain and execution contracts, native workspace formats, variable resolution, secret abstraction, cURL import preview, test support, and the initial CLI.

## Phase 2 — HTTP core and CLI

Status: **core implemented; advanced transport and authentication work remains**.

Delivered: Tokio/Hyper/Rustls execution, HTTP/1.1, HTTP/2 negotiation, arbitrary methods, ordered duplicate fields, redirects, trailers, streamed uploads and downloads, cancellation and timeouts, Basic/Bearer/API-key authentication, in-memory session cookies, bounded gzip/Brotli/zstd decoding, local SQLite history, and shared CLI execution.

Remaining: proxy, `NO_PROXY`, and SOCKS5 support; dedicated TLS and HTTP/2 fixtures; custom CA and mTLS; DNS/IP controls; Digest; OAuth/OIDC; OAuth 1.0a; AWS SigV4; JWT generation; cookie inspection and persistence; and resumable-download policy.

## Phase 3 — GPUI shell

Status: **compiled and linked implementation foundation**.

Delivered: native application process, integrated title bar and activity bar, GPUI Dock shell, virtualized collection tree, request, response, inspector, and history surfaces, component editors, command palette, notifications, status bar, keyboard actions, worker-to-GPUI event delivery, and shared-engine Send/Cancel.

Remaining: persisted dock layout, detached response windows, drag-and-drop pointer reordering, platform assistive-technology audit, and real Wayland/X11 visual smoke testing on a supported desktop host.

## Phase 4 — workspace productivity

Status: **core and shared workflows implemented through Phase 4G; native UX expansion remains**.

Delivered: workspace launch and manifest loading; stable nested request indexing; collection navigation; field-preserving request editing; atomic saves; external-change fingerprints; visual multi-document request tabs backed by the tested tab/session lifecycle; environment and local-override persistence; shared request resolution; GUI and CLI environment selection; redacted source inspection; recursive filesystem observation; live tree refresh; clean reload and dirty conflict handling; collection and folder mutations; deterministic ordering; environment CRUD; bounded SQLite search; Postman v2.1 preview with loss diagnostics; redacted code generation; history schema v2; bounded snapshots; restore, resend, filtering, and semantic response diff; CLI productivity workflows; and a native History panel.

Remaining: drag-and-drop wiring, inheritance inspector UI, variable autocomplete and navigation, OS keyring or encrypted vault integration, polling fallback for unreliable filesystems, and more importer/exporter formats. Native mutation, search, import, and code-generation surfaces are implemented.

## Phase 5 — automation

Status: **implemented foundation**.

Delivered: constrained Rhai scripting without ambient filesystem, process, or network capabilities; structured assertions; bounded sequential and parallel collection execution; iteration data; retries and backoff; cancellation; live events; deterministic JSON, JUnit, and HTML reports; exit-code policy; headless monitors; retention and notification hooks; and hardened systemd user-unit generation.

Remaining: broader compatibility coverage and release-host validation for long-running scheduled workflows. Native bounded collection execution and report inspection are implemented.

## Phase 6 — additional protocols

Status: **implemented protocol models and tested foundations**.

Delivered: GraphQL operation construction, validation, introspection models, persisted-query support, and response history models; bounded WebSocket and SSE session logs with filtering, reconnect policy, timestamps, cancellation, and dropped-event accounting; and fixture-tested gRPC unary, server-streaming, client-streaming, and bidirectional interaction models with status and trailer preservation.

Remaining: production GraphQL subscription transport, production WebSocket/SSE adapters, production gRPC networking/reflection transport, and experimental HTTP/3, Socket.IO, MQTT, raw TCP, and Unix-socket HTTP modules. Native validation, transformation, and bounded session-log tools are implemented.

## Phase 7 — API lifecycle

Status: **implemented foundation**.

Delivered: bounded OpenAPI 3.0 and 3.1 JSON/YAML parsing, actionable diagnostics, operation browsing, request generation, local root-confined references, schema checks, structural diff, Markdown documentation, and a real loopback-default mock server with bounded logging, response sequences, templating, delays, failure controls, public-bind confirmation, and shutdown.

Remaining: Swagger 2 import, automatic remote reference retrieval, deeper schema coverage, and integrated mock-server TLS termination. Native operation browsing, generation, Markdown, and loopback mock controls are implemented.

## Phase 8 — collaboration and extensions

Status: **implemented foundation**.

Delivered: optional shell-free Git discovery, status, diff, stage, commit, and branch-switch operations; dirty-tree and path safeguards; centralized trusted and untrusted workspace capability gates; constrained import-free WebAssembly module validation with explicit approvals and resource limits; typed extension points; and a disabled-by-default provider-neutral AI boundary with payload preview, redaction, endpoint controls, and one-time confirmation.

Remaining: a bundled WebAssembly executor, remote AI provider adapters, and broader extension APIs. Native Git, plugin approval/validation, and redacted AI preview/confirmation surfaces are implemented.

## Phase 9 — hardening and release

Status: **implemented automated hardening and release foundation**.

Delivered: accessibility and focus primitives, bounded virtualization, cancellable chunk processing, security regression coverage, deterministic malformed-input and fuzz-smoke cases, cargo-fuzz targets, CI quality and dependency-audit gates, cross-platform portable CLI builds, Linux packaging metadata, macOS and Windows preparation notes, migration documentation, feature and release-status documentation, release metadata validation, and a release checklist.

Remaining release-environment gates: native GPUI desktop smoke testing, platform screen-reader audit, signed and notarized macOS packages, signed Windows installers, clean Arch/Debian/RPM/Nix/AppImage builds, and long-running fuzz campaigns. Keyboard navigation and bounded-rendering audits are implemented in code.

## Completion record

The structured implementation roadmap through Phase 9 is complete at the shared-library, automated-test, documentation, CI, packaging-metadata, and portable-CLI foundation level.

This completion does not claim that every advanced transport is production-integrated, every capability has polished native GUI coverage, or every platform-specific release gate has been executed. Those remaining boundaries are tracked in [`release-status.md`](release-status.md), [`feature-matrix.md`](feature-matrix.md), and [`release-checklist.md`](release-checklist.md).
