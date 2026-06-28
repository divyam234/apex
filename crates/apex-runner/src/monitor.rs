use crate::{ItemExecutor, RunConfig, RunEventSink, RunItem, RunSummary, run_collection};
use apex_domain::CancellationToken;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct MonitorDefinition {
    pub id: String,
    pub name: String,
    pub schedule: String,
    pub workspace: PathBuf,
    pub collection: String,
    pub report_retention: usize,
    pub notify_on_success: bool,
    pub notify_on_failure: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorNotification {
    pub monitor_id: String,
    pub passed: bool,
    pub report_path: PathBuf,
}

pub trait NotificationSink: Send + Sync {
    fn notify(&self, notification: &MonitorNotification) -> Result<(), String>;
}

impl<F> NotificationSink for F
where
    F: Fn(&MonitorNotification) -> Result<(), String> + Send + Sync,
{
    fn notify(&self, notification: &MonitorNotification) -> Result<(), String> {
        self(notification)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorRunResult {
    pub summary: RunSummary,
    pub report_path: PathBuf,
    pub notification_error: Option<String>,
}

pub struct MonitorRunContext<'a> {
    pub cancellation: CancellationToken,
    pub executor: Arc<dyn ItemExecutor>,
    pub events: Arc<dyn RunEventSink>,
    pub reports_dir: &'a Path,
    pub notifications: Option<&'a dyn NotificationSink>,
}

pub fn run_monitor(
    definition: &MonitorDefinition,
    items: Vec<RunItem>,
    config: RunConfig,
    context: MonitorRunContext<'_>,
) -> Result<MonitorRunResult, String> {
    validate_definition(definition)?;
    let summary = run_collection(
        items,
        config,
        context.cancellation,
        context.executor,
        context.events,
    )?;
    fs::create_dir_all(context.reports_dir)
        .map_err(|error| format!("could not create monitor report directory: {error}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
        .as_millis();
    let report_path = context
        .reports_dir
        .join(format!("{}-{timestamp}.json", sanitize_id(&definition.id)));
    fs::write(
        &report_path,
        summary
            .to_json()
            .map_err(|error| format!("could not render monitor report: {error}"))?,
    )
    .map_err(|error| format!("could not write monitor report: {error}"))?;
    prune_reports(
        context.reports_dir,
        &definition.id,
        definition.report_retention,
    )
    .map_err(|error| format!("could not prune monitor reports: {error}"))?;

    let should_notify = if summary.failed() == 0 {
        definition.notify_on_success
    } else {
        definition.notify_on_failure
    };
    let notification_error = if should_notify {
        context.notifications.and_then(|sink| {
            sink.notify(&MonitorNotification {
                monitor_id: definition.id.clone(),
                passed: summary.failed() == 0,
                report_path: report_path.clone(),
            })
            .err()
        })
    } else {
        None
    };

    Ok(MonitorRunResult {
        summary,
        report_path,
        notification_error,
    })
}

pub fn generate_systemd_user_units(
    definition: &MonitorDefinition,
    executable: &Path,
    definition_path: &Path,
) -> Result<(String, String), String> {
    validate_definition(definition)?;
    let service_name = sanitize_id(&definition.id);
    let executable = systemd_escape_path(executable)?;
    let definition_path = systemd_escape_path(definition_path)?;
    let service = format!(
        "[Unit]\nDescription=ApexAPI monitor {}\n\n[Service]\nType=oneshot\nExecStart={} monitor run --definition {}\nNoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=read-only\n\n[Install]\nWantedBy=default.target\n",
        definition.name.replace('\n', " "),
        executable,
        definition_path
    );
    let timer = format!(
        "[Unit]\nDescription=Schedule ApexAPI monitor {}\n\n[Timer]\nOnCalendar={}\nPersistent=true\nUnit=apex-monitor-{}.service\n\n[Install]\nWantedBy=timers.target\n",
        definition.name.replace('\n', " "),
        definition.schedule.replace('\n', " "),
        service_name
    );
    Ok((service, timer))
}

fn validate_definition(definition: &MonitorDefinition) -> Result<(), String> {
    if definition.id.is_empty()
        || !definition
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("monitor id must contain only ASCII letters, digits, '-' or '_'".to_owned());
    }
    if definition.name.trim().is_empty() || definition.schedule.trim().is_empty() {
        return Err("monitor name and schedule must not be empty".to_owned());
    }
    if definition.report_retention == 0 || definition.report_retention > 10_000 {
        return Err("monitor report retention must be between 1 and 10000".to_owned());
    }
    Ok(())
}

fn prune_reports(directory: &Path, monitor_id: &str, retention: usize) -> io::Result<()> {
    let prefix = format!("{}-", sanitize_id(monitor_id));
    let mut reports: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        })
        .collect();
    reports.sort();
    let remove_count = reports.len().saturating_sub(retention);
    for path in reports.into_iter().take(remove_count) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn systemd_escape_path(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| "systemd unit paths must be valid UTF-8".to_owned())?;
    if text.contains(['\n', '\r', '\0']) {
        return Err("systemd unit paths contain invalid control characters".to_owned());
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ItemExecution, RunEvent};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    fn definition() -> MonitorDefinition {
        MonitorDefinition {
            id: "health-check".to_owned(),
            name: "Health Check".to_owned(),
            schedule: "*:0/5".to_owned(),
            workspace: PathBuf::from("/workspace"),
            collection: "health".to_owned(),
            report_retention: 2,
            notify_on_success: false,
            notify_on_failure: true,
        }
    }

    #[test]
    fn headless_monitor_runs_shared_runner_and_notifies() {
        let root = std::env::temp_dir().join(format!("apex-monitor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let notifications = Arc::clone(&notifications);
            move |notification: &MonitorNotification| {
                notifications
                    .lock()
                    .expect("notification lock")
                    .push(notification.clone());
                Ok(())
            }
        };
        let result = run_monitor(
            &definition(),
            vec![RunItem {
                id: "1".to_owned(),
                name: "check".to_owned(),
                iteration_data: BTreeMap::new(),
            }],
            RunConfig::default(),
            MonitorRunContext {
                cancellation: CancellationToken::default(),
                executor: Arc::new(|_: &RunItem, _: &CancellationToken| {
                    Ok(ItemExecution {
                        passed: false,
                        message: "down".to_owned(),
                        duration_ms: 1,
                    })
                }),
                events: Arc::new(|_: RunEvent| {}),
                reports_dir: &root,
                notifications: Some(&sink),
            },
        )
        .expect("monitor runs");
        assert_eq!(result.summary.exit_code, 1);
        assert!(result.report_path.is_file());
        assert_eq!(notifications.lock().expect("notification lock").len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn retention_is_bounded() {
        let root =
            std::env::temp_dir().join(format!("apex-monitor-retention-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        for index in 0..4 {
            fs::write(root.join(format!("health-check-{index}.json")), "{}")
                .expect("write fixture");
        }
        prune_reports(&root, "health-check", 2).expect("prune");
        assert_eq!(fs::read_dir(&root).expect("read root").count(), 2);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn systemd_units_are_hardened_and_deterministic() {
        let (service, timer) = generate_systemd_user_units(
            &definition(),
            Path::new("/usr/bin/apex-cli"),
            Path::new("/home/me/monitor.json"),
        )
        .expect("units");
        assert!(service.contains("NoNewPrivileges=true"));
        assert!(service.contains("ProtectSystem=strict"));
        assert!(timer.contains("OnCalendar=*:0/5"));
        assert!(timer.contains("apex-monitor-health-check.service"));
    }
}
