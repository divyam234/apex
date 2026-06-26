# Honest feature matrix

Audit date: 2026-06-24. Competitor cells summarize documented product direction at the audit date.
ApexAPI statuses apply only to this checkpoint.

Legend: **Implemented**, **Compiled shell**, **Foundation**, **Planned**, **Experimental planned**.

| Capability | Postman | HTTPie Desktop | Hoppscotch | Insomnia | Bruno | ApexAPI current checkpoint |
|---|---|---|---|---|---|---|
| Account-free core workflow | Local modes vary | Local spaces | Local personal workspace | Local collections | Offline-first | **Implemented** |
| Git-oriented durable files | Export/schema ecosystem | JSON hierarchy export | Import/export | Resource export/Git workflows | Plain-text collections | **Foundation**: one request per stable file |
| Native non-Web UI | No | No | No | No | No | **Compiled shell**: GPUI binary links; desktop GPU smoke pending |
| GUI/CLI same engine | Partial overlap | Related desktop/CLI | CLI/self-host tooling | Inso CLI | Bru CLI | **Implemented for HTTP Send/Cancel** |
| Dockable desktop shell | Desktop panels | Desktop panels | Web layout | Desktop panels | Desktop panels | **Implemented foundation** with GPUI Dock |
| Workspace collection tree | Supported | Supported | Supported | Supported | Supported | **Implemented foundation**, including nested file indexing |
| Multi-document tab lifecycle | Supported | Supported | Supported | Supported | Supported | **Tested model**; full visual strip pending |
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
| cURL import preview | Supported | Supported | Supported | Supported | Supported | **Implemented partial** |
| Postman v2.1 import | Native | Import | Import | Import | Import | **Planned** |
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
| Tab lifecycle | Tested model | Preview/pin/reorder/close/reopen; visual multi-tab wiring pending |
| HTTP adapter | Implemented Phase 2B | `crates/apex-http`, 23 real-network tests |
| CLI send | Implemented | Same adapter/resolver, environment selection, Ctrl+C, JSON/human/quiet, downloads, optional history |
| Workspace/environment variables | Implemented Phase 4B | Stable files, nested values, secret/process references, local override, list/inspect CLI |
| Shared request resolution | Implemented Phase 4B | GUI and CLI resolve URL/query/headers/auth/all bodies and fail before send |
| Multipart streamed files | Implemented | Durable fields, workspace containment, integration test |
| Response memory bound | Implemented | Threshold spill plus wire and decoded hard maxima |
| Automatic decompression | Implemented | gzip/Brotli/zstd, opt-out, bomb-limit tests |
| Basic/Bearer/API key | Implemented | Shared auth crate and adapter integration tests |
| History SQLite | Implemented metadata foundation | Redacted query values, bounded retention, list/clear CLI |
| Proxy/SOCKS/NO_PROXY | Planned Phase 2C | Not claimed |
| Digest/OAuth/OIDC/AWS SigV4/JWT/mTLS | Planned Phase 2C+ | Not claimed |
| Git UI/WASM plugins/AI | Planned Phase 8 | Not claimed |
