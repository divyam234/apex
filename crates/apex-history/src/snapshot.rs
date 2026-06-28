use crate::{
    HistoryDatabase, HistoryError, HistoryPolicy, HistoryRecord, apply_retention,
    insert_history_record, read_history_record,
};
use apex_runner::{ExecutionResult, StoredBody};
use apex_secrets::SecretLeakDetector;
use apex_workspace::{RequestDocument, format_request, parse_request};
use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension as _, params, params_from_iter};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistorySnapshot {
    pub request_toml: Option<String>,
    pub request_truncated: bool,
    pub response_status: Option<u16>,
    pub response_headers: Vec<(String, String)>,
    pub response_body: Option<Vec<u8>>,
    pub response_content_type: Option<String>,
    pub response_truncated: bool,
}

impl HistorySnapshot {
    pub fn capture(
        request: Option<&RequestDocument>,
        response: Option<&ExecutionResult>,
        policy: &HistoryPolicy,
        detector: &SecretLeakDetector,
    ) -> Result<Self, HistoryError> {
        if (policy.store_request_snapshot || policy.store_response_snapshot)
            && policy.maximum_snapshot_bytes == 0
        {
            return Err(HistoryError::InvalidPolicy(
                "maximum snapshot bytes must be greater than zero when snapshots are enabled"
                    .to_owned(),
            ));
        }

        let mut snapshot = Self::default();
        if policy.store_request_snapshot
            && let Some(request) = request
        {
            let formatted = format_request(request);
            let findings = detector.scan(&formatted);
            if !findings.is_empty() {
                return Err(HistoryError::SecretLeak {
                    findings: findings.len(),
                });
            }
            if formatted.len() > policy.maximum_snapshot_bytes {
                snapshot.request_truncated = true;
            } else {
                snapshot.request_toml = Some(formatted);
            }
        }

        if policy.store_response_snapshot
            && let Some(response) = response
        {
            snapshot.response_status = response.response.status;
            snapshot.response_content_type = response.response.content_type.clone();
            snapshot.response_headers =
                redact_headers(&response.response.headers, &policy.redacted_headers);
            let (body, truncated) = read_stored_body(
                &response.response.stored_body,
                policy.maximum_snapshot_bytes,
            )?;
            snapshot.response_body = body;
            snapshot.response_truncated = truncated;
        }
        Ok(snapshot)
    }

    pub fn has_content(&self) -> bool {
        self.request_toml.is_some()
            || self.request_truncated
            || self.response_status.is_some()
            || !self.response_headers.is_empty()
            || self.response_body.is_some()
            || self.response_content_type.is_some()
            || self.response_truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    pub record: HistoryRecord,
    pub snapshot: Option<HistorySnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryQuery {
    pub limit: usize,
    pub request_id: Option<String>,
    pub method: Option<String>,
    pub environment: Option<String>,
    pub status: Option<u16>,
    pub error_category: Option<String>,
    pub pinned: Option<bool>,
    pub text: Option<String>,
    pub after_timestamp_ms: Option<i64>,
    pub before_timestamp_ms: Option<i64>,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            limit: 100,
            request_id: None,
            method: None,
            environment: None,
            status: None,
            error_category: None,
            pinned: None,
            text: None,
            after_timestamp_ms: None,
            before_timestamp_ms: None,
        }
    }
}

