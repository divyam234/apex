# Build status — Phase 4G history snapshots and semantic response diff

Date: 2026-06-26

## Toolchain

```text
rustc 1.96.0 (ac68faa20 2026-05-25)
cargo 1.96.0 (30a34c682 2026-05-25)
clippy 0.1.96
rustfmt 1.9.0
```

The local toolchain, Cargo credentials/cache, native-library aliases, and build artifacts are not
bundled in source checkpoints.

## Native GUI result

The Phase 4B checkpoint was previously linked on x86-64 Linux with `gpui 0.2.2`,
`gpui-component 0.5.1`, and `gpui-component-assets 0.5.1`; its headless launch reached GPUI GPU
context creation before reporting `NoSupportedDeviceFound`.

The 2026-06-26 verification ran in an aarch64 Alpine container. All GUI/UI Rust targets pass
all-target checking and Clippy. Native test binaries and `apex-gui` cannot link on this host because
the development linker libraries for `xcb`, `xkbcommon`, and `xkbcommon-x11` are absent. No new
linked-binary or visual-run claim is made for this host.

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
- Recursive workspace observation through `WorkspaceRepository::watch`, with typed relative resource
  paths, create/modify/remove/rename events, and explicit rescan-required events.
- A fixed-capacity 1,024-event queue; overflow becomes a rescan signal instead of unbounded growth or
  silent success.
- Filtering for access noise, `.git`, `.apex`, editor temporaries, and traversal-shaped paths.
- A bounded background native-shell bridge that refreshes the request tree without render-thread I/O.
- Off-thread active-document fingerprint verification with self-save suppression.
- Explicit reload controls for clean disk changes and conflict controls that preserve dirty local edits.
- Rename/removal state, save blocking while disk state is unresolved, and visible watcher status.
- Stable collection/folder metadata and `.apex-order.toml` traversal without filename prefixes.
- Fingerprint-guarded create/rename/move/duplicate/archive/delete operations, symlink rejection,
  staged publication, rollback, and duplicate stable-ID re-keying.
- Atomic environment CRUD/default management, ignored local-override lifecycle, deletion rollback,
  local-override visibility, and redacted effective-variable source inspection.
- CLI `env create`, `env rename`, `env delete`, and `env default` commands.
- Bounded incremental `.apex/search.sqlite` indexing with exact/fuzzy filters, removal/update
  detection, SQL wildcard escaping, and sensitive/auth value exclusion.
- Postman Collection v2.1 import preview with duplicate/disabled headers, supported raw/form/file/
  GraphQL bodies, strict schema and resource limits, and explicit unsupported-field reports.
- Redacted code generation for cURL, HTTPie, Rust reqwest, Python requests, and Go net/http with
  target limitation warnings.
- CLI `search`, `import-postman`, and `codegen` commands using the shared implementations.
- History schema v2 migration, default-off bounded request/response snapshots, leak detection,
  configured header redaction, filtered queries, and stable request restoration.
- CLI history filters, restore, semantic diff, and opt-in send snapshot flags.
- Bounded deterministic semantic diff for status/timing/size, duplicate headers, cookies, JSON,
  text, and binary bodies.
- Native History dock with off-thread loading, snapshot indicators, latest comparison, and restore as
  an unsaved draft for shared resend.

## Validation commands

```text
cargo fmt --all -- --check                                             PASS
cargo check --workspace --all-targets                                  PASS
cargo clippy --workspace --all-targets -- -D warnings                 PASS
cargo test --workspace --exclude apex-ui --exclude apex-gui           PASS (134 tests)
cargo test --workspace                                                 HOST LINK BLOCKED
cargo build -p apex-gui                                                HOST LINK BLOCKED
```

The current source contains 147 unit/integration tests: 134 executable core tests passed on this
host, including 23 production HTTP-adapter loopback tests, 50 workspace tests, 13 history tests,
10 variable tests, 11 CLI tests, 8 export tests, and 7 import tests. The remaining 12 native UI/session
tests and one GUI launch-parser test compile during all-target checking but cannot link in this
container because of the missing native libraries above. Documentation-test targets for the core
workspace passed.

## Explicitly incomplete

Automatic replacement remains intentionally disabled: clean disk changes require explicit reload,
and dirty conflicts require explicit discard/preserve action. Merge recovery and polling fallback for
unreliable or network filesystems remain incomplete. Native collection/folder mutation dialogs and
drag-and-drop wiring, the full multi-document visual tab strip, editable parameter/header/auth grids,
inheritance inspector, variable language services, OS keyring/vault integration, additional
import/export formats, native search/import/codegen panels, proxy/SOCKS support, custom CA/mTLS,
OAuth/OIDC, scripts/assertions, collection runner,
WebSocket/SSE/gRPC, OpenAPI lifecycle, production mock server, Git UI, WASM plugins, accessibility
audit, benchmarks, and distributable packages are not claimed complete.

## Phase 9 validation scope

The release gate now checks formatting, all-target workspace compilation, Clippy with warnings denied, the full non-GPUI-linked test suite, the security suite, release metadata, portable CLI release builds in CI, and whitespace integrity. Native GPUI test binaries cannot be linked in the current aarch64 Alpine environment because XCB/XKB development libraries are absent; source check and Clippy remain part of the gate, while native runtime testing is listed as a release-environment requirement.
