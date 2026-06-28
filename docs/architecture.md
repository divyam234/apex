# Architecture

## Dependency direction

```text
apps/apex (GPUI) ─┐
                  ├─> apex-ui model/adapters ─┐
apps/apex-cli ────┘                           │
                                              v
                         apex-runner <── apex-http
                              |              |
                              v              v
 apex-domain <── apex-variables <── apex-secrets
      ^
      |
 apex-workspace
      ^
      |
 import/export/history/git/mock/plugins
```

No crate below the UI boundary may import GPUI. Persisted models never contain GPUI entities,
focus handles, pixels, colors, elements, or window references.

## Execution lifecycle

1. Load one durable request document without deserializing the full workspace.
2. Compose implemented workspace/environment/local-override layers and future inherited
   collection/folder/request configuration.
3. Resolve the complete request with a field-specific trace and collect sensitive values for
   redaction before any network activity.
4. Resolve authentication through the secret-store chain when the auth engine is enabled.
5. Run constrained scripts only when workspace trust and permissions allow it.
6. Validate through the selected protocol adapter.
7. Allocate an execution ID, cancellation token, deadline, size policy, resource root, and event sink.
8. Execute outside the GPUI render thread.
9. Stream progress and bodies through bounded sinks.
10. Return one structured result to GUI, CLI, runner, or monitor consumers.
11. Persist privacy-filtered history when history is enabled.

## HTTP adapter boundary

`apex-http` translates a resolved domain request into Hyper types. It owns connector setup,
redirects, HTTP body streaming, response frame/trailer collection, and HTTP error mapping. It does
not read workspace documents, resolve variables, store secrets, render UI, or print CLI output.

`apex-runner::ExecutionContext` provides the execution ID, cancellation token, total timeout,
maximum response size, memory threshold, optional workspace resource root, and optional download
target. `ExecutionEventSink` is thread-safe and receives progress without coupling the adapter to a
particular frontend.

## Protocol adapter rule

Streaming protocols are not represented as a single buffered HTTP response. Each adapter declares
capabilities and emits structured events. The runner owns cancellation and bounded concurrency;
adapters own protocol-specific flow control and status semantics.

## Response storage

- Small responses use `StoredBody::InMemory`.
- Responses crossing the memory threshold spill to an ApexAPI temporary file.
- Explicit downloads write to a partial sibling and rename only after successful completion.
- Hard response limits apply regardless of storage destination.
- Future streaming protocols use bounded stream logs rather than pretending to be one response.

## Durable versus ephemeral state

Durable workspace files contain requests, collections, environments, schemas, mock definitions,
runner profiles, and secret references. SQLite may contain history, indexes, schema caches, UI
session state, and opt-in telemetry. Rebuilding SQLite must never destroy the workspace.

## File concurrency

Every loaded document carries a content fingerprint. Atomic save checks the current fingerprint,
writes a same-directory temporary file, flushes it, renames it, and flushes the directory. An
external edit becomes a typed conflict; it is never overwritten silently.

`apex-workspace` also owns recursive filesystem observation. Backend events are normalized into
workspace-relative typed paths, while access noise, internal state, editor temporaries, and
traversal-shaped paths are discarded. The app-side queue is bounded to 1,024 events. Backend rescan
requests and queue overflow both become an explicit `RescanRequired` change so consumers refresh
from durable files instead of assuming an incomplete event stream is authoritative. GPUI decides how
to reconcile those changes with clean or dirty documents; the watcher itself has no UI dependency.

`apex-ui` owns a bounded background monitor bridge. It receives watcher events and rebuilds request
indexes off the render thread. The active request is compared by workspace-relative path and then
loaded on a dedicated worker for fingerprint verification. Matching fingerprints are ignored as
self-save noise. A changed clean document offers an explicit reload, while a changed dirty document
enters conflict state and blocks save until the user deliberately discards or preserves local edits.
Removal and rename states remain visible instead of silently replacing editor content.

## Collection and environment mutation boundary

`apex-workspace` owns collection, folder, request-order, environment, and local-override mutations.
Collections and folders use stable IDs in `collection.toml` and `folder.toml`; `.apex-order.toml`
controls deterministic traversal without filename prefixes. Destructive operations require bounded
content-and-structure fingerprints, reject symlinks, stage copies before publication, and preserve or
re-key identities according to operation semantics. Archive state is ordinary Git-visible metadata.

Environment IDs remain stable filenames while display names may change. Creation and updates use the
same fingerprint-checked atomic writer as requests. Default changes are manifest-guarded; deleting a
default environment is rejected until another default is selected. Deletion moves the durable file
and ignored local override into a local tombstone together, rolls back on partial failure, and reports
any cleanup path that remains. Effective-variable inspection reports precedence and source labels but
renders all sensitive and secret values as redacted text.

## Search, import, and export boundaries