impl HistoryDatabase {
    pub fn insert_with_snapshot(
        &self,
        record: &HistoryRecord,
        snapshot: Option<&HistorySnapshot>,
        policy: &HistoryPolicy,
    ) -> Result<(), HistoryError> {
        if !policy.enabled {
            return Ok(());
        }
        if policy.maximum_entries == 0 {
            return Err(HistoryError::InvalidPolicy(
                "maximum history entries must be greater than zero".to_owned(),
            ));
        }
        if (policy.store_request_snapshot || policy.store_response_snapshot)
            && policy.maximum_snapshot_bytes == 0
        {
            return Err(HistoryError::InvalidPolicy(
                "maximum snapshot bytes must be greater than zero when snapshots are enabled"
                    .to_owned(),
            ));
        }
        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            insert_history_record(&transaction, record)?;
            if let Some(snapshot) = snapshot.filter(|snapshot| snapshot.has_content()) {
                let request_toml = policy
                    .store_request_snapshot
                    .then_some(snapshot.request_toml.as_deref())
                    .flatten();
                let request_truncated = policy.store_request_snapshot && snapshot.request_truncated;
                let response_status = policy
                    .store_response_snapshot
                    .then_some(snapshot.response_status)
                    .flatten();
                let response_headers_json = if policy.store_response_snapshot {
                    Some(
                        serde_json::to_string(&snapshot.response_headers)
                            .map_err(|error| HistoryError::Snapshot(error.to_string()))?,
                    )
                } else {
                    None
                };
                let response_body = policy
                    .store_response_snapshot
                    .then_some(snapshot.response_body.as_deref())
                    .flatten();
                let response_content_type = policy
                    .store_response_snapshot
                    .then_some(snapshot.response_content_type.as_deref())
                    .flatten();
                let response_truncated =
                    policy.store_response_snapshot && snapshot.response_truncated;
                transaction.execute(
                    "INSERT INTO history_snapshots (
                        execution_id, request_toml, request_truncated, response_status,
                        response_headers_json, response_body, response_content_type,
                        response_truncated
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        record.execution_id,
                        request_toml,
                        request_truncated,
                        response_status,
                        response_headers_json,
                        response_body,
                        response_content_type,
                        response_truncated,
                    ],
                )?;
            }
            apply_retention(&transaction, policy.maximum_entries)?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn get(&self, execution_id: &str) -> Result<Option<HistoryEntry>, HistoryError> {
        self.with_connection(|connection| {
            let record = connection
                .query_row(
                    "SELECT execution_id, request_id, request_name, timestamp_ms, environment,
                            method, resolved_url, status, duration_ms, response_size,
                            error_category, pinned
                     FROM history WHERE execution_id = ?1",
                    [execution_id],
                    read_history_record,
                )
                .optional()?;
            record
                .map(|record| {
                    let snapshot = read_snapshot(connection, execution_id)?;
                    Ok(HistoryEntry { record, snapshot })
                })
                .transpose()
        })
    }

    pub fn query(&self, query: &HistoryQuery) -> Result<Vec<HistoryEntry>, HistoryError> {
        if query.limit == 0 || query.limit > super::MAXIMUM_QUERY_LIMIT {
            return Err(HistoryError::InvalidPolicy(format!(
                "history query limit must be between 1 and {}",
                super::MAXIMUM_QUERY_LIMIT
            )));
        }
        if let (Some(after), Some(before)) = (query.after_timestamp_ms, query.before_timestamp_ms)
            && after > before
        {
            return Err(HistoryError::InvalidPolicy(
                "history query start time must not exceed end time".to_owned(),
            ));
        }

        self.with_connection(|connection| {
            let mut sql = String::from(
                "SELECT execution_id, request_id, request_name, timestamp_ms, environment,
                        method, resolved_url, status, duration_ms, response_size,
                        error_category, pinned
                 FROM history WHERE 1 = 1",
            );
            let mut parameters = Vec::<SqlValue>::new();
            append_optional_filter(
                &mut sql,
                &mut parameters,
                "request_id",
                query.request_id.as_deref(),
            );
            append_optional_filter(
                &mut sql,
                &mut parameters,
                "method",
                query.method.as_deref(),
            );
            append_optional_filter(
                &mut sql,
                &mut parameters,
                "environment",
                query.environment.as_deref(),
            );
            append_optional_filter(
                &mut sql,
                &mut parameters,
                "error_category",
                query.error_category.as_deref(),
            );
            if let Some(status) = query.status {
                sql.push_str(" AND status = ?");
                parameters.push(SqlValue::Integer(i64::from(status)));
            }
            if let Some(pinned) = query.pinned {
                sql.push_str(" AND pinned = ?");
                parameters.push(SqlValue::Integer(i64::from(pinned)));
            }
            if let Some(after) = query.after_timestamp_ms {
                sql.push_str(" AND timestamp_ms >= ?");
                parameters.push(SqlValue::Integer(after));
            }
            if let Some(before) = query.before_timestamp_ms {
                sql.push_str(" AND timestamp_ms <= ?");
                parameters.push(SqlValue::Integer(before));
            }
            if let Some(text) = query.text.as_deref().filter(|text| !text.trim().is_empty()) {
                sql.push_str(
                    " AND (request_name LIKE ? ESCAPE '\\' OR request_id LIKE ? ESCAPE '\\' OR resolved_url LIKE ? ESCAPE '\\')",
                );
                let pattern = SqlValue::Text(format!("%{}%", escape_like(text)));
                parameters.extend([pattern.clone(), pattern.clone(), pattern]);
            }
            sql.push_str(" ORDER BY timestamp_ms DESC, rowid DESC LIMIT ?");
            parameters.push(SqlValue::Integer(
                i64::try_from(query.limit).unwrap_or(i64::MAX),
            ));
            let mut statement = connection.prepare(&sql)?;
            let records = statement
                .query_map(params_from_iter(parameters), read_history_record)?
                .collect::<Result<Vec<_>, _>>()?;
            records
                .into_iter()
                .map(|record| {
                    let snapshot = read_snapshot(connection, &record.execution_id)?;
                    Ok(HistoryEntry { record, snapshot })
                })
                .collect()
        })
    }

