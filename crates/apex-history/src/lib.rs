#![forbid(unsafe_code)]

mod diff;
pub use diff::*;
mod snapshot;
pub use snapshot::*;

use apex_domain::{ErrorCategory, HttpRequest};
use rusqlite::{Connection, OptionalExtension as _, params};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: i64 = 2;
const DEFAULT_MAXIMUM_ENTRIES: usize = 10_000;
const MAXIMUM_QUERY_LIMIT: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryPolicy {
    pub enabled: bool,
    pub store_resolved_url: bool,
    pub redact_all_query_values: bool,
    pub maximum_entries: usize,
    pub store_request_snapshot: bool,
    pub store_response_snapshot: bool,
    pub maximum_snapshot_bytes: usize,
    pub redacted_headers: Vec<String>,
}

impl Default for HistoryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            store_resolved_url: true,
            redact_all_query_values: true,
            maximum_entries: DEFAULT_MAXIMUM_ENTRIES,
            store_request_snapshot: false,
            store_response_snapshot: false,
            maximum_snapshot_bytes: 1024 * 1024,
            redacted_headers: vec![
                "authorization".to_owned(),
                "proxy-authorization".to_owned(),
                "cookie".to_owned(),
                "set-cookie".to_owned(),
            ],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRecord {
    pub execution_id: String,
    pub request_id: String,
    pub request_name: String,
    pub timestamp_ms: i64,
    pub environment: Option<String>,
    pub method: String,
    pub resolved_url: Option<String>,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub response_size: Option<u64>,
    pub error_category: Option<String>,
    pub pinned: bool,
}

impl HistoryRecord {
    pub fn success(
        execution_id: impl Into<String>,
        request: &HttpRequest,
        duration: Duration,
        status: Option<u16>,
        response_size: u64,
        policy: &HistoryPolicy,
    ) -> Self {
        Self::new(
            execution_id.into(),
            request,
            duration,
            status,
            Some(response_size),
            None,
            policy,
        )
    }

    pub fn failure(
        execution_id: impl Into<String>,
        request: &HttpRequest,
        duration: Duration,
        category: ErrorCategory,
        policy: &HistoryPolicy,
    ) -> Self {
        Self::new(
            execution_id.into(),
            request,
            duration,
            None,
            None,
            Some(format!("{category:?}")),
            policy,
        )
    }

    fn new(
        execution_id: String,
        request: &HttpRequest,
        duration: Duration,
        status: Option<u16>,
        response_size: Option<u64>,
        error_category: Option<String>,
        policy: &HistoryPolicy,
    ) -> Self {
        Self {
            execution_id,
            request_id: request.id.to_string(),
            request_name: request.name.clone(),
            timestamp_ms: current_timestamp_ms(),
            environment: None,
            method: request.method.to_string(),
            resolved_url: policy.store_resolved_url.then(|| {
                let resolved_url = request_url_with_query(request);
                if policy.redact_all_query_values {
                    redact_query_values(&resolved_url)
                } else {
                    resolved_url
                }
            }),
            status,
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            response_size,
            error_category,
            pinned: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HistoryDatabase {
    path: PathBuf,
}

impl HistoryDatabase {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, HistoryError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let database = Self { path };
        database.with_connection(migrate)?;
        Ok(database)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn insert(
        &self,
        record: &HistoryRecord,
        policy: &HistoryPolicy,
    ) -> Result<(), HistoryError> {
        self.insert_with_snapshot(record, None, policy)
    }

    pub fn list(&self, limit: usize) -> Result<Vec<HistoryRecord>, HistoryError> {
        if limit == 0 || limit > MAXIMUM_QUERY_LIMIT {
            return Err(HistoryError::InvalidPolicy(format!(
                "history query limit must be between 1 and {MAXIMUM_QUERY_LIMIT}"
            )));
        }
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT execution_id, request_id, request_name, timestamp_ms, environment,
                        method, resolved_url, status, duration_ms, response_size,
                        error_category, pinned
                 FROM history
                 ORDER BY timestamp_ms DESC, rowid DESC
                 LIMIT ?1",
            )?;
            let rows = statement.query_map(
                [i64::try_from(limit).unwrap_or(i64::MAX)],
                read_history_record,
            )?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(HistoryError::from)
        })
    }

