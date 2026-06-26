# Security model

## Trust boundaries

- Workspace files are untrusted input.
- Imported collections/specifications are untrusted input.
- Network peers, redirects, cookies, compressed streams, and response bodies are untrusted input.
- Scripts and plugins are executable content requiring an explicit trust/capability decision.
- Secret backends are privileged services behind narrow traits.
- AI providers are external disclosure boundaries and remain disabled by default.

## Secret invariants

1. Ordinary request, workspace-variable, and environment files store secret references, never
   secret values. Local overrides are ignored under `.apex/` but follow the same rule.
2. Authentication passwords, bearer tokens, and API-key values must contain a variable reference;
   plaintext durable credentials are rejected while parsing.
3. `Debug` output for secret values is always redacted.
4. In-memory secret byte buffers are overwritten on drop where Rust ownership permits it.
5. Redaction runs before logs, diagnostics, command previews, reports, and copy actions.
6. Workspace save/export scans for exact known values, credential-shaped fields, and private keys.
7. Importers never silently copy inline credentials into durable files.
8. The UI must display the source store for each resolved secret.

Phase 4B supplies session/process-environment stores plus durable secret references. A secret
source must be marked secret; plaintext secret literals and inconsistent sensitivity declarations
are rejected. OS keyring, encrypted vault, and age export remain unsupported until implemented and
integration-tested.

## Authentication controls

Basic, Bearer, and API-key authentication are applied inside the shared execution engine, not in the
GUI. Generated headers/query values carry secret sensitivity. An explicit authorization header or
same-name API-key field causes a validation conflict rather than a silent override. Authentication
values pass through the normal variable resolver and redaction pipeline.


## Variable-resolution controls

Workspace, environment, local override, and request overrides are composed by a shared library.
Resolution covers URL, duplicate query/header fields, authentication, every request body type,
content metadata, and relative file paths. Strict unresolved-variable errors identify the exact
field and abort before the network adapter is invoked. Environment inspection redacts sensitive
literals and process-environment values and displays only the identifier of a secret reference.

## Workspace trust

New workspaces default to untrusted. In untrusted mode ApexAPI will not automatically execute
scripts/plugins, launch external commands, load certificates, start mocks, or run monitors. Trust
is local machine state, not a field that another Git contributor can silently enable.

The current manifest models trust for validation experiments, but the production trust decision
will move outside the repository before format version 2. New workspaces create `.gitignore` with
`.apex/` so SQLite history and future local state do not enter ordinary Git diffs. Existing ignore
files are never modified silently.

## Filesystem protections

- Relative resource paths reject absolute paths, parent traversal, root components, and platform
  prefixes.
- Uploaded paths are canonicalized and must remain inside the workspace resource root.
- Request paths are constructed from validated slugs.
- Reads and response writes are bounded before unbounded allocation can occur.
- Workspace writes are same-directory atomic replacements with conflict fingerprints.
- Existing files are not overwritten when no expected fingerprint was supplied.
- Direct downloads use partial files and commit only after successful completion.
- Merge markers are detected before parsing.

## Network protections

Implemented through Phase 2B:

- Rustls with native trust roots for HTTPS.
- Certificate verification cannot be silently disabled; the unsupported setting is rejected.
- URL userinfo credentials are rejected in favor of explicit redacted authentication.
- Authorization, proxy authorization, and cookies are removed on cross-origin redirects.
- Redirect loops/limits, total/connection/idle timeouts, and hard response limits.
- Separate compressed-wire and decoded-response limits.
- gzip, Brotli, and zstd decoding under cancellation and decompression-bomb bounds.
- Session cookies selected by URL, with per-request opt-out and no durable cookie persistence.
- Invalid response cookies produce diagnostics rather than silent acceptance.
- HTTP trailers are advertised and collected rather than discarded.

Remaining network controls include proxy/`NO_PROXY`/SOCKS isolation, custom CA and mTLS policy, DNS
override diagnostics, persistent-cookie privacy controls, and loopback-only mock binding with a
public-bind confirmation gate.

## History privacy

History is a local SQLite metadata index, never the durable collection representation. The default
record stores request identity, timestamp, method, status/error, duration, response size, and a URL
whose query values are redacted. Request and response bodies, cookies, authorization headers, and
secret values are not stored. History can be disabled per CLI execution. Retention is bounded and
pinned rows are not removed by normal retention or clear operations.

## Plugin model

Third-party extensions target the WebAssembly Component Model/WASI. Capabilities are deny-by-
default, resource limited, time bounded, and explicitly approved. Untrusted native dynamic
libraries are not loaded.
