use apex_workspace::{
    WorkspaceChange, WorkspaceRepository, WorkspaceRequestEntry, reconcile_workspace_change,
};
use std::thread;
use std::time::Duration;

const MONITOR_CHANNEL_CAPACITY: usize = 64;
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) enum WorkspaceMonitorMessage {
    Update {
        change: WorkspaceChange,
        requests: Option<Result<Vec<WorkspaceRequestEntry>, String>>,
    },
    Failed(String),
}

pub(crate) fn start_workspace_monitor(
    repository: WorkspaceRepository,
) -> Result<async_channel::Receiver<WorkspaceMonitorMessage>, String> {
    let (sender, receiver) = async_channel::bounded(MONITOR_CHANNEL_CAPACITY);
    thread::Builder::new()
        .name("apex-workspace-monitor".to_owned())
        .spawn(move || {
            let watcher = match repository.watch() {
                Ok(watcher) => watcher,
                Err(error) => {
                    let _ =
                        sender.send_blocking(WorkspaceMonitorMessage::Failed(error.to_string()));
                    return;
                }
            };

            loop {
                if sender.is_closed() {
                    break;
                }
                let change = match watcher.recv_timeout(WATCH_POLL_INTERVAL) {
                    Ok(Some(change)) => change,
                    Ok(None) => continue,
                    Err(error) => {
                        let _ = sender
                            .send_blocking(WorkspaceMonitorMessage::Failed(error.to_string()));
                        break;
                    }
                };
                let refresh_tree = reconcile_workspace_change(None, false, &change).refresh_tree;
                let requests = refresh_tree.then(|| {
                    repository
                        .list_requests()
                        .map_err(|error| error.to_string())
                });
                if sender
                    .send_blocking(WorkspaceMonitorMessage::Update { change, requests })
                    .is_err()
                {
                    break;
                }
            }
        })
        .map_err(|error| format!("failed to spawn workspace monitor: {error}"))?;
    Ok(receiver)
}