The local search index is rebuildable SQLite under `.apex/search.sqlite`; workspace files remain the
only durable source of truth. Refresh compares request fingerprints, updates only changed documents,
removes deleted paths, and enforces document, byte, term, and result limits. Authentication is never
indexed. Sensitive header/form/multipart values are omitted while their field names remain searchable.
Fuzzy SQL patterns escape wildcard characters before execution.

`apex-import` parses Postman Collection v2.1 with explicit byte, item, and nesting limits. Converted
requests preserve duplicate/disabled headers and supported body modes. Scripts, authentication,
variables, examples, protocol behavior, and unknown fields become stable diagnostics and
`unsupported_fields`; credential values are never copied into reports. Schema versions other than
v2.1 are rejected rather than guessed.

`apex-export` is independent of the UI and generates redacted cURL, HTTPie, Rust reqwest, Python
requests, and Go net/http snippets. Duplicate-header limitations and incomplete multipart/file/TLS
assembly are surfaced as warnings. Revealing sensitive values requires an explicit API option; the
CLI intentionally uses redacted defaults only.

## History and semantic comparison boundary

`apex-history` schema v2 stores metadata separately from optional snapshots. Snapshot capture is
disabled by default, byte-bounded, and transactionally committed with metadata. Request snapshots use
the stable workspace request format and must pass secret-leak detection. Response snapshots redact
configured headers, truncate bodies explicitly, and preserve status/content type. Schema v1 databases
migrate in place without losing existing metadata.

Filtered history queries are parameterized and bounded. Restoring a request parses the stored stable
format; truncated snapshots cannot be restored. Semantic comparison is display-independent and covers
status, duration, response size, duplicate headers, cookies, bounded structural JSON pointers, bounded
text changes, and binary length/first-difference data. The CLI exposes list filters, restore, and diff.
The native History dock loads off the render thread, compares the two latest entries, and restores a
snapshot as an unsaved draft that uses the existing shared Send path for resend.

## Implementation map

| Crate | Status | Implemented responsibility |
|---|---|---|
| apex-domain | Implemented | Requests, methods, duplicate fields, bodies, errors, events, timings, cancellation |
| apex-variables | Implemented Phase 4B | Hierarchy, durable workspace/environment loading, full-request resolution, defaults, nested access, cycles, traces, sensitivity |
| apex-secrets | Foundation | Session/env stores, redaction, leak detection, zeroed buffer |
| apex-workspace | Implemented Phase 4C core | Stable request and variable-set formats, environments/local overrides, request index, atomic saves, conflicts, path safety, bounded recursive change observation |
| apex-runner | Implemented contracts | Adapter trait, contexts, events, stored responses, bounded concurrency |
| apex-http | Implemented Phase 2B | Real HTTP execution, auth, cookies, decompression, redirects, streaming, limits, downloads, trailers |
| apex-import | cURL partial | Preview/report model and non-secret cURL conversion |
| apex-test-support | Implemented | Explicitly test-only adapters and loopback HTTP fixture server |
| apex-cli | Implemented Phase 4B foundation | Doctor/init/validate/resolve/env inspection/import-curl/send/history with shared resolution |
| apex-ui / apps/apex | Compiled native shell | GPUI Dock UI, editors/tree/tabs, environment selection, workspace navigation, strict shared-resolution Send/Cancel |


## Native UI boundary

`apps/apex` owns process startup and optional workspace-path parsing. `apex-ui` owns GPUI entities,
rendering, focus, commands, and presentation models. It opens real workspace requests through
`WorkspaceRepository`, then invokes `apex-http` through `ProtocolAdapter` on a worker thread. A
channel carries structured progress/results back to GPUI. The render thread never performs socket,
file-upload, or decompression work.

The request editor keeps a full base `HttpRequest`; changing URL, method, or editable body text does
not reconstruct and discard query fields, headers, authentication, settings, documentation, or
structured body variants. Relative file bodies receive the active workspace root in
`ExecutionContext`.

`apex_ui::session` is a GPUI-independent tab state machine. Resource identity is separate from the
display title and dirty tabs cannot be closed through ordinary close-other/right operations without
an explicit force path.

## Environment and variable boundary

`apex-workspace` parses and atomically writes Git-friendly variable-set documents without resolving
secret values. `apex-variables` composes workspace, selected environment, and ignored local override
layers, then resolves every request field through one API used by both frontends. The HTTP adapter
accepts only the resulting resolved request. This prevents GUI/CLI precedence drift and guarantees
that unresolved placeholders fail before socket creation.

## Phase 5–9 boundaries

Automation is separated into `apex-scripting` and `apex-runner`; protocols into `apex-protocols`; API lifecycle into `apex-contracts` and `apex-mock`; collaboration and optional extensions into `apex-git`, `apex-plugins`, and `apex-ai`; hardening primitives into `apex-quality` and `apex-security`. Each crate is usable without GPUI and has focused unit or integration tests.

Adapters intentionally own external transports and credentials. The protocol models do not claim a production gRPC socket implementation, the plugin host does not claim an embedded interpreter, and mock TLS is not enabled without a certificate transport adapter. Trust, redaction, size limits, cancellation, and bounded retention are enforced before adapter calls.
