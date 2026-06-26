# Packaging status

Packaging is not release-ready in Phase 1. The Nix flake provides a development/build foundation
for the offline core only. Arch, Debian, RPM, AppImage, macOS, and Windows directories contain
planned integration notes rather than pretend installers.

Final Linux packages must install the native binary, desktop entry, AppStream metadata, icons, MIME
association, shell completions, and license files. Package-manager installations defer update
handling to their package manager. Native GPUI dependencies and Wayland/X11 smoke tests are release
gates.
