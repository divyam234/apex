# Contributing

1. Use Rust 1.96 or newer stable and edition 2024.
2. Do not introduce GPUI dependencies below `apex-ui`/`apps/apex`.
3. Do not add a production fake response, placeholder success, empty handler, or silent importer
   drop.
4. Add typed errors and fixture-backed tests for every new format behavior.
5. Run `scripts/check.sh` before submitting changes.
6. Update the honest feature matrix when capabilities change.
7. Keep secret values out of source, fixtures, snapshots, and diagnostics.
