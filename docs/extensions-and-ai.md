# Extensions and optional AI

The plugin boundary validates import-free WebAssembly modules, explicit extension capabilities, memory/function/input/output limits, and host panic isolation. It does not currently bundle a production WebAssembly interpreter; a host executor adapter must be supplied, and no filesystem or network imports are accepted.

AI support is a separate, disabled-by-default provider boundary. Payloads are redacted and previewed, remote endpoints require explicit approval, and a digest-bound confirmation token is required for each transmission. Core workflows have no dependency on AI.
