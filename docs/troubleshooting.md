# Troubleshooting

## Toolchain

ApexAPI currently requires stable Rust 1.96 and edition 2024.

```bash
rustc --version
cargo --version
```

## Cargo registry mirror

The supplied build environment resolves ordinary registry packages—including GPUI and
`gpui-component`—through its configured Cargo Artifactory source. Direct GitHub cloning is not
required for the current lockfile. Keep registry credentials outside the repository.

## Linux native linker libraries

GPUI links against the normal Linux desktop stack. Install development packages that provide
unversioned linker names for `libxkbcommon` and `libxkbcommon-x11`, plus the X11/Wayland libraries
required by your distribution.

Typical Debian/Ubuntu package names include:

```text
libxkbcommon-dev
libxkbcommon-x11-dev
```

The supplied container already had `libxkbcommon.so.0` runtime libraries but not their development
linker names. A local build-only library directory was used; that directory is not part of the
source checkpoint.

## Headless launch fails with `NoSupportedDeviceFound`

GPUI uses a GPU renderer. Xvfb supplies an X server but not necessarily a supported graphics device.
A `NoSupportedDeviceFound` failure under Xvfb does not mean the Rust source or native link failed.
Run the application in a real Wayland/X11 desktop session with a working Vulkan-compatible device
for the visual smoke test.

## Open a workspace

```bash
apex --workspace /path/to/workspace
# or
apex /path/to/workspace
```

The directory must contain `apex.toml`. Invalid manifests or request files appear as explicit UI
errors. A dirty request is not silently replaced when another tree item is opened.

## Workspace validation failure

Run `apex validate <workspace>`. Merge markers, unsupported schema versions, malformed stable IDs,
out-of-root paths, oversized files, and constrained-format parse errors are reported explicitly.
ApexAPI never overwrites a file whose fingerprint changed since it was loaded.
