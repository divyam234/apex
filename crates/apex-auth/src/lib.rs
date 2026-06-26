#![forbid(unsafe_code)]

use apex_domain::{
    ApiKeyPlacement, Authentication, ExecutionError, FormField, HeaderEntry, HttpRequest,
    ValueSensitivity,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::fmt::{Display, Formatter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthError {
    EmptyUsername,
    EmptyCredential(&'static str),
    ConflictingAuthorizationHeader,
    ConflictingApiKey {
        name: String,
        placement: ApiKeyPlacement,
    },
    InvalidHeader(String),
}

impl Display for AuthError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyUsername => formatter.write_str("Basic authentication username is empty"),
            Self::EmptyCredential(kind) => write!(formatter, "{kind} credential is empty"),
            Self::ConflictingAuthorizationHeader => formatter.write_str(
                "configured authentication conflicts with an explicit Authorization header",
            ),
            Self::ConflictingApiKey { name, placement } => write!(
                formatter,
                "configured API key {name} conflicts with an explicit {placement:?} value",
            ),
            Self::InvalidHeader(detail) => {
                write!(formatter, "invalid authentication header: {detail}")
            }
        }
    }
}

impl std::error::Error for AuthError {}

impl From<AuthError> for ExecutionError {
    fn from(value: AuthError) -> Self {
        Self::AuthenticationFailure(value.to_string())
    }
}

pub fn apply_authentication(request: &mut HttpRequest) -> Result<(), AuthError> {
    match request.authentication.clone() {
        Authentication::None => Ok(()),
        Authentication::Basic { username, password } => {
            ensure_no_authorization_header(request)?;
            if username.is_empty() {
                return Err(AuthError::EmptyUsername);
            }
            if password.is_empty() {
                return Err(AuthError::EmptyCredential("Basic password"));
            }
            let encoded = STANDARD.encode(format!("{username}:{password}"));
            append_sensitive_header(request, "Authorization", format!("Basic {encoded}"))
        }
        Authentication::Bearer { token } => {
            ensure_no_authorization_header(request)?;
            if token.is_empty() {
                return Err(AuthError::EmptyCredential("Bearer token"));
            }
            append_sensitive_header(request, "Authorization", format!("Bearer {token}"))
        }
        Authentication::ApiKey {
            name,
            value,
            placement,
        } => {
            if name.trim().is_empty() {
                return Err(AuthError::EmptyCredential("API key name"));
            }
            if value.is_empty() {
                return Err(AuthError::EmptyCredential("API key value"));
            }
            match placement {
                ApiKeyPlacement::Header => {
                    if request
                        .headers
                        .iter()
                        .any(|header| header.enabled && header.name.eq_ignore_ascii_case(&name))
                    {
                        return Err(AuthError::ConflictingApiKey { name, placement });
                    }
                    append_sensitive_header(request, name, value)
                }
                ApiKeyPlacement::Query => {
                    if request
                        .query
                        .iter()
                        .any(|field| field.enabled && field.name == name)
                    {
                        return Err(AuthError::ConflictingApiKey { name, placement });
                    }
                    request.query.push(FormField {
                        name,
                        value,
                        enabled: true,
                        sensitivity: ValueSensitivity::Secret,
                    });
                    Ok(())
                }
            }
        }
    }
}

fn ensure_no_authorization_header(request: &HttpRequest) -> Result<(), AuthError> {
    if request
        .headers
        .iter()
        .any(|header| header.enabled && header.name.eq_ignore_ascii_case("authorization"))
    {
        Err(AuthError::ConflictingAuthorizationHeader)
    } else {
        Ok(())
    }
}

fn append_sensitive_header(
    request: &mut HttpRequest,
    name: impl Into<String>,
    value: impl Into<String>,
) -> Result<(), AuthError> {
    let mut header = HeaderEntry::new(name, value)
        .map_err(|error| AuthError::InvalidHeader(error.to_string()))?;
    header.sensitivity = ValueSensitivity::Secret;
    request.headers.push(header);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{HttpMethod, RequestBody, RequestSettings, StableId};

    fn request(authentication: Authentication) -> HttpRequest {
        HttpRequest {
            id: StableId::parse("auth-test").expect("valid id"),
            name: "auth".to_owned(),
            method: HttpMethod::Get,
            url: "https://example.test".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: String::new(),
        }
    }

    #[test]
    fn basic_auth_uses_rfc_7617_header_shape() {
        let mut request = request(Authentication::Basic {
            username: "user".to_owned(),
            password: "pass".to_owned(),
        });
        apply_authentication(&mut request).expect("auth applies");
        assert_eq!(request.headers[0].value, "Basic dXNlcjpwYXNz");
        assert_eq!(request.headers[0].sensitivity, ValueSensitivity::Secret);
    }

    #[test]
    fn explicit_authorization_conflicts_instead_of_silently_overriding() {
        let mut request = request(Authentication::Bearer {
            token: "secret".to_owned(),
        });
        request
            .headers
            .push(HeaderEntry::new("Authorization", "manual").expect("header"));
        assert_eq!(
            apply_authentication(&mut request),
            Err(AuthError::ConflictingAuthorizationHeader)
        );
    }

    #[test]
    fn api_key_query_is_secret_and_ordered() {
        let mut request = request(Authentication::ApiKey {
            name: "api_key".to_owned(),
            value: "secret".to_owned(),
            placement: ApiKeyPlacement::Query,
        });
        apply_authentication(&mut request).expect("auth applies");
        assert_eq!(request.query[0].name, "api_key");
        assert_eq!(request.query[0].sensitivity, ValueSensitivity::Secret);
    }
}
