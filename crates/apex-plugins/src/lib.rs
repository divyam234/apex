#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use wasmparser::{Parser, Payload, Validator};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Ord, PartialOrd, Eq, PartialEq)]
pub enum Capability {
    Importer,
    Exporter,
    Generator,
    Assertion,
    Viewer,
    Authentication,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum ExtensionPoint {
    Importer,
    Exporter,
    Generator,
    Assertion,
    Viewer,
    Authentication,
}
impl ExtensionPoint {
    fn capability(self) -> Capability {
        match self {
            Self::Importer => Capability::Importer,
            Self::Exporter => Capability::Exporter,
            Self::Generator => Capability::Generator,
            Self::Assertion => Capability::Assertion,
            Self::Viewer => Capability::Viewer,
            Self::Authentication => Capability::Authentication,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub capabilities: BTreeSet<Capability>,
}
#[derive(Clone, Debug)]
pub struct PluginLimits {
    pub maximum_module_bytes: usize,
    pub maximum_input_bytes: usize,
    pub maximum_output_bytes: usize,
    pub maximum_functions: u32,
    pub maximum_memory_pages: u64,
}
impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            maximum_module_bytes: 4 * 1024 * 1024,
            maximum_input_bytes: 1024 * 1024,
            maximum_output_bytes: 1024 * 1024,
            maximum_functions: 10_000,
            maximum_memory_pages: 256,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ValidatedPlugin {
    manifest: PluginManifest,
    module: Vec<u8>,
}
impl ValidatedPlugin {
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    pub fn module_bytes(&self) -> &[u8] {
        &self.module
    }
}

pub fn validate_plugin(
    manifest: PluginManifest,
    module: &[u8],
    approved: &BTreeSet<Capability>,
    limits: &PluginLimits,
) -> Result<ValidatedPlugin, String> {
    if manifest.id.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err("plugin id and version must not be empty".into());
    }
    if module.len() > limits.maximum_module_bytes {
        return Err("plugin module exceeds configured byte limit".into());
    }
    let denied: Vec<_> = manifest
        .capabilities
        .difference(approved)
        .copied()
        .collect();
    if !denied.is_empty() {
        return Err(format!("plugin capabilities require approval: {denied:?}"));
    }
    Validator::new()
        .validate_all(module)
        .map_err(|e| format!("invalid WebAssembly module: {e}"))?;
    let mut functions = 0u32;
    for payload in Parser::new(0).parse_all(module) {
        match payload.map_err(|e| format!("invalid WebAssembly payload: {e}"))? {
            Payload::ImportSection(section) => {
                if section.count() > 0 {
                    return Err("plugin imports are denied; filesystem, process, clock, and network capabilities are unavailable".into());
                }
            }
            Payload::FunctionSection(section) => {
                functions = functions.saturating_add(section.count());
                if functions > limits.maximum_functions {
                    return Err("plugin exceeds configured function limit".into());
                }
            }
            Payload::MemorySection(section) => {
                for memory in section {
                    let memory = memory.map_err(|e| e.to_string())?;
                    if memory.initial > limits.maximum_memory_pages
                        || memory
                            .maximum
                            .is_some_and(|m| m > limits.maximum_memory_pages)
                    {
                        return Err("plugin memory exceeds configured page limit".into());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(ValidatedPlugin {
        manifest,
        module: module.to_vec(),
    })
}

pub trait PluginExecutor: Send + Sync {
    fn invoke(&self, module: &[u8], point: ExtensionPoint, input: &[u8])
    -> Result<Vec<u8>, String>;
}
impl<F> PluginExecutor for F
where
    F: Fn(&[u8], ExtensionPoint, &[u8]) -> Result<Vec<u8>, String> + Send + Sync,
{
    fn invoke(
        &self,
        module: &[u8],
        point: ExtensionPoint,
        input: &[u8],
    ) -> Result<Vec<u8>, String> {
        self(module, point, input)
    }
}

pub fn invoke_plugin(
    plugin: &ValidatedPlugin,
    point: ExtensionPoint,
    input: &[u8],
    limits: &PluginLimits,
    executor: &dyn PluginExecutor,
) -> Result<Vec<u8>, String> {
    if !plugin.manifest.capabilities.contains(&point.capability()) {
        return Err("plugin did not declare the requested extension capability".into());
    }
    if input.len() > limits.maximum_input_bytes {
        return Err("plugin input exceeds configured byte limit".into());
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        executor.invoke(&plugin.module, point, input)
    }))
    .map_err(|_| "plugin execution panicked and was isolated".to_owned())??;
    if result.len() > limits.maximum_output_bytes {
        return Err("plugin output exceeds configured byte limit".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> PluginManifest {
        PluginManifest {
            id: "echo".into(),
            version: "1".into(),
            capabilities: BTreeSet::from([Capability::Importer]),
        }
    }
    fn module() -> Vec<u8> {
        wat::parse_str("(module (func (export \"run\")))").unwrap()
    }
    #[test]
    fn import_free_module_with_approval_is_valid() {
        let p = validate_plugin(
            manifest(),
            &module(),
            &BTreeSet::from([Capability::Importer]),
            &PluginLimits::default(),
        )
        .unwrap();
        assert_eq!(p.manifest().id, "echo");
    }
    #[test]
    fn ambient_imports_and_unapproved_capabilities_are_denied() {
        let imported =
            wat::parse_str("(module (import \"wasi_snapshot_preview1\" \"fd_read\" (func)))")
                .unwrap();
        assert!(
            validate_plugin(
                manifest(),
                &imported,
                &BTreeSet::from([Capability::Importer]),
                &PluginLimits::default()
            )
            .unwrap_err()
            .contains("imports are denied")
        );
        assert!(
            validate_plugin(
                manifest(),
                &module(),
                &BTreeSet::new(),
                &PluginLimits::default()
            )
            .is_err()
        );
    }
    #[test]
    fn crashes_and_outputs_are_isolated_and_bounded() {
        let p = validate_plugin(
            manifest(),
            &module(),
            &BTreeSet::from([Capability::Importer]),
            &PluginLimits::default(),
        )
        .unwrap();
        let panic_exec =
            |_: &[u8], _: ExtensionPoint, _: &[u8]| -> Result<Vec<u8>, String> { panic!("boom") };
        assert!(
            invoke_plugin(
                &p,
                ExtensionPoint::Importer,
                b"x",
                &PluginLimits::default(),
                &panic_exec
            )
            .unwrap_err()
            .contains("isolated")
        );
        let large = |_: &[u8], _: ExtensionPoint, _: &[u8]| Ok(vec![0; 8]);
        assert!(
            invoke_plugin(
                &p,
                ExtensionPoint::Importer,
                b"x",
                &PluginLimits {
                    maximum_output_bytes: 4,
                    ..Default::default()
                },
                &large
            )
            .is_err()
        );
    }
}
