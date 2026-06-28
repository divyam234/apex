# macOS packaging preparation

The portable CLI is checked on `macos-latest` in CI. A notarized `.app`/DMG is intentionally not claimed until an Apple Developer identity, hardened-runtime entitlements, native GPUI verification, and notarization credentials are available. Release artifacts must include SHA-256 checksums and the Apache-2.0 license.
