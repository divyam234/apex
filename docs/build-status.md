# Build status — Phase 4B environments and shared resolution

Date: 2026-06-24

## Toolchain

```text
rustc 1.96.0 (ac68faa20 2026-05-25)
cargo 1.96.0 (30a34c682 2026-05-25)
clippy 0.1.96 (ac68faa20c 2026-05-25)
rustfmt 1.9.0-stable (ac68faa20c 2026-05-25)
```

The local toolchain, Cargo credentials/cache, native-library aliases, and build artifacts are not
bundled in source checkpoints.

## Native GUI result

The real workspace member `apps/apex` resolves `gpui 0.2.2`, `gpui-component 0.5.1`, and
`gpui-component-assets 0.5.1` from the Cargo Artifactory mirror. It passes source compilation and
links to an x86-64 ELF Linux executable. `ldd` reports no missing runtime libraries.

The container has Xvfb but no GPU device supported by GPUI's Blade renderer. A headless launch
therefore stops at GPU-context creation with `NoSupportedDeviceFound`; no visual-run claim is made.
A real Wayland/X11 desktop remains the appropriate visual launch-smoke environment.

## Implemented checkpoint scope

- Native GPUI shell with title bar, Dock panels, editors, virtualized tree, tabs, command palette,
  notifications, status bar, keyboard actions, and real shared-engine Send/Cancel.
- Workspace-path launch, nested request indexing, atomic field-preserving saves, dirty/external-edit
  protection, workspace resource roots, and tested tab/session state.
- Stable `variables.toml`, environment documents, and ignored local environment overrides.
- Literal nested values, secret references, and process-environment variable sources.
- Deterministic workspace/environment/local-override loading with manifest-default selection.
- One shared GUI/CLI resolver for URL, duplicate query/header fields, authentication, all body types,
  multipart metadata, and relative file paths.
- Exact field-level resolution errors and strict failure before network execution.
- Native environment switcher and GUI startup `--environment/-e` support.
- CLI `env list`, redacted `env inspect`, environment-aware `resolve`, and environment-aware `send`.
- Existing HTTP, auth, cookie, decompression, history, secret, import, and workspace foundations.

## Validation commands

```text
cargo fmt --all -- --check                              PASS
cargo check --workspace --all-targets                   PASS
cargo clippy --workspace --all-targets -- -D warnings  PASS
cargo test --workspace                                  PASS
cargo build -p apex-gui                                 PASS
```

Result: **85 unit/integration tests passed**, including **23 production HTTP-adapter loopback tests**,
**16 workspace-format tests**, **9 variable-resolution tests**, and **12 native UI/session tests**.
All documentation-test targets passed.

An independent CLI smoke test also passed. It selected a durable environment, overrode its URL from
`.apex/environments/development.local.toml`, resolved nested workspace variables, issued a real
loopback HTTP request, and verified that environment inspection did not expose the supplied process
secret.

## Explicitly incomplete

The full multi-document visual tab strip, editable parameter/header/auth grids, collection/folder
editing, inheritance inspector, variable language services, filesystem watcher, proxy/SOCKS
support, custom CA/mTLS, OAuth/OIDC, scripts/assertions, collection runner, WebSocket/SSE/gRPC,
OpenAPI lifecycle, production mock server, Git UI, WASM plugins, accessibility audit, benchmarks,
and distributable packages are not claimed complete.
