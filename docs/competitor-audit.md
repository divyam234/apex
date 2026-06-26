# Competitor audit

Audit date: 2026-06-24. This document separates official capability evidence from user-reported
friction. A complaint is not treated as a universal property of a product.

## Feature comparison baseline

| Area | Postman | HTTPie Desktop | Hoppscotch | Insomnia | Bruno | ApexAPI target |
|---|---|---|---|---|---|---|
| Local use | Desktop supports local work, but product centers collaborative services | Desktop spaces, collections, environments | Personal data can be local or cloud-synced | Local collections plus sync/export workflows | Offline-first, file/Git focused | Fully functional without account or cloud |
| Git-friendly files | Collection 2.1 is normally one JSON document; newer collection work is more source-oriented | JSON export hierarchy | Import/export; not native one-request-per-file Git model | Export resources and Git-oriented workflows exist | Core product strength | One request per stable TOML file |
| CLI parity | Newman/Postman CLI cover collection workflows with format limits | HTTPie CLI differs from Desktop's full workspace model | CLI/self-host ecosystem | Inso CLI runs collection/design workflows | Bru CLI | Exact same engine for GUI and CLI |
| Realtime protocols | Broad but workflows vary by protocol | Primarily HTTP/GraphQL | WebSocket, SSE, Socket.IO, MQTT | HTTP, GraphQL, WebSocket, SSE, gRPC capabilities vary by release | HTTP/GraphQL plus expanding protocols | Stable HTTP, GraphQL, WS, SSE, all gRPC stream modes |
| Secrets | Vault/environment/team features | Variables/environments | Environment and workspace variables | Environment/private data mechanisms | Secret variables and external secret managers | References only in workspace; keyring/session/env/encrypted vault |
| Scripts/tests | Mature Postman sandbox ecosystem | Focused request workflow | Scripts/tests | Pre/post scripts and tests | Scripts/tests and Postman compatibility | Constrained QuickJS/Rhai with explicit permissions |
| OpenAPI lifecycle | Import/generate/mock/document/validate ecosystem | Import support with metadata limitations | Import and generated requests | Design documents and spec workflows | Import OpenAPI | 3.0/3.1 resolve, generate, validate, diff, docs |

## Postman

### Strengths

- Largest compatibility ecosystem among the compared tools.
- Mature collections, environments, scripting, assertions, runners, mocks, documentation, and CI.
- Collection v2.1 remains a widely exchanged format and has an official JSON Schema.
- The current documentation also describes a newer multi-file Collection 3.0 direction, but
  Newman compatibility remains tied to 2.1 at the audit date.

### Risks to avoid

- A monolithic collection file produces noisy diffs and broad merge conflicts.
- Cloud/account-centered workflows create ownership and policy concerns for local-only users.
- Script compatibility is a large surface; ApexAPI must report the tested subset instead of
  claiming blanket compatibility.

## HTTPie Desktop

### Strengths

- Clear, low-friction request editing.
- Spaces organize libraries, tabs, collections, drafts, variables, and environments.
- Import/export previews expose the hierarchy before the operation.

### Risks to avoid

- Desktop JSON import/export does not preserve every type of metadata according to its own
  compatibility table.
- Desktop and CLI concepts must not drift into two different execution semantics in ApexAPI.

## Hoppscotch

### Strengths

- Strong breadth for real-time protocols: WebSocket, SSE, Socket.IO, and MQTT.
- Personal workspaces can choose local storage or cloud synchronization.
- Imports Postman, Insomnia, and OpenAPI data.

### Risks to avoid

- A browser/PWA architecture is not suitable for ApexAPI's native performance and filesystem
  ownership goals.
- Team collaboration and self-hosting should remain optional rather than becoming a prerequisite
  for ordinary local use.

## Insomnia

### Strengths

- Request collections, scripts, design/spec documents, import/export, and Inso CLI automation.
- Exported resource identifiers are normalized for portability.

### Risks to avoid

- Collection, design, sync, and runner concepts can feel fragmented.
- Historical account/sync policy changes caused visible user distrust. ApexAPI's local mode and
  file format must remain a permanent product contract.

## Bruno

### Strengths

- Closest philosophical competitor: offline-first, Git-friendly, plain-text collections.
- Secret variables are not exported with ordinary collection data.
- Strong migration story from Postman and other clients.

### Risks to avoid

- A file-first model still needs strong large-response, streaming-protocol, schema lifecycle,
  semantic diff, and native accessibility behavior.
- ApexAPI should not copy Bruno's syntax or interface; it should interoperate through explicit,
  tested import/export adapters.

## Important cross-product user complaints

The recurring themes found in public discussions and issue trackers are: account requirements,
loss of confidence in local ownership, large Electron memory footprints, monolithic or opaque
exports, incomplete import fidelity, differences between GUI and CLI behavior, documentation gaps,
and weak Git merging. These themes drive requirements; they are not blanket assertions that every
version of every competitor always exhibits them.

## Product decisions derived from the audit

1. Local workspaces and execution never require an identity service.
2. Durable collections are ordinary files; SQLite is only an index/history/session adjunct.
3. Imports always produce a preview, warnings, unsupported-field inventory, and deterministic
   report.
4. GUI and CLI invoke the same domain and protocol crates.
5. Streaming adapters expose bounded event histories and backpressure rather than fake responses.
6. Compatibility claims are fixture-backed and version-specific.

## Primary sources

- GPUI Component repository and documentation: https://github.com/longbridge/gpui-component
- Postman collection schemas: https://learning.postman.com/docs/use/use-collections/collections-schemas/
- Postman v2.1 schema: https://schema.postman.com/json/collection/v2.1.0/docs/index.html
- HTTPie Desktop documentation: https://httpie.io/docs/desktop/httpie-desktop
- Hoppscotch documentation: https://docs.hoppscotch.io/
- Insomnia import/export reference: https://developer.konghq.com/insomnia/import-export/
- Bruno documentation: https://docs.usebruno.com/