    pub fn pin(&self, execution_id: &str, pinned: bool) -> Result<bool, HistoryError> {
        self.with_connection(|connection| {
            Ok(connection.execute(
                "UPDATE history SET pinned = ?2 WHERE execution_id = ?1",
                params![execution_id, pinned],
            )? > 0)
        })
    }

    pub fn clear_unpinned(&self) -> Result<usize, HistoryError> {
        self.with_connection(|connection| {
            connection
                .execute("DELETE FROM history WHERE pinned = 0", [])
                .map_err(HistoryError::from)
        })
    }

    pub fn count(&self) -> Result<usize, HistoryError> {
        self.with_connection(|connection| {
            let value: i64 =
                connection.query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))?;
            usize::try_from(value).map_err(|_| {
                HistoryError::Corruption("history row count is out of range".to_owned())
            })
        })
    }

    fn with_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, HistoryError>,
    ) -> Result<T, HistoryError> {
        let mut connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        operation(&mut connection)
    }
}

fn migrate(connection: &mut Connection) -> Result<(), HistoryError> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .optional()?
        .unwrap_or_default();
    if version > SCHEMA_VERSION {
        return Err(HistoryError::UnsupportedSchema(version));
    }
    if version == 0 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE history (
                execution_id TEXT PRIMARY KEY NOT NULL,
                request_id TEXT NOT NULL,
                request_name TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                environment TEXT,
                method TEXT NOT NULL,
                resolved_url TEXT,
                status INTEGER,
                duration_ms INTEGER NOT NULL,
                response_size INTEGER,
                error_category TEXT,
                pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1))
            );
            CREATE INDEX history_timestamp_idx ON history(timestamp_ms DESC);
            CREATE INDEX history_request_idx ON history(request_id, timestamp_ms DESC);
            CREATE TABLE history_snapshots (
                execution_id TEXT PRIMARY KEY NOT NULL,
                request_toml TEXT,
                request_truncated INTEGER NOT NULL DEFAULT 0 CHECK (request_truncated IN (0, 1)),
                response_status INTEGER,
                response_headers_json TEXT,
                response_body BLOB,
                response_content_type TEXT,
                response_truncated INTEGER NOT NULL DEFAULT 0 CHECK (response_truncated IN (0, 1)),
                FOREIGN KEY (execution_id) REFERENCES history(execution_id) ON DELETE CASCADE
            );",
        )?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
    } else if version == 1 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE history_snapshots (
                execution_id TEXT PRIMARY KEY NOT NULL,
                request_toml TEXT,
                request_truncated INTEGER NOT NULL DEFAULT 0 CHECK (request_truncated IN (0, 1)),
                response_status INTEGER,
                response_headers_json TEXT,
                response_body BLOB,
                response_content_type TEXT,
                response_truncated INTEGER NOT NULL DEFAULT 0 CHECK (response_truncated IN (0, 1)),
                FOREIGN KEY (execution_id) REFERENCES history(execution_id) ON DELETE CASCADE
            );",
        )?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
    }
    Ok(())
}

pub(crate) fn read_history_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRecord> {
    Ok(HistoryRecord {
        execution_id: row.get(0)?,
        request_id: row.get(1)?,
        request_name: row.get(2)?,
        timestamp_ms: row.get(3)?,
        environment: row.get(4)?,
        method: row.get(5)?,
        resolved_url: row.get(6)?,
        status: row.get(7)?,
        duration_ms: from_sql_u64(row.get(8)?, "duration_ms")?,
        response_size: row
            .get::<_, Option<i64>>(9)?
            .map(|value| from_sql_u64(value, "response_size"))
            .transpose()?,
        error_category: row.get(10)?,
        pinned: row.get(11)?,
    })
}

