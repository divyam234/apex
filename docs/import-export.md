# Import/export compatibility

The import contract always returns a preview, diagnostics, unsupported-field inventory, and a
report before workspace files are changed.

| Format | Import | Export | Checkpoint status |
|---|---:|---:|---|
| cURL command | Partial | Planned | Parser preserves method/body/ordered duplicate headers; reports unsupported options |
| Native Apex workspace | Read/write foundation | Read/write foundation | Stable constrained TOML subset and JSON Schemas |
| Postman Collection 2.0/2.1 | Planned | Planned 2.1 | No support claim |
| Postman environment | Planned | Planned where meaningful | No support claim |
| Hoppscotch | Planned | Planned where meaningful | No support claim |
| HTTPie Desktop | Planned | Planned where meaningful | No support claim |
| Insomnia | Planned | Planned where meaningful | No support claim |
| Bruno | Planned | Planned where meaningful | No support claim |
| OpenAPI 3.0/3.1 | Phase 7 | Phase 7 where meaningful | No support claim |
| Swagger 2.0 | Phase 7 | Not a primary target | No support claim |
| HAR 1.2 | Planned | Planned | No support claim |
| Raw HTTP | Planned | Planned | No support claim |
| GraphQL schema / Protobuf | Phase 6/7 | Not ordinary request export | No support claim |

Secrets are redacted by default and a leak scan blocks ordinary workspace export when known secret
values or private-key material are detected.
