# Packaging

The repository includes maintainable metadata for Arch (`PKGBUILD`), Debian (`control` and `rules`), RPM (`.spec`), Nix (`flake.nix`), and AppImage launcher/desktop files. `scripts/validate-release-metadata.sh` checks that required files and binary identities agree with Cargo manifests.

The CI matrix builds the portable `apex` CLI on Linux, macOS, and Windows. Clean distribution-builder runs are still required before publishing each package. Desktop bundles additionally require native GPUI validation and platform signing/notarization; those external gates are tracked in `docs/release-checklist.md`.