pub(crate) fn insert_history_record(
    transaction: &rusqlite::Transaction<'_>,
    record: &HistoryRecord,
) -> Result<(), HistoryError> {
    transaction.execute(
        "INSERT OR REPLACE INTO history (
            execution_id, request_id, request_name, timestamp_ms, environment,
            method, resolved_url, status, duration_ms, response_size,
            error_category, pinned
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            record.execution_id,
            record.request_id,
            record.request_name,
            record.timestamp_ms,
            record.environment,
            record.method,
            record.resolved_url,
            record.status,
            to_sql_i64(record.duration_ms)?,
            record.response_size.map(to_sql_i64).transpose()?,
            record.error_category,
            record.pinned,
        ],
    )?;
    Ok(())
}

pub(crate) fn apply_retention(
    transaction: &rusqlite::Transaction<'_>,
    maximum_entries: usize,
) -> Result<(), HistoryError> {
    let keep = i64::try_from(maximum_entries).unwrap_or(i64::MAX);
    transaction.execute(
        "DELETE FROM history
         WHERE pinned = 0 AND execution_id IN (
            SELECT execution_id FROM history
            WHERE pinned = 0
            ORDER BY timestamp_ms DESC, rowid DESC
            LIMIT -1 OFFSET ?1
         )",
        [keep],
    )?;
    Ok(())
}

fn request_url_with_query(request: &HttpRequest) -> String {
    let Ok(mut url) = url::Url::parse(&request.url) else {
        return request.url.clone();
    };
    {
        let mut query = url.query_pairs_mut();
        for field in request.query.iter().filter(|field| field.enabled) {
            query.append_pair(&field.name, &field.value);
        }
    }
    url.into()
}

fn redact_query_values(input: &str) -> String {
    let Ok(mut url) = url::Url::parse(input) else {
        return redact_unparsed_query_values(input);
    };
    let names = url
        .query_pairs()
        .map(|(name, _)| name.into_owned())
        .collect::<Vec<_>>();
    if names.is_empty() {
        return input.to_owned();
    }
    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        for name in names {
            query.append_pair(&name, "[REDACTED]");
        }
    }
    url.into()
}

fn redact_unparsed_query_values(input: &str) -> String {
    let (without_fragment, fragment) = input
        .split_once('#')
        .map_or((input, None), |(base, fragment)| (base, Some(fragment)));
    let Some((base, query)) = without_fragment.split_once('?') else {
        return input.to_owned();
    };
    let query = query
        .split('&')
        .map(|pair| {
            pair.split_once('=')
                .map_or_else(|| pair.to_owned(), |(name, _)| format!("{name}=[REDACTED]"))
        })
        .collect::<Vec<_>>()
        .join("&");
    match fragment {
        Some(fragment) => format!("{base}?{query}#{fragment}"),
        None => format!("{base}?{query}"),
    }
}

fn current_timestamp_ms() -> i64 {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_sql_i64(value: u64) -> Result<i64, HistoryError> {
    i64::try_from(value).map_err(|_| {
        HistoryError::InvalidPolicy("numeric history value exceeds SQLite range".to_owned())
    })
}

fn from_sql_u64(value: i64, field: &str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(HistoryConversionError(field.to_owned())),
        )
    })
}

#[derive(Debug)]
struct HistoryConversionError(String);

impl Display for HistoryConversionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "history field {} is negative", self.0)
    }
}

impl std::error::Error for HistoryConversionError {}

#[derive(Debug)]
pub enum HistoryError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    InvalidPolicy(String),
    UnsupportedSchema(i64),
    Corruption(String),
    Snapshot(String),
    SecretLeak { findings: usize },
}

impl Display for HistoryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "history I/O error: {error}"),
            Self::Sqlite(error) => write!(formatter, "history database error: {error}"),
            Self::InvalidPolicy(detail) => write!(formatter, "invalid history policy: {detail}"),
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "history schema version {version} is newer than supported"
                )
            }
            Self::Corruption(detail) => write!(formatter, "history database is corrupt: {detail}"),
            Self::Snapshot(detail) => write!(formatter, "history snapshot failed: {detail}"),
            Self::SecretLeak { findings } => write!(
                formatter,
                "history request snapshot was rejected after {findings} potential secret finding(s)"
            ),
        }
    }
}

