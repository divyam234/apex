# Release checklist

## Required automated gates

- [x] `cargo fmt --all -- --check`
- [x] `cargo check --workspace --all-targets`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] Core tests excluding native GPUI link-only targets
- [x] Security regression and deterministic fuzz-smoke suite
- [x] Release metadata validation and `git diff --check`
- [x] Portable CLI release build contract for Linux, macOS, and Windows CI

## Required human/release-environment gates

- [ ] Native GPUI smoke test on a supported Linux desktop with XCB/XKB development/runtime libraries
- [ ] Assistive-technology audit with a screen reader and keyboard-only workflow
- [ ] Signed/notarized macOS application after an Apple signing identity is available
- [ ] Signed Windows installer after a publisher certificate is available
- [ ] Distribution package builds in clean Arch, Debian, RPM, Nix, and AppImage builder images
- [ ] Long-running libFuzzer campaigns and dependency audit reviewed for the release commit

A release candidate must not be described as a signed desktop release until the unchecked platform-specific gates are completed. The portable CLI and core libraries can be released independently after their automated gates pass.
