# Release status

The roadmap implementation is complete at the shared-library, portable CLI, and native desktop foundation level. Verified foundations include automation, additional protocol models, OpenAPI lifecycle operations, loopback mocking, Git/trust controls, constrained scripting/plugins, optional AI boundaries, accessibility/performance primitives, and security regression coverage.

Current boundaries are intentional and must remain visible in release notes:

- GraphQL subscriptions are an experimental transport boundary.
- gRPC includes descriptor/reflection contracts and fixture-tested interaction models; production network transport integration remains adapter-owned.
- OpenAPI remote reference retrieval is not automatic; local references are root-confined.
- Mock TLS requires an external certificate/transport adapter.
- The WebAssembly layer validates and isolates modules but requires an executor adapter.
- Native GPUI linking and assistive-technology testing require a desktop release environment not present in the current Alpine/aarch64 container.
- Signed macOS and Windows packages require external signing identities.


## Native desktop completion checkpoint — 2026-06-29

The GPUI shell now exposes structured request editing, complete tested tab/session controls, workspace mutations, environment and settings administration, indexed search, import preview, redacted code generation, real bounded collection execution, protocol validation/session tools, OpenAPI generation, loopback mock control, Git operations, plugin approval/validation, and AI payload preview/confirmation.

Keyboard-only request-tab navigation uses Ctrl/Cmd+PageUp, Ctrl/Cmd+PageDown, and Ctrl/Cmd+Shift+T. Rendering remains bounded through configurable visible-tab limits, 200-entry history limits, 100-result search limits, bounded response previews, and bounded stream/gRPC logs.
