# Release checklist

## Current checkpoint gate

- [x] `cargo fmt --all -- --check`
- [x] `cargo check --workspace --all-targets`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace`
- [x] Native GPUI application compiles and links
- [x] GUI Send/Cancel calls the shared HTTP adapter off the render thread
- [x] Honest feature matrix updated
- [x] Architecture/security/file-format decisions documented
- [x] Source checkpoint excludes `target/`, local toolchains, Cargo credentials, and local state
- [ ] Visual Wayland/X11 smoke on a host with a supported GPU
- [ ] Upstream full gallery run from a source checkout that includes the story crate

## ApexAPI 1.0 gate

The complete acceptance criteria remain those in the product brief. The current shell is not a 1.0
claim. Multi-document UI, complete request editors, environments, secure OS keyring/vault,
proxy/custom-TLS controls, scripts/assertions, collection runner, streaming protocols, OpenAPI,
imports, response diff, mocks, trust restrictions, Git workflows, plugins, accessibility,
benchmarks, and Linux packaging must all be implemented and verified first.