    pub fn restore_request(
        &self,
        execution_id: &str,
    ) -> Result<Option<RequestDocument>, HistoryError> {
        let Some(entry) = self.get(execution_id)? else {
            return Ok(None);
        };
        let Some(snapshot) = entry.snapshot else {
            return Ok(None);
        };
        if snapshot.request_truncated {
            return Err(HistoryError::Snapshot(
                "request snapshot was too large and cannot be restored".to_owned(),
            ));
        }
        snapshot
            .request_toml
            .map(|content| {
                parse_request(&content).map_err(|error| HistoryError::Snapshot(error.to_string()))
            })
            .transpose()
    }
}

fn read_snapshot(
    connection: &rusqlite::Connection,
    execution_id: &str,
) -> Result<Option<HistorySnapshot>, HistoryError> {
    connection
        .query_row(
            "SELECT request_toml, request_truncated, response_status,
                    response_headers_json, response_body, response_content_type,
                    response_truncated
             FROM history_snapshots WHERE execution_id = ?1",
            [execution_id],
            |row| {
                let headers_json = row.get::<_, Option<String>>(3)?;
                let response_headers = headers_json
                    .as_deref()
                    .map(serde_json::from_str::<Vec<(String, String)>>)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?
                    .unwrap_or_default();
                Ok(HistorySnapshot {
                    request_toml: row.get(0)?,
                    request_truncated: row.get(1)?,
                    response_status: row.get(2)?,
                    response_headers,
                    response_body: row.get(4)?,
                    response_content_type: row.get(5)?,
                    response_truncated: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(HistoryError::from)
}

fn append_optional_filter(
    sql: &mut String,
    parameters: &mut Vec<SqlValue>,
    column: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        sql.push_str(" AND ");
        sql.push_str(column);
        sql.push_str(" = ?");
        parameters.push(SqlValue::Text(value.to_owned()));
    }
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn redact_headers(
    headers: &[(String, String)],
    redacted_headers: &[String],
) -> Vec<(String, String)> {
    let redacted = redacted_headers
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    headers
        .iter()
        .map(|(name, value)| {
            if redacted.contains(&name.to_ascii_lowercase()) {
                (name.clone(), "[REDACTED]".to_owned())
            } else {
                (name.clone(), value.clone())
            }
        })
        .collect()
}

fn read_stored_body(
    stored_body: &StoredBody,
    maximum_bytes: usize,
) -> Result<(Option<Vec<u8>>, bool), HistoryError> {
    match stored_body {
        StoredBody::Empty => Ok((Some(Vec::new()), false)),
        StoredBody::InMemory(bytes) => {
            let truncated = bytes.len() > maximum_bytes;
            Ok((
                Some(bytes[..bytes.len().min(maximum_bytes)].to_vec()),
                truncated,
            ))
        }
        StoredBody::File { path, .. } | StoredBody::StreamLog(path) => {
            read_file_prefix(path, maximum_bytes).map(|(bytes, truncated)| (Some(bytes), truncated))
        }
    }
}

fn read_file_prefix(path: &Path, maximum_bytes: usize) -> Result<(Vec<u8>, bool), HistoryError> {
    let file = File::open(path)?;
    let limit = u64::try_from(maximum_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(maximum_bytes.min(64 * 1024));
    file.take(limit).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > maximum_bytes;
    bytes.truncate(maximum_bytes);
    Ok((bytes, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{
        Authentication, ExecutionId, HttpMethod, RequestBody, RequestSettings, StableId,
    };
    use apex_runner::{ExecutionResult, ResponseMetadata};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temporary_database() -> PathBuf {
        std::env::temp_dir().join(format!(
            "apex-history-snapshot-{}-{}.sqlite",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn request_document(authentication: Authentication) -> RequestDocument {
        RequestDocument::new(apex_domain::HttpRequest {
            id: StableId::parse("history-snapshot-request").unwrap(),
            name: "Snapshot request".to_owned(),
            method: HttpMethod::Get,
            url: "https://example.test/users".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            authentication,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: "history snapshot".to_owned(),
        })
    }

    fn result(body: Vec<u8>) -> ExecutionResult {
        ExecutionResult {
            execution_id: ExecutionId::new(),
            response: ResponseMetadata {
                status: Some(200),
                status_text: Some("OK".to_owned()),
                protocol_version: "HTTP/1.1".to_owned(),
                headers: vec![
                    ("Content-Type".to_owned(), "application/json".to_owned()),
                    ("Set-Cookie".to_owned(), "session=secret".to_owned()),
                ],
                trailers: Vec::new(),
                received_bytes: body.len() as u64,
                wire_bytes: body.len() as u64,
                declared_content_length: Some(body.len() as u64),
                content_type: Some("application/json".to_owned()),
                content_encoding: None,
                decompressed: false,
                redirect_chain: Vec::new(),
                stored_body: StoredBody::InMemory(body),
            },
            timing: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn request_and_response_snapshots_restore_and_redact_headers() {
        let path = temporary_database();
        let database = HistoryDatabase::open(&path).unwrap();
        let request = request_document(Authentication::Bearer {
            token: "{{access_token}}".to_owned(),
        });
        let response = result(br#"{"id":1}"#.to_vec());
        let policy = HistoryPolicy {
            store_request_snapshot: true,
            store_response_snapshot: true,
            ..HistoryPolicy::default()
        };
        let snapshot = HistorySnapshot::capture(
            Some(&request),
            Some(&response),
            &policy,
            &SecretLeakDetector::default(),
        )
        .unwrap();
        let record = HistoryRecord::success(
            "snapshot-1",
            &request.request,
            Duration::from_millis(5),
            Some(200),
            8,
            &policy,
        );
        database
            .insert_with_snapshot(&record, Some(&snapshot), &policy)
            .unwrap();
        let restored = database.restore_request("snapshot-1").unwrap().unwrap();
        assert_eq!(restored.request.id, request.request.id);
        let entry = database.get("snapshot-1").unwrap().unwrap();
        assert_eq!(entry.snapshot.unwrap().response_headers[1].1, "[REDACTED]");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn plaintext_request_secret_is_rejected() {
        let request = request_document(Authentication::Bearer {
            token: "actual-secret-value".to_owned(),
        });
        let mut detector = SecretLeakDetector::default();
        detector.add_exact("actual-secret-value");
        let policy = HistoryPolicy {
            store_request_snapshot: true,
            ..HistoryPolicy::default()
        };
        assert!(matches!(
            HistorySnapshot::capture(Some(&request), None, &policy, &detector),
            Err(HistoryError::SecretLeak { .. })
        ));
    }

    #[test]
    fn response_snapshot_is_bounded_and_marks_truncation() {
        let policy = HistoryPolicy {
            store_response_snapshot: true,
            maximum_snapshot_bytes: 4,
            ..HistoryPolicy::default()
        };
        let snapshot = HistorySnapshot::capture(
            None,
            Some(&result(b"abcdefgh".to_vec())),
            &policy,
            &SecretLeakDetector::default(),
        )
        .unwrap();
        assert_eq!(snapshot.response_body.as_deref(), Some(b"abcd".as_slice()));
        assert!(snapshot.response_truncated);
    }

    #[test]
    fn filtered_query_loads_associated_snapshots() {
        let path = temporary_database();
        let database = HistoryDatabase::open(&path).unwrap();
        let policy = HistoryPolicy::default();
        for (id, method, status) in [
            ("one", HttpMethod::Get, 200),
            ("two", HttpMethod::Post, 201),
        ] {
            let mut request = request_document(Authentication::None).request;
            request.method = method;
            request.name = format!("Request {id}");
            let record = HistoryRecord::success(
                id,
                &request,
                Duration::from_millis(1),
                Some(status),
                0,
                &policy,
            );
            database.insert(&record, &policy).unwrap();
        }
        let results = database
            .query(&HistoryQuery {
                method: Some("POST".to_owned()),
                status: Some(201),
                text: Some("Request two".to_owned()),
                ..HistoryQuery::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.execution_id, "two");
        let _ = fs::remove_file(path);
    }
}
