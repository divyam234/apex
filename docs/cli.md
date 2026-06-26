# CLI reference

Binary name: `apex` (package: `apex-cli`). GUI dependencies are not linked into the CLI.

## Implemented commands

```text
apex doctor
apex init <directory> [name]
apex validate <workspace-directory>
apex resolve <template> [--workspace path] [--environment id] [--set name=value]...
apex env list <workspace> [--json]
apex env inspect <workspace> [environment] [--json]
apex import-curl <curl command>
apex send <request-file> [options]
apex history list <database> [--limit n] [--json]
apex history clear <database>
```

## `apex send`

```text
--environment id, -e id         select a workspace environment instead of the manifest default
--no-local-environment          ignore `.apex/environments/<id>.local.toml`
--set name=value                request-scope public variable
--secret-env name=ENV_NAME      request-scope secret variable from the process environment
--download path                 stream the decoded response to an atomic destination
--overwrite                     permit replacing an existing download target
--max-response-bytes n          override the decoded response limit
--memory-threshold n            spill larger decoded responses to a temporary file
--history-db path               use a non-default SQLite history path
--no-history                    do not create or write a history database
--json                          structured result on stdout
--quiet, -q                     suppress normal output
```

The default history path is `<workspace>/.apex/history.sqlite`. New workspaces ignore `.apex/` in
Git. The request is loaded from its workspace file; variables are resolved in URL, query, headers,
authentication, body, and resource paths; execution goes through the same `HttpAdapter` intended for
the GUI. Ctrl+C cancels the shared execution token. `--json` and `--quiet` are mutually exclusive.

JSON output includes decoded and wire byte counts, content encoding, whether decompression occurred,
response storage information, redirect chain, timings, tests, and redacted diagnostics.


## Environment commands

`apex env list <workspace>` lists stable environment IDs, names, variable counts, and the manifest
default. `--json` emits structured output.

`apex env inspect <workspace> [environment]` displays the effective source documents for workspace,
environment, and local override layers. Sensitive literal values and process-environment values are
redacted; secret entries show only their secret reference. With no explicit environment, the
manifest default is inspected.

`apex resolve` accepts the same workspace/environment selection flags as `send`, allowing resolution
traces to be inspected without network execution.

## History commands

`history list` displays newest records first and supports a bounded limit. `--json` emits structured
metadata. `history clear` removes unpinned entries only. History does not contain request or response
bodies, cookies, authorization headers, or secret values; URL query values are redacted by default.

## Exit codes

| Code | Meaning |
|---:|---|
| 0 | success |
| 2 | usage error |
| 3 | validation or variable-resolution failure |
| 4 | workspace/filesystem/history I/O failure |
| 5 | import failure |
| 6 | network/protocol execution failure |
| 130 | cancelled |

## Not implemented yet

Collection/folder `run`, general import/export, `validate-spec`, `mock start`, environment mutation
commands, JUnit reports, and completions require their owning engines. They do not return simulated success.
