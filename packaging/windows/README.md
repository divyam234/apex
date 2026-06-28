# Windows packaging preparation

The portable CLI is built with `cargo build --release --locked -p apex-cli` on `windows-latest` in CI. A signed MSI is intentionally not claimed until publisher identity, signing certificates, and native GUI dependency validation are available. Release artifacts must include SHA-256 checksums and the Apache-2.0 license.
