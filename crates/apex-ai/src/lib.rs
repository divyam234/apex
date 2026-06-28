#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct AiConfig {
    pub enabled: bool,
    pub provider: String,
    pub endpoint: Option<String>,
    pub allow_remote: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AiRequest {
    pub task: String,
    pub payload: Value,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PayloadPreview {
    pub redacted_request: AiRequest,
    pub digest: String,
    pub confirmation_token: String,
}

pub trait AiProvider: Send + Sync {
    fn send(&self, endpoint: Option<&str>, request: &AiRequest) -> Result<Value, String>;
}
impl<F> AiProvider for F
where
    F: Fn(Option<&str>, &AiRequest) -> Result<Value, String> + Send + Sync,
{
    fn send(&self, endpoint: Option<&str>, request: &AiRequest) -> Result<Value, String> {
        self(endpoint, request)
    }
}

pub fn validate_config(config: &AiConfig) -> Result<(), String> {
    if !config.enabled {
        return Ok(());
    }
    if config.provider.trim().is_empty() {
        return Err("enabled AI configuration requires a provider".into());
    }
    if let Some(endpoint) = &config.endpoint {
        let local = endpoint.starts_with("http://127.0.0.1")
            || endpoint.starts_with("http://localhost")
            || endpoint.starts_with("http://[::1]");
        let secure = endpoint.starts_with("https://");
        if !local && !secure {
            return Err("AI endpoint must be local HTTP or HTTPS".into());
        }
        if !local && !config.allow_remote {
            return Err("remote AI endpoint requires explicit approval".into());
        }
    }
    Ok(())
}

pub fn preview(request: &AiRequest, secrets: &[String]) -> PayloadPreview {
    let mut redacted = request.clone();
    redact_value(&mut redacted.payload, secrets);
    for value in redacted.metadata.values_mut() {
        *value = redact_text(value, secrets);
    }
    let bytes = serde_json::to_vec(&redacted).unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let confirmation_token = format!("confirm:{}", &digest[..16]);
    PayloadPreview {
        redacted_request: redacted,
        digest,
        confirmation_token,
    }
}

pub fn send_confirmed(
    config: &AiConfig,
    preview: &PayloadPreview,
    confirmation: &str,
    provider: &dyn AiProvider,
) -> Result<Value, String> {
    validate_config(config)?;
    if !config.enabled {
        return Err("AI is disabled".into());
    }
    if confirmation != preview.confirmation_token {
        return Err("AI payload was not explicitly confirmed".into());
    }
    provider.send(config.endpoint.as_deref(), &preview.redacted_request)
}

fn redact_value(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = redact_text(text, secrets),
        Value::Array(values) => {
            for value in values {
                redact_value(value, secrets)
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if is_sensitive_key(key) {
                    *value = Value::String("[REDACTED]".into())
                } else {
                    redact_value(value, secrets)
                }
            }
        }
        _ => {}
    }
}
fn redact_text(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_owned();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(secret, "[REDACTED]");
    }
    out
}
fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "authorization" | "token" | "api_key" | "apikey" | "password" | "secret" | "cookie"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[test]
    fn disabled_core_never_transmits() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = {
            let calls = calls.clone();
            move |_: Option<&str>, _: &AiRequest| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(json!({}))
            }
        };
        let p = preview(
            &AiRequest {
                task: "x".into(),
                payload: json!({}),
                metadata: BTreeMap::new(),
            },
            &[],
        );
        assert!(
            send_confirmed(&AiConfig::default(), &p, &p.confirmation_token, &provider).is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn preview_redacts_and_confirmation_is_required() {
        let request = AiRequest {
            task: "review".into(),
            payload: json!({"authorization":"Bearer secret","body":"secret"}),
            metadata: BTreeMap::new(),
        };
        let p = preview(&request, &["secret".into()]);
        let text = serde_json::to_string(&p.redacted_request).unwrap();
        assert!(!text.contains("secret"));
        let config = AiConfig {
            enabled: true,
            provider: "local".into(),
            endpoint: Some("http://127.0.0.1:11434".into()),
            allow_remote: false,
        };
        let provider = |_: Option<&str>, _: &AiRequest| Ok(json!({"ok":true}));
        assert!(send_confirmed(&config, &p, "wrong", &provider).is_err());
        assert_eq!(
            send_confirmed(&config, &p, &p.confirmation_token, &provider).unwrap()["ok"],
            true
        );
    }
    #[test]
    fn remote_endpoints_require_explicit_approval() {
        let config = AiConfig {
            enabled: true,
            provider: "custom".into(),
            endpoint: Some("https://example.test".into()),
            allow_remote: false,
        };
        assert!(validate_config(&config).is_err());
    }
}
