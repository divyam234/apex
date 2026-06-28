use crate::{HistoryEntry, HistorySnapshot};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticDiffPolicy {
    pub maximum_json_changes: usize,
    pub maximum_json_depth: usize,
    pub maximum_text_bytes: usize,
}

impl Default for SemanticDiffPolicy {
    fn default() -> Self {
        Self {
            maximum_json_changes: 1_000,
            maximum_json_depth: 128,
            maximum_text_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueChange<T> {
    pub left: T,
    pub right: T,
    pub changed: bool,
}

impl<T: Eq> ValueChange<T> {
    fn new(left: T, right: T) -> Self {
        let changed = left != right;
        Self {
            left,
            right,
            changed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeaderDifference {
    pub name: String,
    pub left_values: Vec<String>,
    pub right_values: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CookieDifference {
    pub name: String,
    pub left_value: Option<String>,
    pub right_value: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JsonChangeKind {
    Added,
    Removed,
    Changed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonChange {
    pub pointer: String,
    pub kind: JsonChangeKind,
    pub left: Option<Value>,
    pub right: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonBodyDiff {
    pub changes: Vec<JsonChange>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextBodyDiff {
    pub common_prefix_lines: usize,
    pub common_suffix_lines: usize,
    pub left_changed_lines: Vec<String>,
    pub right_changed_lines: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BinaryBodyDiff {
    pub left_length: usize,
    pub right_length: usize,
    pub first_difference: Option<usize>,
    pub left_truncated: bool,
    pub right_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BodyDifference {
    Unavailable,
    Unchanged,
    Json(JsonBodyDiff),
    Text(TextBodyDiff),
    Binary(BinaryBodyDiff),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticResponseDiff {
    pub status: ValueChange<Option<u16>>,
    pub duration_ms: ValueChange<u64>,
    pub response_size: ValueChange<Option<u64>>,
    pub headers: Vec<HeaderDifference>,
    pub cookies: Vec<CookieDifference>,
    pub body: BodyDifference,
}

pub fn semantic_response_diff(
    left: &HistoryEntry,
    right: &HistoryEntry,
    policy: &SemanticDiffPolicy,
) -> SemanticResponseDiff {
    let left_snapshot = left.snapshot.as_ref();
    let right_snapshot = right.snapshot.as_ref();
    SemanticResponseDiff {
        status: ValueChange::new(
            snapshot_status(left_snapshot).or(left.record.status),
            snapshot_status(right_snapshot).or(right.record.status),
        ),
        duration_ms: ValueChange::new(left.record.duration_ms, right.record.duration_ms),
        response_size: ValueChange::new(left.record.response_size, right.record.response_size),
        headers: diff_headers(left_snapshot, right_snapshot),
        cookies: diff_cookies(left_snapshot, right_snapshot),
        body: diff_body(left_snapshot, right_snapshot, policy),
    }
}

fn snapshot_status(snapshot: Option<&HistorySnapshot>) -> Option<u16> {
    snapshot.and_then(|snapshot| snapshot.response_status)
}

fn diff_headers(
    left: Option<&HistorySnapshot>,
    right: Option<&HistorySnapshot>,
) -> Vec<HeaderDifference> {
    let left = grouped_headers(left.map_or(&[], |snapshot| &snapshot.response_headers));
    let right = grouped_headers(right.map_or(&[], |snapshot| &snapshot.response_headers));
    let names = left
        .keys()
        .chain(right.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    names
        .into_iter()
        .filter_map(|name| {
            let left_values = left.get(&name).cloned().unwrap_or_default();
            let right_values = right.get(&name).cloned().unwrap_or_default();
            (left_values != right_values).then_some(HeaderDifference {
                name,
                left_values,
                right_values,
            })
        })
        .collect()
}

fn grouped_headers(headers: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for (name, value) in headers {
        grouped
            .entry(name.to_ascii_lowercase())
            .or_default()
            .push(value.clone());
    }
    grouped
}

fn diff_cookies(
    left: Option<&HistorySnapshot>,
    right: Option<&HistorySnapshot>,
) -> Vec<CookieDifference> {
    let left = cookies(left.map_or(&[], |snapshot| &snapshot.response_headers));
    let right = cookies(right.map_or(&[], |snapshot| &snapshot.response_headers));
    let names = left
        .keys()
        .chain(right.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    names
        .into_iter()
        .filter_map(|name| {
            let left_value = left.get(&name).cloned();
            let right_value = right.get(&name).cloned();
            (left_value != right_value).then_some(CookieDifference {
                name,
                left_value,
                right_value,
            })
        })
        .collect()
}

fn cookies(headers: &[(String, String)]) -> BTreeMap<String, String> {
    let mut cookies = BTreeMap::new();
    for (_, value) in headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
    {
        let first = value.split(';').next().unwrap_or_default();
        if let Some((name, value)) = first.split_once('=') {
            cookies.insert(name.trim().to_owned(), value.trim().to_owned());
        }
    }
    cookies
}

fn diff_body(
    left: Option<&HistorySnapshot>,
    right: Option<&HistorySnapshot>,
    policy: &SemanticDiffPolicy,
) -> BodyDifference {
    let (Some(left), Some(right)) = (left, right) else {
        return BodyDifference::Unavailable;
    };
    let (Some(left_body), Some(right_body)) = (
        left.response_body.as_deref(),
        right.response_body.as_deref(),
    ) else {
        return BodyDifference::Unavailable;
    };
    if left_body == right_body
        && left.response_truncated == right.response_truncated
        && left.response_content_type == right.response_content_type
    {
        return BodyDifference::Unchanged;
    }

    let left_json = serde_json::from_slice::<Value>(left_body);
    let right_json = serde_json::from_slice::<Value>(right_body);
    let content_type_is_json = left
        .response_content_type
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("json"))
        || right
            .response_content_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("json"));
    if let (Ok(left_json), Ok(right_json)) = (left_json, right_json) {
        return BodyDifference::Json(json_diff(
            &left_json,
            &right_json,
            policy,
            left.response_truncated || right.response_truncated,
        ));
    }
    if content_type_is_json {
        return text_or_binary_diff(left, right, policy);
    }
    text_or_binary_diff(left, right, policy)
}

fn json_diff(
    left: &Value,
    right: &Value,
    policy: &SemanticDiffPolicy,
    initially_truncated: bool,
) -> JsonBodyDiff {
    let mut output = JsonBodyDiff {
        changes: Vec::new(),
        truncated: initially_truncated,
    };
    walk_json(left, right, "", 0, policy, &mut output);
    output
}

fn walk_json(
    left: &Value,
    right: &Value,
    pointer: &str,
    depth: usize,
    policy: &SemanticDiffPolicy,
    output: &mut JsonBodyDiff,
) {
    if left == right || output.changes.len() >= policy.maximum_json_changes {
        if output.changes.len() >= policy.maximum_json_changes && left != right {
            output.truncated = true;
        }
        return;
    }
    if depth >= policy.maximum_json_depth {
        output.truncated = true;
        push_json_change(
            output,
            policy,
            JsonChange {
                pointer: pointer.to_owned(),
                kind: JsonChangeKind::Changed,
                left: Some(left.clone()),
                right: Some(right.clone()),
            },
        );
        return;
    }
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let keys = left
                .keys()
                .chain(right.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let child = format!("{pointer}/{}", escape_pointer(&key));
                match (left.get(&key), right.get(&key)) {
                    (Some(left), Some(right)) => {
                        walk_json(left, right, &child, depth + 1, policy, output);
                    }
                    (Some(left), None) => push_json_change(
                        output,
                        policy,
                        JsonChange {
                            pointer: child,
                            kind: JsonChangeKind::Removed,
                            left: Some(left.clone()),
                            right: None,
                        },
                    ),
                    (None, Some(right)) => push_json_change(
                        output,
                        policy,
                        JsonChange {
                            pointer: child,
                            kind: JsonChangeKind::Added,
                            left: None,
                            right: Some(right.clone()),
                        },
                    ),
                    (None, None) => {}
                }
                if output.changes.len() >= policy.maximum_json_changes {
                    output.truncated = true;
                    break;
                }
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            let maximum = left.len().max(right.len());
            for index in 0..maximum {
                let child = format!("{pointer}/{index}");
                match (left.get(index), right.get(index)) {
                    (Some(left), Some(right)) => {
                        walk_json(left, right, &child, depth + 1, policy, output);
                    }
                    (Some(left), None) => push_json_change(
                        output,
                        policy,
                        JsonChange {
                            pointer: child,
                            kind: JsonChangeKind::Removed,
                            left: Some(left.clone()),
                            right: None,
                        },
                    ),
                    (None, Some(right)) => push_json_change(
                        output,
                        policy,
                        JsonChange {
                            pointer: child,
                            kind: JsonChangeKind::Added,
                            left: None,
                            right: Some(right.clone()),
                        },
                    ),
                    (None, None) => {}
                }
                if output.changes.len() >= policy.maximum_json_changes {
                    output.truncated = true;
                    break;
                }
            }
        }
        _ => push_json_change(
            output,
            policy,
            JsonChange {
                pointer: pointer.to_owned(),
                kind: JsonChangeKind::Changed,
                left: Some(left.clone()),
                right: Some(right.clone()),
            },
        ),
    }
}

fn push_json_change(output: &mut JsonBodyDiff, policy: &SemanticDiffPolicy, change: JsonChange) {
    if output.changes.len() < policy.maximum_json_changes {
        output.changes.push(change);
    } else {
        output.truncated = true;
    }
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn text_or_binary_diff(
    left: &HistorySnapshot,
    right: &HistorySnapshot,
    policy: &SemanticDiffPolicy,
) -> BodyDifference {
    let left_body = left.response_body.as_deref().unwrap_or_default();
    let right_body = right.response_body.as_deref().unwrap_or_default();
    match (
        std::str::from_utf8(left_body),
        std::str::from_utf8(right_body),
    ) {
        (Ok(left_text), Ok(right_text)) => BodyDifference::Text(text_diff(
            left_text,
            right_text,
            policy.maximum_text_bytes,
            left.response_truncated || right.response_truncated,
        )),
        _ => BodyDifference::Binary(binary_diff(left, right)),
    }
}

fn text_diff(
    left: &str,
    right: &str,
    maximum_bytes: usize,
    initially_truncated: bool,
) -> TextBodyDiff {
    let (left, left_truncated) = truncate_text(left, maximum_bytes);
    let (right, right_truncated) = truncate_text(right, maximum_bytes);
    let left_lines = left.lines().map(str::to_owned).collect::<Vec<_>>();
    let right_lines = right.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut prefix = 0;
    while prefix < left_lines.len()
        && prefix < right_lines.len()
        && left_lines[prefix] == right_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < left_lines.len().saturating_sub(prefix)
        && suffix < right_lines.len().saturating_sub(prefix)
        && left_lines[left_lines.len() - 1 - suffix] == right_lines[right_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    TextBodyDiff {
        common_prefix_lines: prefix,
        common_suffix_lines: suffix,
        left_changed_lines: left_lines[prefix..left_lines.len().saturating_sub(suffix)].to_vec(),
        right_changed_lines: right_lines[prefix..right_lines.len().saturating_sub(suffix)].to_vec(),
        truncated: initially_truncated || left_truncated || right_truncated,
    }
}

fn truncate_text(value: &str, maximum_bytes: usize) -> (&str, bool) {
    if value.len() <= maximum_bytes {
        return (value, false);
    }
    let mut boundary = maximum_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (&value[..boundary], true)
}

fn binary_diff(left: &HistorySnapshot, right: &HistorySnapshot) -> BinaryBodyDiff {
    let left_body = left.response_body.as_deref().unwrap_or_default();
    let right_body = right.response_body.as_deref().unwrap_or_default();
    let shared = left_body.len().min(right_body.len());
    let first_difference = (0..shared)
        .find(|index| left_body[*index] != right_body[*index])
        .or_else(|| (left_body.len() != right_body.len()).then_some(shared));
    BinaryBodyDiff {
        left_length: left_body.len(),
        right_length: right_body.len(),
        first_difference,
        left_truncated: left.response_truncated,
        right_truncated: right.response_truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HistoryRecord, HistorySnapshot};

    fn entry(
        id: &str,
        status: u16,
        duration_ms: u64,
        headers: Vec<(String, String)>,
        content_type: &str,
        body: &[u8],
    ) -> HistoryEntry {
        HistoryEntry {
            record: HistoryRecord {
                execution_id: id.to_owned(),
                request_id: "request".to_owned(),
                request_name: "Request".to_owned(),
                timestamp_ms: 0,
                environment: None,
                method: "GET".to_owned(),
                resolved_url: None,
                status: Some(status),
                duration_ms,
                response_size: Some(body.len() as u64),
                error_category: None,
                pinned: false,
            },
            snapshot: Some(HistorySnapshot {
                response_status: Some(status),
                response_headers: headers,
                response_body: Some(body.to_vec()),
                response_content_type: Some(content_type.to_owned()),
                ..HistorySnapshot::default()
            }),
        }
    }

    #[test]
    fn json_diff_is_structural_and_pointer_sorted() {
        let left = entry(
            "left",
            200,
            10,
            Vec::new(),
            "application/json",
            br#"{"id":1,"name":"Ada","tags":["a"]}"#,
        );
        let right = entry(
            "right",
            201,
            15,
            Vec::new(),
            "application/json",
            br#"{"id":1,"name":"Grace","tags":["a","b"]}"#,
        );
        let diff = semantic_response_diff(&left, &right, &SemanticDiffPolicy::default());
        assert!(diff.status.changed);
        assert!(diff.duration_ms.changed);
        let BodyDifference::Json(body) = diff.body else {
            panic!("expected JSON diff");
        };
        assert_eq!(body.changes.len(), 2);
        assert_eq!(body.changes[0].pointer, "/name");
        assert_eq!(body.changes[1].pointer, "/tags/1");
    }

    #[test]
    fn duplicate_headers_and_cookies_are_compared_semantically() {
        let left = entry(
            "left",
            200,
            1,
            vec![
                ("X-Test".to_owned(), "one".to_owned()),
                ("X-Test".to_owned(), "two".to_owned()),
                ("Set-Cookie".to_owned(), "session=left; Path=/".to_owned()),
            ],
            "text/plain",
            b"same",
        );
        let right = entry(
            "right",
            200,
            1,
            vec![
                ("x-test".to_owned(), "one".to_owned()),
                ("Set-Cookie".to_owned(), "session=right; Path=/".to_owned()),
            ],
            "text/plain",
            b"same",
        );
        let diff = semantic_response_diff(&left, &right, &SemanticDiffPolicy::default());
        assert_eq!(diff.headers.len(), 2);
        assert_eq!(diff.cookies.len(), 1);
        assert_eq!(diff.cookies[0].name, "session");
    }

    #[test]
    fn text_diff_reports_changed_middle_lines() {
        let left = entry(
            "left",
            200,
            1,
            Vec::new(),
            "text/plain",
            b"same\nleft\ntail",
        );
        let right = entry(
            "right",
            200,
            1,
            Vec::new(),
            "text/plain",
            b"same\nright\ntail",
        );
        let diff = semantic_response_diff(&left, &right, &SemanticDiffPolicy::default());
        let BodyDifference::Text(body) = diff.body else {
            panic!("expected text diff");
        };
        assert_eq!(body.common_prefix_lines, 1);
        assert_eq!(body.common_suffix_lines, 1);
        assert_eq!(body.left_changed_lines, ["left"]);
        assert_eq!(body.right_changed_lines, ["right"]);
    }

    #[test]
    fn binary_diff_is_bounded_and_reports_first_byte() {
        let left = entry(
            "left",
            200,
            1,
            Vec::new(),
            "application/octet-stream",
            &[0xff, 1, 2],
        );
        let right = entry(
            "right",
            200,
            1,
            Vec::new(),
            "application/octet-stream",
            &[0xff, 9, 2, 3],
        );
        let diff = semantic_response_diff(&left, &right, &SemanticDiffPolicy::default());
        let BodyDifference::Binary(body) = diff.body else {
            panic!("expected binary diff");
        };
        assert_eq!(body.first_difference, Some(1));
        assert_eq!(body.left_length, 3);
        assert_eq!(body.right_length, 4);
    }

    #[test]
    fn json_change_limit_marks_truncation() {
        let left = entry(
            "left",
            200,
            1,
            Vec::new(),
            "application/json",
            br#"{"a":1,"b":2,"c":3}"#,
        );
        let right = entry(
            "right",
            200,
            1,
            Vec::new(),
            "application/json",
            br#"{"a":4,"b":5,"c":6}"#,
        );
        let diff = semantic_response_diff(
            &left,
            &right,
            &SemanticDiffPolicy {
                maximum_json_changes: 1,
                ..SemanticDiffPolicy::default()
            },
        );
        let BodyDifference::Json(body) = diff.body else {
            panic!("expected JSON diff");
        };
        assert_eq!(body.changes.len(), 1);
        assert!(body.truncated);
    }
}
