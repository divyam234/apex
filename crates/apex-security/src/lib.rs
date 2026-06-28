#![forbid(unsafe_code)]

use flate2::read::GzDecoder;
use std::io::{self, Read};
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecompressionLimits {
    pub maximum_output_bytes: usize,
    pub maximum_ratio: usize,
}

impl Default for DecompressionLimits {
    fn default() -> Self {
        Self {
            maximum_output_bytes: 64 * 1024 * 1024,
            maximum_ratio: 100,
        }
    }
}

pub fn decompress_gzip_bounded(
    input: &[u8],
    limits: DecompressionLimits,
) -> Result<Vec<u8>, String> {
    if input.is_empty() {
        return Err("compressed payload is empty".to_owned());
    }
    if limits.maximum_output_bytes == 0 || limits.maximum_ratio == 0 {
        return Err("decompression limits must be non-zero".to_owned());
    }
    let ratio_limit = input.len().saturating_mul(limits.maximum_ratio);
    let output_limit = limits.maximum_output_bytes.min(ratio_limit);
    let mut decoder = GzDecoder::new(input);
    let mut output = Vec::with_capacity(output_limit.min(64 * 1024));
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = decoder
            .read(&mut chunk)
            .map_err(|error| format!("invalid gzip payload: {error}"))?;
        if read == 0 {
            break;
        }
        if output.len().saturating_add(read) > output_limit {
            return Err("decompressed payload exceeds configured size or ratio limit".to_owned());
        }
        output.extend_from_slice(&chunk[..read]);
    }
    Ok(output)
}

pub fn redact_diagnostic(message: &str, secrets: &[String]) -> String {
    let mut redacted = message.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        redacted = redacted.replace(secret, "[REDACTED]");
    }
    redact_assignment(&redacted)
}

pub fn validate_workspace_relative_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("path must remain inside the workspace".to_owned());
    }
    Ok(())
}

pub fn validate_certificate_bytes(bytes: &[u8], maximum_bytes: usize) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > maximum_bytes {
        return Err("certificate is empty or exceeds configured byte limit".to_owned());
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| "certificate must be valid UTF-8 PEM".to_owned())?;
    if !text.contains("-----BEGIN CERTIFICATE-----") || !text.contains("-----END CERTIFICATE-----")
    {
        return Err("certificate PEM boundaries are missing".to_owned());
    }
    if text.contains("PRIVATE KEY") {
        return Err("certificate input must not contain a private key".to_owned());
    }
    Ok(())
}

pub fn parser_fuzz_smoke(corpus: &[Vec<u8>]) -> usize {
    let mut cases = 0;
    for bytes in corpus {
        let _ = apex_contracts::OpenApiDocument::parse(bytes, 64 * 1024);
        let _ = apex_plugins::validate_plugin(
            apex_plugins::PluginManifest {
                id: "fuzz".to_owned(),
                version: "1".to_owned(),
                capabilities: Default::default(),
            },
            bytes,
            &Default::default(),
            &apex_plugins::PluginLimits::default(),
        );
        let _ = apex_scripting::ScriptRuntime.execute(
            &String::from_utf8_lossy(bytes),
            &apex_scripting::ScriptContext::default(),
            &apex_scripting::ScriptLimits {
                maximum_operations: 1_000,
                maximum_string_bytes: 64 * 1024,
                timeout: std::time::Duration::from_millis(20),
                ..apex_scripting::ScriptLimits::default()
            },
            apex_domain::CancellationToken::default(),
        );
        cases += 1;
    }
    cases
}

fn redact_assignment(message: &str) -> String {
    const KEYS: [&str; 7] = [
        "authorization",
        "token",
        "api_key",
        "apikey",
        "password",
        "secret",
        "cookie",
    ];
    let mut output = message.to_owned();
    for key in KEYS {
        for separator in ['=', ':'] {
            let lower = output.to_ascii_lowercase();
            let needle = format!("{key}{separator}");
            let mut cursor = 0;
            while let Some(relative) = lower[cursor..].find(&needle) {
                let value_start = cursor + relative + needle.len();
                let value_end = output[value_start..]
                    .find(|character: char| {
                        character.is_whitespace() || matches!(character, ',' | ';')
                    })
                    .map_or(output.len(), |offset| value_start + offset);
                output.replace_range(value_start..value_end, "[REDACTED]");
                cursor = value_start + "[REDACTED]".len();
                if cursor >= output.len() {
                    break;
                }
            }
        }
    }
    output
}

pub struct BoundedReader<R> {
    inner: R,
    remaining: usize,
}

impl<R> BoundedReader<R> {
    pub fn new(inner: R, maximum_bytes: usize) -> Self {
        Self {
            inner,
            remaining: maximum_bytes,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let allowed = buffer.len().min(self.remaining);
        let read = self.inner.read(&mut buffer[..allowed])?;
        self.remaining = self.remaining.saturating_sub(read);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(bytes).expect("write compressed fixture");
        encoder.finish().expect("finish compressed fixture")
    }

    #[test]
    fn decompression_bombs_are_rejected_by_size_and_ratio() {
        let compressed = gzip(&vec![b'a'; 1_000_000]);
        let error = decompress_gzip_bounded(
            &compressed,
            DecompressionLimits {
                maximum_output_bytes: 2_000_000,
                maximum_ratio: 10,
            },
        )
        .expect_err("high-ratio payload must fail");
        assert!(error.contains("ratio"));
    }

    #[test]
    fn diagnostics_remove_known_and_structural_secrets() {
        let secret = "top-secret-value".to_owned();
        let diagnostic = redact_diagnostic(
            "authorization:Bearer top-secret-value password=hunter2 token=abc",
            std::slice::from_ref(&secret),
        );
        assert!(!diagnostic.contains(&secret));
        assert!(!diagnostic.contains("hunter2"));
        assert!(!diagnostic.contains("token=abc"));
    }

    #[test]
    fn malicious_paths_and_certificate_private_keys_are_rejected() {
        assert!(validate_workspace_relative_path(Path::new("../secret")).is_err());
        assert!(validate_workspace_relative_path(Path::new("/etc/passwd")).is_err());
        assert!(validate_workspace_relative_path(Path::new("collections/a")).is_ok());
        let mixed = b"-----BEGIN CERTIFICATE-----\na\n-----END CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nb\n-----END PRIVATE KEY-----";
        assert!(validate_certificate_bytes(mixed, 4096).is_err());
    }

    #[test]
    fn deterministic_fuzz_smoke_never_panics() {
        let mut corpus = vec![
            Vec::new(),
            vec![0],
            vec![0xff; 256],
            b"openapi: 3.1.0\npaths: {}".to_vec(),
            b"while true {}".to_vec(),
        ];
        for seed in 0_u8..64 {
            corpus.push(
                (0..512)
                    .map(|index| seed.wrapping_mul(31).wrapping_add(index as u8))
                    .collect(),
            );
        }
        assert_eq!(parser_fuzz_smoke(&corpus), corpus.len());
    }

    #[test]
    fn bounded_reader_never_reads_past_limit() {
        let source = vec![1_u8; 1024];
        let mut reader = BoundedReader::new(source.as_slice(), 100);
        let mut output = Vec::new();
        reader.read_to_end(&mut output).expect("bounded read");
        assert_eq!(output.len(), 100);
    }
}
