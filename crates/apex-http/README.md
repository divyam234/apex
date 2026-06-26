# apex-http

Production HTTP adapter foundation for ApexAPI Phase 2A.

Implemented:

- Tokio + Hyper/hyper-util execution with hyper-rustls native-root HTTPS;
- HTTP/1.1 and HTTP/2 negotiation path;
- ordered query/header fields, redirects, trailers, and typed validation;
- streamed file and multipart uploads;
- bounded in-memory responses, spill-to-file, and atomic downloads;
- total, connection, and idle timeouts plus cancellation; and
- real loopback integration tests.

See `docs/http-engine.md` and `docs/feature-matrix.md` for explicit limitations. Authentication,
cookies, proxies, automatic decompression, custom CA/mTLS, and history are not implemented here.
