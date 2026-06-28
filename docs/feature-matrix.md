# Honest feature matrix

Competitor audit date: 2026-06-24. Competitor cells summarize documented product direction at that date.
ApexAPI checkpoint status was verified on 2026-06-26.

Legend: **Implemented**, **Compiled shell**, **Foundation**, **Planned**, **Experimental planned**.

| Capability | Postman | HTTPie Desktop | Hoppscotch | Insomnia | Bruno | ApexAPI current checkpoint |
|---|---|---|---|---|---|---|
| Account-free core workflow | Local modes vary | Local spaces | Local personal workspace | Local collections | Offline-first | **Implemented** |
| Git-oriented durable files | Export/schema ecosystem | JSON hierarchy export | Import/export | Resource export/Git workflows | Plain-text collections | **Foundation**: one request per stable file |
| Native non-Web UI | No | No | No | No | No | **Compiled shell**: GPUI binary links; desktop GPU smoke pending |
| GUI/CLI same engine | Partial overlap | Related desktop/CLI | CLI/self-host tooling | Inso CLI | Bru CLI | **Implemented for HTTP Send/Cancel** |
| Dockable desktop shell | Desktop panels | Desktop panels | Web layout | Desktop panels | Desktop panels | **Implemented foundation** with GPUI Dock |
| Workspace collection tree | Supported | Supported | Supported | Supported | Supported | **Implemented foundation**, including nested file indexing |
| Multi-document tab lifecycle | Supported | Supported | Supported | Supported | Supported | **Implemented foundation**: visual open/activate/close with dirty guards; advanced actions pending |
| Ordered duplicate query/headers | Format-dependent | Supported | Supported | Supported | Supported | **Implemented and round-trip tested** |
| Deterministic variable trace | Environments | Variables | Variables | Environments | Variables | **Implemented** across ten scopes and complete request fields |
| Durable environment selection | Supported | Supported | Supported | Supported | Supported | **Implemented foundation** in GUI/CLI with default and ignored local override |
| Secret-free workspace files | Vault/environment | Variable controls | Environment controls | Private environments | Export excludes secrets | **Foundation**, including auth-field rejection |
| HTTP/1.1 | Supported | Supported | Supported | Supported | Supported | **Implemented and loopback tested** |
| HTTP/2 | Supported | Supported | Supported | Supported | Supported | **Negotiation implemented; dedicated fixture pending** |
| HTTPS with native roots | Supported | Supported | Supported | Supported | Supported | **Implemented; dedicated local TLS fixture pending** |
| Streaming upload/download | Supported | Supported | Varies | Supported | Supported | **Implemented and tested for files/multipart/downloads** |
| Redirect transparency | Supported | Supported | Supported | Supported | Supported | **Implemented**, including chain and credential policy |
| Basic/Bearer/API key auth | Supported | Supported | Supported | Supported | Supported | **Implemented and tested** |
| Cookie session | Supported | Supported | Supported | Supported | Supported | **Implemented in-memory; inspection/persistence pending** |
| gzip/Brotli/zstd decoding | Supported | Supported | Supported | Supported | Supported | **Implemented with separate wire/decoded limits** |
| Local history | Supported | Supported | Supported | Supported | Supported | **Implemented metadata-only SQLite history and CLI** |
| WebSocket/SSE/gRPC | Broad protocol tooling | Focused HTTP/GraphQL | Strong real-time breadth | Multiple protocols | Evolving | **Planned Phase 6** |
| Scripts/assertions | Mature | Limited | Scripts/tests | Scripts/tests | Scripts/tests | **Planned Phase 5; disabled** |
| OpenAPI lifecycle | Broad | Import | Import/generation | Design/spec workflows | Import | **Planned Phase 7** |
| cURL import preview | Supported | Supported | Supported | Supported | Supported | **Implemented partial**, credential values excluded from reports |
| Postman v2.1 import | Native | Import | Import | Import | Import | **Implemented partial**: requests/body modes plus explicit loss report; scripts/auth/variables remain unsupported |
| Local mock server | Supported | Varies | Supported | Supported | Supported | **Planned; fixtures are test-only** |
| Constrained third-party plugins | Product-specific | Product-specific | Product-specific | Plugin ecosystem | Extension model | **Planned WASM model; loading disabled** |

