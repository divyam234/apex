# Plugin model

Status: **architecture only; third-party plugin loading is disabled in this checkpoint**.

ApexAPI's planned extension boundary uses the WebAssembly Component Model/WASI rather than
untrusted native dynamic libraries. Initial categories are importers, exporters, code generators,
variable generators, assertion libraries, response viewers, and authentication strategies.

Every plugin declares capabilities, resource limits, input/output schemas, and compatible ApexAPI
API versions. Filesystem and network access are absent unless a narrow user-approved capability is
both declared and granted. Executions have time and memory limits and run outside the GPUI render
path. A plugin crash or trap becomes a typed diagnostic and cannot report success.

Signed-plugin metadata may communicate publisher identity, but a signature alone is not treated as
permission or proof of safety. Users can inspect, disable, revoke permissions from, and uninstall a
plugin without changing workspace data.
