#!/bin/sh
set -eu
required='Cargo.toml Cargo.lock LICENSE packaging/arch/PKGBUILD packaging/debian/control packaging/debian/rules packaging/rpm/apex-api.spec packaging/appimage/AppRun packaging/appimage/apex-api.desktop packaging/nix/flake.nix packaging/windows/README.md packaging/macos/README.md docs/release-checklist.md docs/security-model.md docs/feature-matrix.md'
for file in $required; do
  test -s "$file" || { echo "missing or empty: $file" >&2; exit 1; }
done
grep -q '^name = "apex-cli"' apps/apex-cli/Cargo.toml
grep -q '^name = "apex"' apps/apex-cli/Cargo.toml
grep -q '^name = "apex-gui"' apps/apex/Cargo.toml
grep -q '^name = "apex"' apps/apex/Cargo.toml
grep -q '^Package: apex-api-cli' packaging/debian/control
grep -q '^Name:[[:space:]]*apex-api' packaging/rpm/apex-api.spec
grep -q '^pkgname=apex-api' packaging/arch/PKGBUILD
grep -q '^Exec=apex$' packaging/appimage/apex-api.desktop
grep -q 'usr/bin/apex"' packaging/appimage/AppRun
printf '%s\n' 'release metadata validation passed'
