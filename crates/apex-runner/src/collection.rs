use apex_domain::CancellationToken;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct RunItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub iteration_data: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum FailurePolicy {
    Continue,
    Stop,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum CookiePolicy {
    Shared,
    IsolatedPerItem,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub concurrency: usize,
    pub retries: usize,
    pub retry_backoff: Duration,
    pub failure_policy: FailurePolicy,
    pub cookie_policy: CookiePolicy,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            concurrency: 1,
            retries: 0,
            retry_backoff: Duration::from_millis(100),
            failure_policy: FailurePolicy::Continue,
            cookie_policy: CookiePolicy::Shared,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum RunEvent {
    Started {
        total: usize,
    },
    ItemStarted {
        id: String,
        attempt: usize,
    },
    ItemFinished {
        id: String,
        passed: bool,
        attempt: usize,
    },
    Cancelled,
    Finished {
        passed: usize,
        failed: usize,
    },
}

pub trait RunEventSink: Send + Sync {
    fn emit(&self, event: RunEvent);
}

impl<F> RunEventSink for F
where
    F: Fn(RunEvent) + Send + Sync,
{
    fn emit(&self, event: RunEvent) {
        self(event);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemExecution {
    pub passed: bool,
    pub message: String,
    pub duration_ms: u64,
}

pub trait ItemExecutor: Send + Sync {
    fn execute(
        &self,
        item: &RunItem,
        cancellation: &CancellationToken,
    ) -> Result<ItemExecution, String>;
}

impl<F> ItemExecutor for F
where
    F: Fn(&RunItem, &CancellationToken) -> Result<ItemExecution, String> + Send + Sync,
{
    fn execute(
        &self,
        item: &RunItem,
        cancellation: &CancellationToken,
    ) -> Result<ItemExecution, String> {
        self(item, cancellation)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemRunResult {
    pub id: String,
    pub name: String,
    pub passed: bool,
    pub attempts: usize,
    pub duration_ms: u64,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct RunSummary {
    pub results: Vec<ItemRunResult>,
    pub cancelled: bool,
    pub exit_code: i32,
}

impl RunSummary {
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|result| result.passed).count()
    }

    pub fn failed(&self) -> usize {
        self.results.iter().filter(|result| !result.passed).count()
    }

    pub fn to_json(&self) -> Result<String, ReportError> {
        serde_json::to_string_pretty(self).map_err(ReportError::Json)
    }

    pub fn to_junit(&self, suite_name: &str) -> String {
        let mut xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\">\n",
            escape_xml(suite_name),
            self.results.len(),
            self.failed()
        );
        for result in &self.results {
            xml.push_str(&format!(
                "  <testcase name=\"{}\" time=\"{:.3}\">",
                escape_xml(&result.name),
                result.duration_ms as f64 / 1000.0
            ));
            if result.passed {
                xml.push_str("</testcase>\n");
            } else {
                xml.push_str(&format!(
                    "<failure message=\"{}\" /></testcase>\n",
                    escape_xml(&result.message)
                ));
            }
        }
        xml.push_str("</testsuite>\n");
        xml
    }

    pub fn to_html(&self, title: &str) -> String {
        let mut rows = String::new();
        for result in &self.results {
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&result.name),
                if result.passed { "passed" } else { "failed" },
                result.attempts,
                escape_html(&result.message)
            ));
        }
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1><p>passed: {} failed: {}</p><table><thead><tr><th>name</th><th>status</th><th>attempts</th><th>message</th></tr></thead><tbody>{}</tbody></table></body></html>",
            escape_html(title),
            escape_html(title),
            self.passed(),
            self.failed(),
            rows
        )
    }
}

#[derive(Debug)]
pub enum ReportError {
    Json(serde_json::Error),
}

impl Display for ReportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "could not serialize report: {error}"),
        }
    }
}

impl std::error::Error for ReportError {}

