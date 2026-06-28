#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let _ = apex_contracts::OpenApiDocument::parse(data, 64 * 1024);
});
