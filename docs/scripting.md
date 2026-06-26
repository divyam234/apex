# Scripting design

Status: **planned for Phase 5; no production scripting runtime is enabled in this checkpoint**.

ApexAPI will expose a capability-scoped scripting service shared by GUI, CLI, runners, and
monitors. The default compatibility runtime is intended to be QuickJS with optional Rhai for
small native scripts. A runtime must support wall-clock and instruction limits, memory ceilings,
cancellation, deterministic host APIs, and redacted diagnostics.

## Host surface

The planned stable surface is `apex.request`, `apex.response`, `apex.variables`,
`apex.environment`, `apex.collection`, `apex.cookies`, `apex.expect`, `apex.console`,
`apex.crypto`, and a permission-gated `apex.sendRequest`.

Scripts receive no ambient filesystem, process, native-module, or network access. Any future host
capability is deny-by-default, visible before trust is granted, and separately revocable.

## Compatibility policy

A Postman-style `pm` compatibility layer will be fixture-tested method by method. ApexAPI will not
claim complete compatibility. Unsupported APIs must fail with a typed diagnostic instead of being
silently ignored.