pub fn run_collection(
    items: Vec<RunItem>,
    config: RunConfig,
    cancellation: CancellationToken,
    executor: Arc<dyn ItemExecutor>,
    events: Arc<dyn RunEventSink>,
) -> Result<RunSummary, String> {
    if config.concurrency == 0 || config.concurrency > 1024 {
        return Err("runner concurrency must be between 1 and 1024".to_owned());
    }
    events.emit(RunEvent::Started { total: items.len() });
    let queue = Arc::new(Mutex::new(VecDeque::from(items)));
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(Mutex::new(false));
    let worker_count = config.concurrency;
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let sender = sender.clone();
        let executor = Arc::clone(&executor);
        let cancellation = cancellation.clone();
        let events = Arc::clone(&events);
        let stop = Arc::clone(&stop);
        let config = config.clone();
        workers.push(thread::spawn(move || {
            loop {
                if cancellation.is_cancelled() || *stop.lock().expect("stop lock") {
                    break;
                }
                let item = queue.lock().expect("queue lock").pop_front();
                let Some(item) = item else { break };
                let started = Instant::now();
                let mut attempts = 0;
                let final_execution = loop {
                    attempts += 1;
                    events.emit(RunEvent::ItemStarted {
                        id: item.id.clone(),
                        attempt: attempts,
                    });
                    let execution = executor.execute(&item, &cancellation);
                    let passed = execution.as_ref().is_ok_and(|value| value.passed);
                    events.emit(RunEvent::ItemFinished {
                        id: item.id.clone(),
                        passed,
                        attempt: attempts,
                    });
                    if passed || attempts > config.retries || cancellation.is_cancelled() {
                        break execution;
                    }
                    thread::sleep(
                        config
                            .retry_backoff
                            .saturating_mul(u32::try_from(attempts).unwrap_or(u32::MAX)),
                    );
                };
                let (passed, message, reported_ms) = match final_execution {
                    Ok(execution) => (execution.passed, execution.message, execution.duration_ms),
                    Err(message) => (false, message, 0),
                };
                let duration_ms = if reported_ms == 0 {
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
                } else {
                    reported_ms
                };
                let result = ItemRunResult {
                    id: item.id,
                    name: item.name,
                    passed,
                    attempts,
                    duration_ms,
                    message,
                };
                if !passed && config.failure_policy == FailurePolicy::Stop {
                    *stop.lock().expect("stop lock") = true;
                }
                if sender.send(result).is_err() {
                    break;
                }
            }
        }));
    }
    drop(sender);
    let mut results: Vec<_> = receiver.into_iter().collect();
    for worker in workers {
        worker
            .join()
            .map_err(|_| "runner worker panicked".to_owned())?;
    }
    results.sort_by(|left, right| left.id.cmp(&right.id));
    let cancelled = cancellation.is_cancelled();
    if cancelled {
        events.emit(RunEvent::Cancelled);
    }
    let passed = results.iter().filter(|result| result.passed).count();
    let failed = results.len().saturating_sub(passed);
    events.emit(RunEvent::Finished { passed, failed });
    Ok(RunSummary {
        exit_code: if cancelled {
            2
        } else if failed == 0 {
            0
        } else {
            1
        },
        results,
        cancelled,
    })
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_html(value: &str) -> String {
    escape_xml(value).replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn item(id: &str) -> RunItem {
        RunItem {
            id: id.to_owned(),
            name: format!("item-{id}"),
            iteration_data: BTreeMap::new(),
        }
    }

    #[test]
    fn concurrency_is_bounded_and_results_are_deterministic() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let executor = {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            Arc::new(move |_: &RunItem, _: &CancellationToken| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(ItemExecution {
                    passed: true,
                    message: "ok".to_owned(),
                    duration_ms: 5,
                })
            }) as Arc<dyn ItemExecutor>
        };
        let summary = run_collection(
            vec![item("c"), item("a"), item("b")],
            RunConfig {
                concurrency: 2,
                ..RunConfig::default()
            },
            CancellationToken::default(),
            executor,
            Arc::new(|_: RunEvent| {}),
        )
        .expect("run succeeds");
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert_eq!(
            summary
                .results
                .iter()
                .map(|result| result.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(summary.exit_code, 0);
    }

    #[test]
    fn retries_and_stop_policy_are_enforced() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let executor = {
            let attempts = Arc::clone(&attempts);
            Arc::new(move |item: &RunItem, _: &CancellationToken| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(ItemExecution {
                    passed: item.id != "a",
                    message: "failed".to_owned(),
                    duration_ms: 1,
                })
            }) as Arc<dyn ItemExecutor>
        };
        let summary = run_collection(
            vec![item("a"), item("b")],
            RunConfig {
                retries: 1,
                failure_policy: FailurePolicy::Stop,
                ..RunConfig::default()
            },
            CancellationToken::default(),
            executor,
            Arc::new(|_: RunEvent| {}),
        )
        .expect("run succeeds");
        assert_eq!(summary.results.len(), 1);
        assert_eq!(summary.results[0].attempts, 2);
        assert_eq!(summary.exit_code, 1);
    }

    #[test]
    fn cancellation_has_distinct_exit_code() {
        let token = CancellationToken::default();
        token.cancel();
        let summary = run_collection(
            vec![item("a")],
            RunConfig::default(),
            token,
            Arc::new(|_: &RunItem, _: &CancellationToken| {
                Ok(ItemExecution {
                    passed: true,
                    message: "ok".to_owned(),
                    duration_ms: 1,
                })
            }),
            Arc::new(|_: RunEvent| {}),
        )
        .expect("run succeeds");
        assert!(summary.cancelled);
        assert_eq!(summary.exit_code, 2);
    }

    #[test]
    fn reports_escape_content_and_are_stable() {
        let summary = RunSummary {
            results: vec![ItemRunResult {
                id: "1".to_owned(),
                name: "a<b".to_owned(),
                passed: false,
                attempts: 1,
                duration_ms: 10,
                message: "x&y".to_owned(),
            }],
            cancelled: false,
            exit_code: 1,
        };
        assert!(
            summary
                .to_json()
                .expect("json")
                .contains("\"exit_code\": 1")
        );
        assert!(summary.to_junit("suite").contains("a&lt;b"));
        assert!(summary.to_html("report").contains("x&amp;y"));
    }
}
