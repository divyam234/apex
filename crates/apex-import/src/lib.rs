#![forbid(unsafe_code)]

use apex_domain::{HeaderEntry, HttpMethod, HttpRequest, RequestBody, RequestSettings, StableId};
use apex_workspace::RequestDocument;
use std::fmt::{Display, Formatter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportSeverity {
    Information,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportDiagnostic {
    pub severity: ImportSeverity,
    pub code: &'static str,
    pub message: String,
    pub source_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportPreview {
    pub source_format: &'static str,
    pub requests: Vec<RequestDocument>,
    pub diagnostics: Vec<ImportDiagnostic>,
    pub unsupported_fields: Vec<String>,
}

impl ImportPreview {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == ImportSeverity::Error)
    }
}

pub trait Importer {
    fn format_id(&self) -> &'static str;
    fn preview(&self, input: &[u8]) -> Result<ImportPreview, ImportError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CurlImporter;

impl Importer for CurlImporter {
    fn format_id(&self) -> &'static str {
        "curl"
    }

    fn preview(&self, input: &[u8]) -> Result<ImportPreview, ImportError> {
        let command = std::str::from_utf8(input).map_err(|_| ImportError::InvalidUtf8)?;
        parse_curl(command)
    }
}

pub fn parse_curl(command: &str) -> Result<ImportPreview, ImportError> {
    let tokens = shell_words(command)?;
    if tokens.is_empty() {
        return Err(ImportError::EmptyInput);
    }
    let command_index = tokens
        .iter()
        .position(|token| token == "curl")
        .ok_or(ImportError::NotCurl)?;
    let mut method: Option<HttpMethod> = None;
    let mut headers = Vec::new();
    let mut body_chunks = Vec::new();
    let mut url: Option<String> = None;
    let mut diagnostics = Vec::new();
    let mut unsupported_fields = Vec::new();
    let mut index = command_index + 1;

    while index < tokens.len() {
        let token = &tokens[index];
        match token.as_str() {
            "-X" | "--request" => {
                let value = next_value(&tokens, &mut index, token)?;
                method = Some(
                    HttpMethod::parse(value)
                        .map_err(|error| ImportError::InvalidMethod(error.to_string()))?,
                );
            }
            "-H" | "--header" => {
                let value = next_value(&tokens, &mut index, token)?;
                let Some((name, header_value)) = value.split_once(':') else {
                    return Err(ImportError::InvalidHeader(value.to_owned()));
                };
                let header = HeaderEntry::new(name.trim(), header_value.trim())
                    .map_err(|error| ImportError::InvalidHeader(error.to_string()))?;
                headers.push(header);
            }
            "-d" | "--data" | "--data-raw" | "--data-ascii" => {
                body_chunks.push(next_value(&tokens, &mut index, token)?.to_owned());
            }
            "--data-binary" => {
                let value = next_value(&tokens, &mut index, token)?;
                if let Some(path) = value.strip_prefix('@') {
                    body_chunks.push(format!("{{{{file:{path}}}}}"));
                    diagnostics.push(ImportDiagnostic {
                        severity: ImportSeverity::Warning,
                        code: "curl.file-reference",
                        message: "cURL file upload was preserved as an explicit file placeholder; review the relative path before saving.".to_owned(),
                        source_path: Some(path.to_owned()),
                    });
                } else {
                    body_chunks.push(value.to_owned());
                }
            }
            "--url" => {
                url = Some(next_value(&tokens, &mut index, token)?.to_owned());
            }
            "-u" | "--user" => {
                let value = next_value(&tokens, &mut index, token)?;
                diagnostics.push(ImportDiagnostic {
                    severity: ImportSeverity::Warning,
                    code: "curl.basic-auth",
                    message: "Basic-auth credentials were not copied into the request file. Create a secret reference during import confirmation.".to_owned(),
                    source_path: None,
                });
                unsupported_fields.push(format!("credential:{value}"));
            }
            "--compressed" | "--location" | "-L" | "--silent" | "-s" | "--show-error" => {
                diagnostics.push(ImportDiagnostic {
                    severity: ImportSeverity::Information,
                    code: "curl.behavior-option",
                    message: format!(
                        "The cURL option {token} maps to runtime settings and requires review."
                    ),
                    source_path: None,
                });
            }
            _ if token.starts_with('-') => {
                unsupported_fields.push(token.clone());
                diagnostics.push(ImportDiagnostic {
                    severity: ImportSeverity::Warning,
                    code: "curl.unsupported-option",
                    message: format!(
                        "Unsupported cURL option preserved in the import report: {token}"
                    ),
                    source_path: None,
                });
            }
            _ => {
                if url.is_none() {
                    url = Some(token.clone());
                } else {
                    diagnostics.push(ImportDiagnostic {
                        severity: ImportSeverity::Warning,
                        code: "curl.extra-argument",
                        message: format!("Extra positional argument was not interpreted: {token}"),
                        source_path: None,
                    });
                }
            }
        }
        index += 1;
    }

    let url = url.ok_or(ImportError::MissingUrl)?;
    let method = method.unwrap_or({
        if body_chunks.is_empty() {
            HttpMethod::Get
        } else {
            HttpMethod::Post
        }
    });
    let body_text = body_chunks.join("&");
    let content_type = headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.as_str());
    let body = if body_text.is_empty() {
        RequestBody::Empty
    } else if content_type.is_some_and(|value| value.contains("application/json")) {
        RequestBody::Json(body_text)
    } else if content_type.is_some_and(|value| value.contains("xml")) {
        RequestBody::Xml(body_text)
    } else {
        RequestBody::Text {
            content_type: content_type.map(str::to_owned),
            text: body_text,
        }
    };
    let request = HttpRequest {
        id: StableId::parse("imported-curl-request").expect("static identifier is valid"),
        name: "Imported cURL request".to_owned(),
        method,
        url,
        query: Vec::new(),
        headers,
        authentication: apex_domain::Authentication::None,
        body,
        settings: RequestSettings::default(),
        documentation: "Imported from cURL. Review diagnostics before saving.".to_owned(),
    };
    Ok(ImportPreview {
        source_format: "curl",
        requests: vec![RequestDocument::new(request)],
        diagnostics,
        unsupported_fields,
    })
}

