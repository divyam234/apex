# Parser fuzzing

Install `cargo-fuzz`, then run bounded campaigns from the repository root:

```sh
cargo fuzz run openapi -- -max_total_time=60
cargo fuzz run wasm_plugin -- -max_total_time=60
```

The normal `apex-security` test suite also executes deterministic corpus smoke cases on every CI run, so malformed-input coverage does not depend on libFuzzer being installed.