## Checkpoint detail

| Capability | Status | Evidence / limitation |
|---|---|---|
| Native GPUI application | Compiled and linked | `apps/apex`, registry GPUI stack, visual GPU smoke pending |
| Native title bar/Dock/activity/status bars | Implemented foundation | Real GPUI components and panel entities |
| URL/body editors and request/response tabs | Implemented foundation | GPUI Component input/editor and tab bars |
| Workspace-path launch | Implemented | Positional path or `--workspace/-w` |
| Nested request indexing | Implemented | `WorkspaceRepository::list_requests`, stable ordering test |
| Workspace request open/save | Implemented foundation | Dirty guard, field preservation, atomic fingerprint save |
| Workspace filesystem observation | Implemented Phase 4D | Recursive typed events, bounded queues, live tree refresh, verified reload/conflict UX |
| Collection/folder mutations | Implemented Phase 4E core | Stable metadata/order, guarded create/rename/move/duplicate/archive/delete; native dialogs pending |
| Incremental global request search | Implemented Phase 4F core | Rebuildable bounded SQLite index, exact/fuzzy field filters, sensitive values excluded; native panel pending |
| Redacted code generation | Implemented Phase 4F foundation | cURL, HTTPie, Rust reqwest, Python requests, Go net/http; target limitations reported |
| Privacy-governed history snapshots | Implemented Phase 4G | Default-off bounded request/response snapshots, v1→v2 migration, redacted headers, filtered queries |
| History restore/resend | Implemented Phase 4G | CLI restore and native draft restore; resend uses shared request execution path |
| Semantic response diff | Implemented Phase 4G | Status, timing, size, duplicate headers, cookies, JSON, text, binary; bounded and deterministic |
| Tab lifecycle | Implemented foundation | Visual open/activate/close, dirty guards, active-resource preservation, history draft tabs; pin/reorder/reopen UI pending |
| HTTP adapter | Implemented Phase 2B | `crates/apex-http`, 23 real-network tests |
| CLI send | Implemented | Same adapter/resolver, environment selection, Ctrl+C, JSON/human/quiet, downloads, optional history |
| Workspace/environment variables | Implemented Phase 4E foundation | Atomic CRUD/default handling, local override lifecycle, source inspection, redacted CLI administration |
| Shared request resolution | Implemented Phase 4B | GUI and CLI resolve URL/query/headers/auth/all bodies and fail before send |
| Multipart streamed files | Implemented | Durable fields, workspace containment, integration test |
| Response memory bound | Implemented | Threshold spill plus wire and decoded hard maxima |
| Automatic decompression | Implemented | gzip/Brotli/zstd, opt-out, bomb-limit tests |
| Basic/Bearer/API key | Implemented | Shared auth crate and adapter integration tests |
| History SQLite | Implemented metadata foundation | Redacted query values, bounded retention, list/clear CLI |
| Proxy/SOCKS/NO_PROXY | Planned Phase 2C | Not claimed |
| Digest/OAuth/OIDC/AWS SigV4/JWT/mTLS | Planned Phase 2C+ | Not claimed |
| Git UI/WASM plugins/AI | Planned Phase 8 | Not claimed |

## Phase 5–9 completion addendum (2026-06-26)

Implemented and tested foundations now include constrained scripting, structured assertions, bounded collection execution and reports, headless monitors, GraphQL request/introspection models, bounded stream sessions, fixture-tested gRPC interaction modes, OpenAPI 3.0/3.1 parsing/generation/diff, a real loopback mock server, optional Git and workspace trust controls, import-free WebAssembly validation, disabled-by-default AI provider contracts, focus/virtualization quality primitives, and malicious-input security coverage.

The following remain adapter or release-environment boundaries rather than completed native integrations: GraphQL subscription transport, production gRPC networking/reflection transport, automatic remote OpenAPI reference fetching, mock-server TLS termination, a bundled WebAssembly interpreter, signed desktop packages, and a desktop assistive-technology audit. See `docs/release-status.md`.