fn next_value<'a>(
    tokens: &'a [String],
    index: &mut usize,
    option: &str,
) -> Result<&'a str, ImportError> {
    *index += 1;
    tokens
        .get(*index)
        .map(String::as_str)
        .ok_or_else(|| ImportError::MissingOptionValue(option.to_owned()))
}

fn shell_words(input: &str) -> Result<Vec<String>, ImportError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match (quote, character) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => current.push(character),
            (Some('"'), '\\') => escaped = true,
            (Some('"'), _) => current.push(character),
            (None, '\'') | (None, '"') => quote = Some(character),
            (None, '\\') => escaped = true,
            (None, character) if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            (None, _) => current.push(character),
            _ => unreachable!(),
        }
    }
    if escaped {
        return Err(ImportError::TrailingEscape);
    }
    if let Some(quote) = quote {
        return Err(ImportError::UnclosedQuote(quote));
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportError {
    EmptyInput,
    InvalidUtf8,
    NotCurl,
    MissingUrl,
    MissingOptionValue(String),
    InvalidHeader(String),
    InvalidMethod(String),
    TrailingEscape,
    UnclosedQuote(char),
}

impl Display for ImportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInput => formatter.write_str("import input is empty"),
            Self::InvalidUtf8 => formatter.write_str("import input is not UTF-8"),
            Self::NotCurl => formatter.write_str("input does not contain a cURL command"),
            Self::MissingUrl => formatter.write_str("cURL command has no URL"),
            Self::MissingOptionValue(option) => {
                write!(formatter, "cURL option {option} requires a value")
            }
            Self::InvalidHeader(value) => write!(formatter, "invalid cURL header: {value}"),
            Self::InvalidMethod(value) => write!(formatter, "invalid cURL method: {value}"),
            Self::TrailingEscape => {
                formatter.write_str("cURL command ends with an escape character")
            }
            Self::UnclosedQuote(quote) => {
                write!(formatter, "cURL command has an unclosed {quote} quote")
            }
        }
    }
}

impl std::error::Error for ImportError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_duplicate_headers_and_json_body() {
        let preview = parse_curl(
            "curl -X POST 'https://api.test/users' -H 'X-Trace: one' -H 'X-Trace: two' -H 'Content-Type: application/json' --data-raw '{\"name\":\"Ada\"}'",
        )
        .expect("imports");
        let request = &preview.requests[0].request;
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(
            request.header_values("x-trace").collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert!(matches!(&request.body, RequestBody::Json(value) if value.contains("Ada")));
    }

    #[test]
    fn does_not_copy_basic_auth_credentials() {
        let preview =
            parse_curl("curl -u 'admin:secret' https://api.test").expect("imports with diagnostic");
        assert_eq!(preview.unsupported_fields, ["credential:admin:secret"]);
        assert!(
            preview
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "curl.basic-auth")
        );
    }

    #[test]
    fn rejects_unclosed_quotes() {
        assert_eq!(
            parse_curl("curl 'https://api.test").expect_err("must fail"),
            ImportError::UnclosedQuote('\'')
        );
    }
}