impl std::error::Error for HistoryError {}

impl From<std::io::Error> for HistoryError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<rusqlite::Error> for HistoryError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_domain::{
        Authentication, ExecutionId, HttpMethod, RequestBody, RequestSettings, StableId,
    };

    fn request() -> HttpRequest {
        HttpRequest {
            id: StableId::parse("history-request").expect("id"),
            name: "History request".to_owned(),
            method: HttpMethod::Get,
            url: "https://example.test/users?token=secret&tag=one".to_owned(),
            query: vec![apex_domain::FormField {
                name: "filter".to_owned(),
                value: "active users".to_owned(),
                enabled: true,
                sensitivity: apex_domain::ValueSensitivity::Sensitive,
            }],
            headers: Vec::new(),
            authentication: Authentication::None,
            body: RequestBody::Empty,
            settings: RequestSettings::default(),
            documentation: String::new(),
        }
    }

    fn temporary_database() -> PathBuf {
        std::env::temp_dir().join(format!("apex-history-{}.sqlite", ExecutionId::new()))
    }

    #[test]
    fn migrates_v1_database_and_preserves_existing_records() {
        let path = temporary_database();
        {
            let connection = Connection::open(&path).expect("open v1 database");
            connection
                .execute_batch(
                    "CREATE TABLE history (
                        execution_id TEXT PRIMARY KEY NOT NULL,
                        request_id TEXT NOT NULL,
                        request_name TEXT NOT NULL,
                        timestamp_ms INTEGER NOT NULL,
                        environment TEXT,
                        method TEXT NOT NULL,
                        resolved_url TEXT,
                        status INTEGER,
                        duration_ms INTEGER NOT NULL,
                        response_size INTEGER,
                        error_category TEXT,
                        pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1))
                    );
                    CREATE INDEX history_timestamp_idx ON history(timestamp_ms DESC);
                    CREATE INDEX history_request_idx ON history(request_id, timestamp_ms DESC);
                    INSERT INTO history VALUES (
                        'legacy', 'request', 'Legacy', 1, NULL, 'GET', NULL,
                        200, 5, 10, NULL, 0
                    );
                    PRAGMA user_version = 1;",
                )
                .expect("create v1 schema");
        }
        let database = HistoryDatabase::open(&path).expect("migrate database");
        assert_eq!(database.list(10).unwrap()[0].execution_id, "legacy");
        assert!(database.get("legacy").unwrap().unwrap().snapshot.is_none());
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let snapshot_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'history_snapshots'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(snapshot_table, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn metadata_history_redacts_query_values() {
        let path = temporary_database();
        let database = HistoryDatabase::open(&path).expect("database opens");
        let policy = HistoryPolicy::default();
        let record = HistoryRecord::success(
            "execution-1",
            &request(),
            Duration::from_millis(12),
            Some(200),
            42,
            &policy,
        );
        database.insert(&record, &policy).expect("insert");
        let listed = database.list(10).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].resolved_url.as_deref(),
            Some(
                "https://example.test/users?token=%5BREDACTED%5D&tag=%5BREDACTED%5D&filter=%5BREDACTED%5D"
            )
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn url_redaction_preserves_fragment_and_duplicate_names() {
        let redacted = redact_query_values("https://example.test/path?a=one&a=two#section");
        assert_eq!(
            redacted,
            "https://example.test/path?a=%5BREDACTED%5D&a=%5BREDACTED%5D#section"
        );
    }

    #[test]
    fn retention_keeps_only_newest_unpinned_records() {
        let path = temporary_database();
        let database = HistoryDatabase::open(&path).expect("database opens");
        let policy = HistoryPolicy {
            maximum_entries: 2,
            ..HistoryPolicy::default()
        };
        for index in 0..3 {
            let record = HistoryRecord::success(
                format!("execution-{index}"),
                &request(),
                Duration::from_millis(index),
                Some(200),
                index,
                &policy,
            );
            database.insert(&record, &policy).expect("insert");
        }
        assert_eq!(database.count().expect("count"), 2);
        let _ = fs::remove_file(path);
    }
}
