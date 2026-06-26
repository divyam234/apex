# Workspace file format v1

## Goals

- One request per file.
- Stable ordering and IDs.
- Small diffs.
- Relative resource paths.
- Explicit schema versions and migrations.
- Preservation of unknown root fields where practical.
- No plaintext secrets.
- Atomic conflict-aware writes.

## Layout

```text
workspace/
├── apex.toml
├── variables.toml
├── environments/
│   ├── development.toml
│   └── staging.toml
├── .apex/
│   ├── environments/<environment>.local.toml
│   └── history.sqlite
├── collections/<collection>/
│   ├── collection.toml
│   ├── <request>.request.toml
│   ├── scripts/
│   ├── examples/
│   └── tests/
├── schemas/
├── grpc/
├── mocks/
└── profiles/
```

## Manifest example

```toml
schema_version = 1
workspace_id = "team-api"
name = "Team API"
default_environment = "development"
trust = "untrusted"
```

`trust` is transitional in Phase 1. The production trust decision must become local-only rather
than a Git-controlled permission grant before a stable release.

## Workspace and environment variables

`variables.toml` stores workspace variables. Files under `environments/` use the same format and
provide environment-scoped values. Local machine overrides live under
`.apex/environments/<id>.local.toml`; `.apex/` is ignored by Git for newly initialized workspaces.

```toml
schema_version = 1
id = "development"
name = "Development"

[[variables]]
name = "base_url"
enabled = true
sensitivity = "public"
source = "literal"
value_kind = "string"
value = "http://127.0.0.1:8080"

[[variables]]
name = "access_token"
enabled = true
sensitivity = "secret"
source = "secret"
secret_namespace = "development"
secret_name = "access-token"

[[variables]]
name = "build_id"
enabled = true
sensitivity = "sensitive"
source = "process_environment"
environment_name = "CI_BUILD_ID"
```

Literal values support null, Boolean, finite number, string, object, and array values. Objects and
arrays are serialized as stable JSON payloads. A secret source must use `sensitivity = "secret"`;
plaintext literal secrets are rejected. Process-environment references persist only the variable
name, never the process value.

The GUI and CLI load workspace, selected environment, and local override layers through the same
resolver. The manifest default is used when no environment is selected explicitly. `--set` and GUI
request values remain higher-precedence request-scope overrides.

## Request example

```toml
schema_version = 1
id = "create-user"
name = "Create user"
method = "POST"
url = "https://{{host}}/users"
timeout_ms = 30000
connection_timeout_ms = 10000
idle_timeout_ms = 30000
maximum_response_bytes = 67108864
maximum_wire_response_bytes = 67108864
redirect_limit = 10
follow_redirects = true
verify_certificates = true
cookie_jar = true
decompress_response = true

[[query]]
name = "include"
value = "profile"
enabled = true
sensitivity = "public"

[[query]]
name = "include"
value = "permissions"
enabled = true
sensitivity = "public"

[[headers]]
name = "Accept"
value = "application/json"
enabled = true
sensitivity = "public"

[[headers]]
name = "X-Trace"
value = "one"
enabled = true
sensitivity = "public"

[[headers]]
name = "X-Trace"
value = "two"
enabled = true
sensitivity = "public"

[auth]
kind = "bearer"
token = "{{access_token}}"

[body]
kind = "json"
text = "{\"name\":\"{{name}}\"}"
```

Query fields and headers are arrays, not maps, so order, duplicates, disabled entries, and
sensitivity survive round-trips.


## Authentication

Durable request files support `none`, `basic`, `bearer`, and `api_key`. Credential-bearing fields
must reference variables. Plaintext passwords, bearer tokens, and API-key values are rejected by the
parser.

```toml
[auth]
kind = "basic"
username = "service-account"
password = "{{service_password}}"
```

```toml
[auth]
kind = "api_key"
name = "X-API-Key"
value = "{{api_key}}"
placement = "header" # or "query"
```

The username in Basic authentication may be public, but its password must remain a variable
reference. Resolved values are applied only in the execution engine and carry secret sensitivity.

## Local application state

New workspaces create `.gitignore` containing `.apex/`. The default history database is
`.apex/history.sqlite`; it is local metadata, not a collection source of truth. If a workspace
already has `.gitignore`, initialization does not change it silently.

## URL-encoded forms

```toml
[body]
kind = "form_urlencoded"
encoding_version = 1

[[body.fields]]
name = "tag"
value = "one"
enabled = true
sensitivity = "public"

[[body.fields]]
name = "tag"
value = "two"
enabled = false
sensitivity = "sensitive"
```

## Multipart forms

```toml
[body]
kind = "multipart"
encoding_version = 1

[[body.fields]]
name = "metadata"
value_kind = "text"
value = "{\"type\":\"avatar\"}"
content_type = "application/json"
enabled = true
sensitivity = "public"

[[body.fields]]
name = "file"
value_kind = "file"
relative_path = "fixtures/avatar.png"
content_type = "image/png"
enabled = true
sensitivity = "sensitive"
```

File paths must be relative. The HTTP engine canonicalizes the file and the workspace resource root
and rejects any path that escapes the workspace, including traversal through symlinks.

## Variable precedence

Lowest to highest:

1. built-in dynamic
2. global
3. workspace
4. collection
5. folder
6. environment
7. local environment override
8. request
9. script-created
10. runner iteration data

Later scopes override earlier scopes. Disabled definitions never win. Every resolution returns the
examined scopes and selected source.

## Compatibility

The current parser is a constrained parser for ApexAPI's emitted TOML subset, not a claim of
complete TOML conformance. Before stable release, `toml`/`toml_edit` integration must pass golden
format tests while preserving unknown fields and stable ordering.

Machine-readable logical schemas are in `docs/schemas/`.
