#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SecretRef {
    pub namespace: String,
    pub name: String,
}

impl SecretRef {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Result<Self, SecretError> {
        let namespace = namespace.into();
        let name = name.into();
        if !valid_component(&namespace) || !valid_component(&name) {
            return Err(SecretError::InvalidReference(format!("{namespace}/{name}")));
        }
        Ok(Self { namespace, name })
    }

    pub fn display_name(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> Result<&str, SecretError> {
        std::str::from_utf8(&self.0).map_err(|_| SecretError::NotUtf8)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Clone for SecretValue {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Debug for SecretValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretStoreCapability {
    ReadOnly,
    ReadWrite,
}

pub trait SecretStore: Send + Sync {
    fn store_id(&self) -> &'static str;
    fn capability(&self) -> SecretStoreCapability;
    fn get(&self, reference: &SecretRef) -> Result<Option<SecretValue>, SecretError>;
    fn put(&self, reference: &SecretRef, value: SecretValue) -> Result<(), SecretError>;
    fn delete(&self, reference: &SecretRef) -> Result<bool, SecretError>;
    fn list(&self, namespace: &str) -> Result<Vec<SecretRef>, SecretError>;
}

#[derive(Debug, Default)]
pub struct SessionSecretStore {
    values: RwLock<BTreeMap<SecretRef, SecretValue>>,
}

impl SecretStore for SessionSecretStore {
    fn store_id(&self) -> &'static str {
        "session"
    }

    fn capability(&self) -> SecretStoreCapability {
        SecretStoreCapability::ReadWrite
    }

    fn get(&self, reference: &SecretRef) -> Result<Option<SecretValue>, SecretError> {
        Ok(self
            .values
            .read()
            .map_err(|_| SecretError::StorePoisoned)?
            .get(reference)
            .cloned())
    }

    fn put(&self, reference: &SecretRef, value: SecretValue) -> Result<(), SecretError> {
        self.values
            .write()
            .map_err(|_| SecretError::StorePoisoned)?
            .insert(reference.clone(), value);
        Ok(())
    }

    fn delete(&self, reference: &SecretRef) -> Result<bool, SecretError> {
        Ok(self
            .values
            .write()
            .map_err(|_| SecretError::StorePoisoned)?
            .remove(reference)
            .is_some())
    }

    fn list(&self, namespace: &str) -> Result<Vec<SecretRef>, SecretError> {
        Ok(self
            .values
            .read()
            .map_err(|_| SecretError::StorePoisoned)?
            .keys()
            .filter(|reference| reference.namespace == namespace)
            .cloned()
            .collect())
    }
}

#[derive(Debug, Default)]
pub struct EnvironmentSecretStore;

impl EnvironmentSecretStore {
    fn environment_name(reference: &SecretRef) -> String {
        format!(
            "APEX_SECRET_{}_{}",
            normalize_environment_component(&reference.namespace),
            normalize_environment_component(&reference.name)
        )
    }
}

fn normalize_environment_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

impl SecretStore for EnvironmentSecretStore {
    fn store_id(&self) -> &'static str {
        "process-environment"
    }

    fn capability(&self) -> SecretStoreCapability {
        SecretStoreCapability::ReadOnly
    }

    fn get(&self, reference: &SecretRef) -> Result<Option<SecretValue>, SecretError> {
        match env::var(Self::environment_name(reference)) {
            Ok(value) => Ok(Some(SecretValue::new(value.into_bytes()))),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(SecretError::NotUtf8),
        }
    }

    fn put(&self, _reference: &SecretRef, _value: SecretValue) -> Result<(), SecretError> {
        Err(SecretError::ReadOnlyStore(self.store_id()))
    }

    fn delete(&self, _reference: &SecretRef) -> Result<bool, SecretError> {
        Err(SecretError::ReadOnlyStore(self.store_id()))
    }

    fn list(&self, _namespace: &str) -> Result<Vec<SecretRef>, SecretError> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
pub struct SecretStoreChain {
    stores: Vec<Arc<dyn SecretStore>>,
}

impl SecretStoreChain {
    pub fn push(&mut self, store: Arc<dyn SecretStore>) {
        self.stores.push(store);
    }

    pub fn resolve(&self, reference: &SecretRef) -> Result<ResolvedSecret, SecretError> {
        for store in &self.stores {
            if let Some(value) = store.get(reference)? {
                return Ok(ResolvedSecret {
                    value,
                    source_store: store.store_id(),
                });
            }
        }
        Err(SecretError::Missing(reference.display_name()))
    }

    pub fn first_writable(&self) -> Option<&Arc<dyn SecretStore>> {
        self.stores
            .iter()
            .find(|store| store.capability() == SecretStoreCapability::ReadWrite)
    }
}

pub struct ResolvedSecret {
    pub value: SecretValue,
    pub source_store: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct SecretRedactor {
    exact_values: BTreeSet<String>,
}

impl SecretRedactor {
    pub fn add_exact(&mut self, value: impl Into<String>) {
        let value = value.into();
        if value.len() >= 4 {
            self.exact_values.insert(value);
        }
    }

    pub fn redact(&self, input: &str) -> String {
        let mut output = input.to_owned();
        let mut values = self.exact_values.iter().collect::<Vec<_>>();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        for value in values {
            output = output.replace(value, "[REDACTED]");
        }
        redact_sensitive_header_lines(&output)
    }
}

fn redact_sensitive_header_lines(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let lowercase = line.to_ascii_lowercase();
            let sensitive = [
                "authorization:",
                "proxy-authorization:",
                "x-api-key:",
                "api-key:",
                "cookie:",
                "set-cookie:",
            ]
            .iter()
            .find_map(|prefix| lowercase.find(prefix).map(|index| (index, *prefix)));
            sensitive.map_or_else(
                || line.to_owned(),
                |(index, prefix)| {
                    let end = index + prefix.len();
                    format!("{} [REDACTED]", &line[..end])
                },
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeakFindingKind {
    ExactSecretValue,
    PlaintextCredentialField,
    PrivateKeyMaterial,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeakFinding {
    pub kind: LeakFindingKind,
    pub line: usize,
    pub key_hint: String,
}

#[derive(Clone, Debug, Default)]
pub struct SecretLeakDetector {
    exact_values: BTreeSet<String>,
}

impl SecretLeakDetector {
    pub fn add_exact(&mut self, value: impl Into<String>) {
        let value = value.into();
        if value.len() >= 4 {
            self.exact_values.insert(value);
        }
    }

    pub fn scan(&self, content: &str) -> Vec<LeakFinding> {
        let mut findings = Vec::new();
        for (index, line) in content.lines().enumerate() {
            for value in &self.exact_values {
                if line.contains(value) {
                    findings.push(LeakFinding {
                        kind: LeakFindingKind::ExactSecretValue,
                        line: index + 1,
                        key_hint: "known secret value".to_owned(),
                    });
                }
            }
            let lowercase = line.to_ascii_lowercase();
            if [
                "password",
                "secret",
                "access_token",
                "refresh_token",
                "api_key",
            ]
            .iter()
            .any(|key| lowercase.trim_start().starts_with(key))
                && line.contains('=')
                && !lowercase.contains("secret_ref")
                && !lowercase.contains("{{")
            {
                findings.push(LeakFinding {
                    kind: LeakFindingKind::PlaintextCredentialField,
                    line: index + 1,
                    key_hint: line.split('=').next().unwrap_or_default().trim().to_owned(),
                });
            }
            if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
                findings.push(LeakFinding {
                    kind: LeakFindingKind::PrivateKeyMaterial,
                    line: index + 1,
                    key_hint: "private key".to_owned(),
                });
            }
        }
        findings
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecretError {
    InvalidReference(String),
    Missing(String),
    ReadOnlyStore(&'static str),
    StorePoisoned,
    NotUtf8,
    Backend(String),
}

impl Display for SecretError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidReference(reference) => {
                write!(formatter, "invalid secret reference: {reference}")
            }
            Self::Missing(reference) => write!(formatter, "missing secret: {reference}"),
            Self::ReadOnlyStore(store) => write!(formatter, "secret store {store} is read-only"),
            Self::StorePoisoned => formatter.write_str("secret store lock is poisoned"),
            Self::NotUtf8 => formatter.write_str("secret value is not valid UTF-8"),
            Self::Backend(detail) => write!(formatter, "secret backend failed: {detail}"),
        }
    }
}

impl std::error::Error for SecretError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_never_exposes_value() {
        let value = SecretValue::new(b"correct-horse-battery-staple".to_vec());
        assert_eq!(format!("{value:?}"), "SecretValue([REDACTED])");
    }

    #[test]
    fn session_store_round_trip() {
        let store = SessionSecretStore::default();
        let reference = SecretRef::new("workspace", "token").expect("valid ref");
        store
            .put(&reference, SecretValue::new(b"secret".to_vec()))
            .expect("put succeeds");
        assert_eq!(
            store
                .get(&reference)
                .expect("get succeeds")
                .expect("present")
                .expose()
                .expect("utf8"),
            "secret"
        );
        assert!(store.delete(&reference).expect("delete succeeds"));
    }

    #[test]
    fn redacts_exact_values_and_sensitive_headers() {
        let mut redactor = SecretRedactor::default();
        redactor.add_exact("abc12345");
        let output = redactor.redact("Authorization: Bearer abc12345\nvalue=abc12345");
        assert_eq!(output, "Authorization: [REDACTED]\nvalue=[REDACTED]");
    }

    #[test]
    fn leak_detector_finds_plaintext_secret_fields() {
        let detector = SecretLeakDetector::default();
        let findings = detector.scan("name = \"demo\"\napi_key = \"plaintext\"");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, LeakFindingKind::PlaintextCredentialField);
    }
}
