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

## Implementation map

| Crate | Status | Implemented responsibility |
|---|---|---|
| apex-domain | Implemented | Requests, methods, duplicate fields, bodies, errors, events, timings, cancellation |
| apex-variables | Implemented Phase 4B | Hierarchy, durable workspace/environment loading, full-request resolution, defaults, nested access, cycles, traces, sensitivity |
| apex-secrets | Foundation | Session/env stores, redaction, leak detection, zeroed buffer |
| apex-workspace | Implemented foundation | Stable request and variable-set formats, environments/local overrides, request index, atomic saves, conflicts, path safety |
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
