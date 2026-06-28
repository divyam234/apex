use crate::{WorkspaceError, WorkspaceRepository};
use apex_domain::{HttpRequest, MultipartValue, RequestBody, ValueSensitivity};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SEARCH_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchIndexPolicy {
    pub maximum_documents: usize,
    pub maximum_document_bytes: usize,
    pub maximum_terms_per_document: usize,
    pub maximum_results: usize,
}

impl Default for SearchIndexPolicy {
    fn default() -> Self {
        Self {
            maximum_documents: 100_000,
            maximum_document_bytes: 512 * 1024,
            maximum_terms_per_document: 10_000,
            maximum_results: 200,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchRefreshReport {
    pub scanned: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub truncated: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchField {
    Name,
    Method,
    Url,
    Headers,
    Body,
    Documentation,
}

impl SearchField {
    fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Method => "method",
            Self::Url => "url",
            Self::Headers => "headers",
            Self::Body => "body",
            Self::Documentation => "documentation",
        }
    }

    fn weight(self) -> i64 {
        match self {
            Self::Name => 50,
            Self::Method => 40,
            Self::Url => 35,
            Self::Headers => 20,
            Self::Body => 10,
            Self::Documentation => 8,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchQuery {
    pub text: String,
    pub exact: bool,
    pub method: Option<String>,
    pub field: Option<SearchField>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    pub relative_path: PathBuf,
    pub name: String,
    pub method: String,
    pub url: String,
    pub score: i64,
    pub matched_fields: Vec<SearchField>,
    pub truncated_source: bool,
}

pub struct WorkspaceSearchIndex {
    connection: Connection,
    policy: SearchIndexPolicy,
}

impl WorkspaceSearchIndex {
    pub fn open(
        repository: &WorkspaceRepository,
        policy: SearchIndexPolicy,
    ) -> Result<Self, SearchIndexError> {
        let path = repository.root().join(".apex").join("search.sqlite");
        Self::open_path(&path, policy)
    }

    pub fn open_path(path: &Path, policy: SearchIndexPolicy) -> Result<Self, SearchIndexError> {
        let parent = path
            .parent()
            .ok_or_else(|| SearchIndexError::InvalidPath(path.to_owned()))?;
        std::fs::create_dir_all(parent)?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS search_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS search_documents (
                path TEXT PRIMARY KEY,
                fingerprint TEXT NOT NULL,
                name TEXT NOT NULL,
                method TEXT NOT NULL,
                url TEXT NOT NULL,
                truncated INTEGER NOT NULL CHECK (truncated IN (0, 1))
            );
            CREATE TABLE IF NOT EXISTS search_terms (
                path TEXT NOT NULL,
                term TEXT NOT NULL,
                field TEXT NOT NULL,
                PRIMARY KEY (path, term, field),
                FOREIGN KEY (path) REFERENCES search_documents(path) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS search_terms_term_idx ON search_terms(term);
            CREATE INDEX IF NOT EXISTS search_documents_method_idx ON search_documents(method);",
        )?;
        let existing_version = connection
            .query_row(
                "SELECT value FROM search_metadata WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing_version {
            Some(version) if version == SEARCH_SCHEMA_VERSION.to_string() => {}
            Some(version) => {
                return Err(SearchIndexError::UnsupportedSchema(version));
            }
            None => {
                connection.execute(
                    "INSERT INTO search_metadata(key, value) VALUES ('schema_version', ?1)",
                    [SEARCH_SCHEMA_VERSION.to_string()],
                )?;
            }
        }
        Ok(Self { connection, policy })
    }

    pub fn refresh(
        &mut self,
        repository: &WorkspaceRepository,
    ) -> Result<SearchRefreshReport, SearchIndexError> {
        let entries = repository.list_requests()?;
        if entries.len() > self.policy.maximum_documents {
            return Err(SearchIndexError::DocumentLimit {
                maximum: self.policy.maximum_documents,
                observed: entries.len(),
            });
        }
        let current_paths = entries
            .iter()
            .map(|entry| entry.relative_path.to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        let existing = read_existing_documents(&self.connection)?;
        let mut report = SearchRefreshReport {
            scanned: entries.len(),
            ..SearchRefreshReport::default()
        };
        let transaction = self.connection.transaction()?;
        for entry in entries {
            let loaded = repository.load_request(&entry.path)?;
            let relative_path = entry.relative_path.to_string_lossy().into_owned();
            let fingerprint = loaded.fingerprint.to_hex();
            if existing.get(&relative_path) == Some(&fingerprint) {
                report.unchanged += 1;
                continue;
            }
            let indexed = build_indexed_document(&loaded.value.request, &self.policy);
            if indexed.truncated {
                report.truncated += 1;
            }
            write_indexed_document(&transaction, &relative_path, &fingerprint, &indexed)?;
            if existing.contains_key(&relative_path) {
                report.updated += 1;
            } else {
                report.inserted += 1;
            }
        }
        for path in existing.keys() {
            if !current_paths.contains(path) {
                transaction.execute("DELETE FROM search_documents WHERE path = ?1", [path])?;
                report.removed += 1;
            }
        }
        transaction.commit()?;
        Ok(report)
    }

    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchIndexError> {
        let query_terms = tokenize(&query.text, 32);
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }
        let maximum_results = query
            .limit
            .unwrap_or(self.policy.maximum_results)
            .min(self.policy.maximum_results);
        let mut scores = BTreeMap::<String, SearchScore>::new();
        for query_term in query_terms {
            let pattern = if query.exact {
                query_term.clone()
            } else {
                format!("%{}%", escape_like(&query_term))
            };
            let sql = if query.exact {
                "SELECT path, term, field FROM search_terms WHERE term = ?1 LIMIT ?2"
            } else {
                "SELECT path, term, field FROM search_terms WHERE term LIKE ?1 ESCAPE '\\' LIMIT ?2"
            };
            let term_limit = self.policy.maximum_results.saturating_mul(100).min(50_000) as i64;
            let mut statement = self.connection.prepare(sql)?;
            let rows = statement.query_map(params![pattern, term_limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (path, term, field_name) = row?;
                let Some(field) = parse_field(&field_name) else {
                    continue;
                };
                if query.field.is_some_and(|selected| selected != field) {
                    continue;
                }
                let match_weight = if term == query_term {
                    100
                } else if term.starts_with(&query_term) {
                    60
                } else {
                    25
                };
                let score = scores.entry(path).or_default();
                score.value += match_weight + field.weight();
                score.fields.insert(field.as_str().to_owned());
            }
        }
        let mut ranked = scores.into_iter().collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .1
                .value
                .cmp(&left.1.value)
                .then_with(|| left.0.cmp(&right.0))
        });
        let mut results = Vec::new();
        for (path, score) in ranked {
            if results.len() >= maximum_results {
                break;
            }
            let document = self
                .connection
                .query_row(
                    "SELECT name, method, url, truncated FROM search_documents WHERE path = ?1",
                    [&path],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((name, method, url, truncated)) = document else {
                continue;
            };
            if query
                .method
                .as_ref()
                .is_some_and(|selected| !method.eq_ignore_ascii_case(selected))
            {
                continue;
            }
            let matched_fields = score
                .fields
                .iter()
                .filter_map(|field| parse_field(field))
                .collect();
            results.push(SearchResult {
                relative_path: PathBuf::from(path),
                name,
                method,
                url,
                score: score.value,
                matched_fields,
                truncated_source: truncated,
            });
        }
        Ok(results)
    }

    pub fn clear(&mut self) -> Result<(), SearchIndexError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM search_documents", [])?;
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Default)]
struct SearchScore {
    value: i64,
    fields: BTreeSet<String>,
}

struct IndexedDocument {
    name: String,
    method: String,
    url: String,
    truncated: bool,
    terms: BTreeMap<String, BTreeSet<String>>,
}

fn build_indexed_document(request: &HttpRequest, policy: &SearchIndexPolicy) -> IndexedDocument {
    let mut budget = policy.maximum_document_bytes;
    let mut truncated = false;
    let mut fields = Vec::<(SearchField, String)>::new();
    fields.push((SearchField::Name, request.name.clone()));
    fields.push((SearchField::Method, request.method.as_str().to_owned()));
    fields.push((SearchField::Url, request.url.clone()));
    let headers = request
        .headers
        .iter()
        .map(|header| {
            if header.sensitivity == ValueSensitivity::Public {
                format!("{} {}", header.name, header.value)
            } else {
                header.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fields.push((SearchField::Headers, headers));
    fields.push((SearchField::Body, body_search_text(&request.body)));
    fields.push((SearchField::Documentation, request.documentation.clone()));
    let mut terms = BTreeMap::<String, BTreeSet<String>>::new();
    let mut term_count = 0_usize;
    for (field, value) in fields {
        if budget == 0 || term_count >= policy.maximum_terms_per_document {
            truncated = true;
            break;
        }
        let bytes = value.as_bytes();
        let take = bytes.len().min(budget);
        let boundary = floor_char_boundary(&value, take);
        if boundary < bytes.len() {
            truncated = true;
        }
        budget = budget.saturating_sub(boundary);
        for term in tokenize(
            &value[..boundary],
            policy.maximum_terms_per_document - term_count,
        ) {
            terms
                .entry(term)
                .or_default()
                .insert(field.as_str().to_owned());
            term_count += 1;
            if term_count >= policy.maximum_terms_per_document {
                truncated = true;
                break;
            }
        }
    }
    IndexedDocument {
        name: request.name.clone(),
        method: request.method.as_str().to_owned(),
        url: request.url.clone(),
        truncated,
        terms,
    }
}

fn body_search_text(body: &RequestBody) -> String {
    match body {
        RequestBody::Empty => String::new(),
        RequestBody::Text { text, .. } | RequestBody::Json(text) | RequestBody::Xml(text) => {
            text.clone()
        }
        RequestBody::GraphQl {
            query,
            variables_json,
            operation_name,
        } => format!(
            "{}\n{}\n{}",
            operation_name.as_deref().unwrap_or_default(),
            query,
            variables_json
        ),
        RequestBody::FormUrlEncoded(fields) => fields
            .iter()
            .map(|field| {
                if field.sensitivity == ValueSensitivity::Public {
                    format!("{} {}", field.name, field.value)
                } else {
                    field.name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        RequestBody::Multipart(fields) => fields
            .iter()
            .map(|field| match &field.value {
                MultipartValue::Text(value) if field.sensitivity == ValueSensitivity::Public => {
                    format!("{} {value}", field.name)
                }
                MultipartValue::Text(_) => field.name.clone(),
                MultipartValue::File { relative_path } => {
                    format!("{} {relative_path}", field.name)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        RequestBody::BinaryFile { relative_path } | RequestBody::StreamFile { relative_path } => {
            relative_path.clone()
        }
    }
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn tokenize(value: &str, maximum: usize) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() || matches!(character, '_' | '-' | '.') {
            current.extend(character.to_lowercase());
            if current.len() >= 64 {
                output.push(std::mem::take(&mut current));
            }
        } else if !current.is_empty() {
            output.push(std::mem::take(&mut current));
        }
        if output.len() >= maximum {
            return output;
        }
    }
    if !current.is_empty() && output.len() < maximum {
        output.push(current);
    }
    output.sort();
    output.dedup();
    output
}

fn read_existing_documents(
    connection: &Connection,
) -> Result<BTreeMap<String, String>, SearchIndexError> {
    let mut statement = connection.prepare("SELECT path, fingerprint FROM search_documents")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(SearchIndexError::Database)
}

fn write_indexed_document(
    transaction: &Transaction<'_>,
    path: &str,
    fingerprint: &str,
    document: &IndexedDocument,
) -> Result<(), SearchIndexError> {
    transaction.execute(
        "INSERT INTO search_documents(path, fingerprint, name, method, url, truncated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET
            fingerprint = excluded.fingerprint,
            name = excluded.name,
            method = excluded.method,
            url = excluded.url,
            truncated = excluded.truncated",
        params![
            path,
            fingerprint,
            document.name,
            document.method,
            document.url,
            document.truncated
        ],
    )?;
    transaction.execute("DELETE FROM search_terms WHERE path = ?1", [path])?;
    let mut statement = transaction
        .prepare("INSERT OR IGNORE INTO search_terms(path, term, field) VALUES (?1, ?2, ?3)")?;
    for (term, fields) in &document.terms {
        for field in fields {
            statement.execute(params![path, term, field])?;
        }
    }
    Ok(())
}

fn parse_field(value: &str) -> Option<SearchField> {
    match value {
        "name" => Some(SearchField::Name),
        "method" => Some(SearchField::Method),
        "url" => Some(SearchField::Url),
        "headers" => Some(SearchField::Headers),
        "body" => Some(SearchField::Body),
        "documentation" => Some(SearchField::Documentation),
        _ => None,
    }
}

#[derive(Debug)]
pub enum SearchIndexError {
    Database(rusqlite::Error),
    Workspace(WorkspaceError),
    Io(std::io::Error),
    InvalidPath(PathBuf),
    UnsupportedSchema(String),
    DocumentLimit { maximum: usize, observed: usize },
}

impl Display for SearchIndexError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "search database failed: {error}"),
            Self::Workspace(error) => write!(formatter, "workspace search refresh failed: {error}"),
            Self::Io(error) => write!(formatter, "search index I/O failed: {error}"),
            Self::InvalidPath(path) => {
                write!(
                    formatter,
                    "search index path has no parent: {}",
                    path.display()
                )
            }
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported search index schema version: {version}"
                )
            }
            Self::DocumentLimit { maximum, observed } => write!(
                formatter,
                "workspace has {observed} searchable requests; configured maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for SearchIndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Workspace(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::InvalidPath(_) | Self::UnsupportedSchema(_) | Self::DocumentLimit { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for SearchIndexError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}

impl From<WorkspaceError> for SearchIndexError {
    fn from(value: WorkspaceError) -> Self {
        Self::Workspace(value)
    }
}

impl From<std::io::Error> for SearchIndexError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RequestDocument, WorkspaceManifest};
    use apex_domain::{
        Authentication, HeaderEntry, HttpMethod, RequestSettings, StableId, ValueSensitivity,
    };
    use apex_secrets::SecretLeakDetector;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn refresh_is_incremental_and_removes_deleted_requests() {
        let (repository, root) = fixture("incremental");
        let first = save_request(&repository, "users", "Get User", "GET", "alpha marker");
        let second = save_request(&repository, "health", "Health", "GET", "beta marker");
        let mut index =
            WorkspaceSearchIndex::open(&repository, SearchIndexPolicy::default()).unwrap();
        let first_report = index.refresh(&repository).unwrap();
        assert_eq!(first_report.inserted, 2);

        let loaded = repository.load_request(&first).unwrap();
        let mut document = loaded.value;
        document.request.documentation = "updated gamma marker".to_owned();
        repository
            .save_request(
                &first,
                &document,
                Some(loaded.fingerprint),
                &SecretLeakDetector::default(),
            )
            .unwrap();
        fs::remove_file(second).unwrap();
        let report = index.refresh(&repository).unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(report.removed, 1);
        assert_eq!(report.unchanged, 0);

        let results = index
            .search(&SearchQuery {
                text: "gamma".to_owned(),
                ..SearchQuery::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Get User");
        assert!(
            index
                .search(&SearchQuery {
                    text: "beta".to_owned(),
                    ..SearchQuery::default()
                })
                .unwrap()
                .is_empty()
        );
        cleanup(root);
    }

    #[test]
    fn exact_and_method_filters_are_enforced() {
        let (repository, root) = fixture("filters");
        save_request(
            &repository,
            "create",
            "Create User",
            "POST",
            "customer-profile",
        );
        save_request(&repository, "read", "Read User", "GET", "customer-profile");
        let mut index =
            WorkspaceSearchIndex::open(&repository, SearchIndexPolicy::default()).unwrap();
        index.refresh(&repository).unwrap();
        let results = index
            .search(&SearchQuery {
                text: "customer-profile".to_owned(),
                exact: true,
                method: Some("POST".to_owned()),
                ..SearchQuery::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].method, "POST");
        cleanup(root);
    }

    #[test]
    fn document_and_content_limits_fail_or_truncate_deterministically() {
        let (repository, root) = fixture("limits");
        save_request(
            &repository,
            "large",
            "Large",
            "POST",
            &"content ".repeat(100),
        );
        let mut index = WorkspaceSearchIndex::open(
            &repository,
            SearchIndexPolicy {
                maximum_documents: 1,
                maximum_document_bytes: 32,
                maximum_terms_per_document: 5,
                maximum_results: 10,
            },
        )
        .unwrap();
        let report = index.refresh(&repository).unwrap();
        assert_eq!(report.truncated, 1);
        save_request(&repository, "second", "Second", "GET", "small");
        assert!(matches!(
            index.refresh(&repository),
            Err(SearchIndexError::DocumentLimit {
                maximum: 1,
                observed: 2
            })
        ));
        cleanup(root);
    }

    #[test]
    fn header_and_body_fields_are_searchable_without_indexing_authentication() {
        let (repository, root) = fixture("fields");
        let path = save_request(&repository, "fields", "Fields", "POST", "body-marker");
        let loaded = repository.load_request(&path).unwrap();
        let mut document = loaded.value;
        let mut header = HeaderEntry::new("X-Search", "header-marker").unwrap();
        header.sensitivity = ValueSensitivity::Sensitive;
        document.request.headers.push(header);
        document.request.authentication = Authentication::Bearer {
            token: "{{never_index_this_token}}".to_owned(),
        };
        repository
            .save_request(
                &path,
                &document,
                Some(loaded.fingerprint),
                &SecretLeakDetector::default(),
            )
            .unwrap();
        let mut index =
            WorkspaceSearchIndex::open(&repository, SearchIndexPolicy::default()).unwrap();
        index.refresh(&repository).unwrap();
        assert_eq!(
            index
                .search(&SearchQuery {
                    text: "x-search".to_owned(),
                    field: Some(SearchField::Headers),
                    ..SearchQuery::default()
                })
                .unwrap()
                .len(),
            1
        );
        for excluded in ["header-marker", "never_index_this_token"] {
            assert!(
                index
                    .search(&SearchQuery {
                        text: excluded.to_owned(),
                        ..SearchQuery::default()
                    })
                    .unwrap()
                    .is_empty(),
                "excluded term: {excluded}"
            );
        }
        cleanup(root);
    }

    fn fixture(name: &str) -> (WorkspaceRepository, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "apex-search-{name}-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let repository = WorkspaceRepository::new(&root).unwrap();
        repository
            .initialize(&WorkspaceManifest::new(
                StableId::parse("workspace").unwrap(),
                "Search fixture",
            ))
            .unwrap();
        let collection = repository.collection_path("search").unwrap();
        fs::create_dir_all(collection).unwrap();
        (repository, root)
    }

    fn save_request(
        repository: &WorkspaceRepository,
        slug: &str,
        name: &str,
        method: &str,
        documentation: &str,
    ) -> PathBuf {
        let path = repository
            .collection_path("search")
            .unwrap()
            .join(format!("{slug}.request.toml"));
        let request = HttpRequest {
            id: StableId::parse(format!("request-{slug}")).unwrap(),
            name: name.to_owned(),
            method: HttpMethod::parse(method).unwrap(),
            url: format!("https://example.test/{slug}"),
            query: Vec::new(),
            headers: Vec::new(),
            authentication: Authentication::None,
            body: RequestBody::Text {
                content_type: Some("text/plain".to_owned()),
                text: documentation.to_owned(),
            },
            settings: RequestSettings::default(),
            documentation: documentation.to_owned(),
        };
        repository
            .save_request(
                &path,
                &RequestDocument::new(request),
                None,
                &SecretLeakDetector::default(),
            )
            .unwrap();
        path
    }

    fn cleanup(root: PathBuf) {
        fs::remove_dir_all(root).unwrap();
    }
}
