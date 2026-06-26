# HTTP engine — Phase 2B

## Stack and ownership

`apex-http` owns HTTP-specific execution and imports no GPUI code. It uses Tokio for asynchronous
I/O, Hyper/hyper-util for HTTP, hyper-rustls/Rustls for HTTPS, `cookie_store` for RFC-aware session
cookies, and `async-compression` for streaming response decoding. `apex-auth` owns authentication
application. `apex-runner` owns the stable adapter contract, execution context, cancellation token,
event sink, response metadata, and bounded runner policy. Both GUI and CLI call this boundary.

## Request flow

1. Validate method, URL, headers, safety limits, certificate policy, and relative resource paths.
2. Resolve and apply Basic, Bearer, or API-key authentication without exposing secret values.
3. Append enabled ordered query fields without collapsing duplicates.
4. Add matching session cookies unless the request supplied an explicit `Cookie` header.
5. Resolve the body into bounded bytes or a streaming file/multipart body.
6. Add a default User-Agent, trailer negotiation, and supported content encodings without replacing
   caller-provided values.
7. Send through a pooled client keyed by connection and idle timeout policy.
8. Store valid `Set-Cookie` values before following redirects.
9. Apply redirects explicitly so the chain and cross-origin credential policy remain visible.
10. Stream wire frames under the compressed-size limit while preserving response trailers.
11. Decode gzip, Brotli, or zstd under a separate decoded-size limit when enabled.
12. Store decoded bytes in memory, a temporary file, or an explicit partial download.
13. Commit explicit downloads only after successful completion.
14. Return structured metadata, timings, diagnostics, and the stored-body location.

## Authentication

Stable Phase 2B strategies:

- Basic authentication.
- Bearer tokens.
- API keys in ordered headers or query fields.

Credential-bearing fields must be variable references in durable request files. The workspace parser
rejects plaintext passwords, tokens, and API-key values. The adapter refuses ambiguous conflicts
with an explicitly supplied authorization header or duplicate API-key location instead of silently
overwriting caller data.

Digest, OAuth 1.0a, OAuth 2.0/OIDC, AWS SigV4, JWT generation, and mTLS remain unimplemented.

## Cookie behavior

The HTTP adapter owns an in-memory session cookie jar. Matching cookies are selected by URL and
replayed on later calls through the same adapter instance. Redirect response cookies are stored
before the next redirect request. Invalid `Set-Cookie` values become diagnostics. Requests can
opt out with `cookie_jar = false`, and an explicit `Cookie` header takes precedence.

Cookies are not persisted to the workspace or history database in this phase. GUI inspection,
per-environment jars, and selective deletion are future work.

## Response decoding and bounds

`maximum_wire_response_bytes` bounds compressed/on-wire payload bytes. `maximum_response_bytes`
bounds decoded/output bytes. gzip, `x-gzip`, Brotli, and zstd are decoded asynchronously when
`decompress_response = true`. Unknown or stacked content encodings fail with a typed decompression
error rather than being misrepresented as decoded data.

Compressed bytes are spooled to a temporary file before decoding, avoiding a second full in-memory
copy. The decoded stream uses the normal bounded response collector, including memory threshold,
spill-to-disk, explicit download, timeout, and cancellation behavior. Temporary compressed files
are removed on success and failure.

## Redirect security

ApexAPI rejects credentials embedded in URL userinfo. When a redirect changes origin, it removes
`Authorization`, `Proxy-Authorization`, and `Cookie` before sending the next request. Redirect loops
and the configured redirect limit are typed failures. Status 303 changes the next request to GET;
301/302 change POST to GET. Body-specific headers are removed when the body is removed.

## Streaming and atomicity

File and multipart file bodies are read asynchronously from canonical paths contained by the
workspace root. Bodies remain in memory only up to `memory_response_threshold`; larger bodies spill
to a temporary file. Explicit downloads use a same-directory partial file and rename only after
success, so cancellation, decompression failure, and size-limit failure cannot present partial data
as complete.

## Timing honesty

The pooled connector does not expose reliable independent DNS, TCP, TLS, and exact upload timings.
Those phases are returned as unavailable rather than estimated. Server wait, response download, and
response decompression are measured around layers ApexAPI controls.

## Tested behavior

Real loopback tests cover duplicate headers/query fields, redirects, cross-origin credential
removal, 303 rewriting, response trailers, file/multipart streaming, spill-to-disk, atomic downloads,
wire and decoded response limits, timeout, cancellation, Basic/API-key auth, session-cookie replay,
redirect cookies, cookie opt-out, gzip/Brotli/zstd decoding, decompression opt-out, and compression-
bomb rejection.

## Explicitly incomplete

- HTTP/2 has an implemented negotiation path but still needs a dedicated local fixture.
- Proxy, `NO_PROXY`, SOCKS5, DNS overrides, custom CA, mTLS, and SNI override.
- Persistent/per-environment cookie stores and cookie inspection UI.
- Resumable downloads.
- Digest, OAuth, OIDC, OAuth 1.0a, AWS SigV4, JWT generation, and inherited auth.
- Dedicated local TLS and proxy integration fixtures.

Unsupported settings are rejected or absent; they are not silently treated as successful.
