#![no_main]
use apex_plugins::{PluginLimits, PluginManifest, validate_plugin};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;
fuzz_target!(|data: &[u8]| {
    let _ = validate_plugin(
        PluginManifest { id: "fuzz".into(), version: "1".into(), capabilities: BTreeSet::new() },
        data,
        &BTreeSet::new(),
        &PluginLimits::default(),
    );
});
