use crate::WorkspaceRepository;
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::ffi::OsStr;
use std::fmt::{Display, Formatter};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

pub const DEFAULT_WORKSPACE_WATCH_CAPACITY: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceChangeKind {
    Created,
    Modified,
    Removed,
    Renamed,
    RescanRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceResourceKind {
    Manifest,
    Variables,
    Environment,
    Request,
    Collection,
    Schema,
    Grpc,
    Mock,
    Profile,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceChangedPath {
    pub relative_path: PathBuf,
    pub resource: WorkspaceResourceKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceChange {
    pub kind: WorkspaceChangeKind,
    pub paths: Vec<WorkspaceChangedPath>,
}

pub struct WorkspaceWatcher {
    root: PathBuf,
    receiver: Receiver<notify::Result<Event>>,
    overflowed: Arc<AtomicBool>,
    _watcher: RecommendedWatcher,
}

impl WorkspaceRepository {
    pub fn watch(&self) -> Result<WorkspaceWatcher, WorkspaceWatchError> {
        WorkspaceWatcher::new(self.root())
    }
}

impl WorkspaceWatcher {
    fn new(root: &Path) -> Result<Self, WorkspaceWatchError> {
        let root = root.canonicalize().map_err(WorkspaceWatchError::Io)?;
        if !root.is_dir() {
            return Err(WorkspaceWatchError::NotDirectory(root));
        }

        let (sender, receiver) = mpsc::sync_channel(DEFAULT_WORKSPACE_WATCH_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));
        let handler_overflowed = Arc::clone(&overflowed);
        let mut watcher = notify::recommended_watcher(move |event| {
            try_enqueue_event(&sender, &handler_overflowed, event);
        })
        .map_err(WorkspaceWatchError::Backend)?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(WorkspaceWatchError::Backend)?;

        Ok(Self {
            root,
            receiver,
            overflowed,
            _watcher: watcher,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn try_recv(&self) -> Result<Option<WorkspaceChange>, WorkspaceWatchError> {
        loop {
            if let Some(change) = take_overflow(&self.overflowed) {
                return Ok(Some(change));
            }
            match self.receiver.try_recv() {
                Ok(result) => {
                    if let Some(change) = take_overflow(&self.overflowed) {
                        return Ok(Some(change));
                    }
                    if let Some(change) = self.normalize_result(result)? {
                        return Ok(Some(change));
                    }
                }
                Err(TryRecvError::Empty) => return Ok(take_overflow(&self.overflowed)),
                Err(TryRecvError::Disconnected) => {
                    return Err(WorkspaceWatchError::Disconnected);
                }
            }
        }
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<WorkspaceChange>, WorkspaceWatchError> {
        if timeout.is_zero() {
            return self.try_recv();
        }

        let started = Instant::now();
        let mut remaining = timeout;
        loop {
            if let Some(change) = take_overflow(&self.overflowed) {
                return Ok(Some(change));
            }
            match self.receiver.recv_timeout(remaining) {
                Ok(result) => {
                    if let Some(change) = take_overflow(&self.overflowed) {
                        return Ok(Some(change));
                    }
                    if let Some(change) = self.normalize_result(result)? {
                        return Ok(Some(change));
                    }
                    remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Ok(take_overflow(&self.overflowed));
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Ok(take_overflow(&self.overflowed)),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(WorkspaceWatchError::Disconnected);
                }
            }
        }
    }

    fn normalize_result(
        &self,
        result: notify::Result<Event>,
    ) -> Result<Option<WorkspaceChange>, WorkspaceWatchError> {
        result
            .map(|event| normalize_event(&self.root, event))
            .map_err(WorkspaceWatchError::Backend)
    }
}

fn try_enqueue_event(
    sender: &SyncSender<notify::Result<Event>>,
    overflowed: &AtomicBool,
    event: notify::Result<Event>,
) {
    match sender.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => overflowed.store(true, Ordering::Release),
        Err(TrySendError::Disconnected(_)) => {}
    }
}

fn take_overflow(overflowed: &AtomicBool) -> Option<WorkspaceChange> {
    overflowed.swap(false, Ordering::AcqRel).then(rescan_change)
}

fn rescan_change() -> WorkspaceChange {
    WorkspaceChange {
        kind: WorkspaceChangeKind::RescanRequired,
        paths: Vec::new(),
    }
}

fn normalize_event(root: &Path, event: Event) -> Option<WorkspaceChange> {
    if event.need_rescan() {
        return Some(rescan_change());
    }

    let kind = match event.kind {
        EventKind::Access(_) => return None,
        EventKind::Create(_) => WorkspaceChangeKind::Created,
        EventKind::Modify(ModifyKind::Name(_)) => WorkspaceChangeKind::Renamed,
        EventKind::Modify(_) => WorkspaceChangeKind::Modified,
        EventKind::Remove(_) => WorkspaceChangeKind::Removed,
        EventKind::Any | EventKind::Other => WorkspaceChangeKind::Modified,
    };

    let paths = event
        .paths
        .into_iter()
        .filter_map(|path| normalize_path(root, path))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        None
    } else {
        Some(WorkspaceChange { kind, paths })
    }
}

fn normalize_path(root: &Path, path: PathBuf) -> Option<WorkspaceChangedPath> {
    let absolute = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let relative_path = absolute.strip_prefix(root).ok()?.to_owned();
    if relative_path.as_os_str().is_empty()
        || relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || is_ignored_path(&relative_path)
    {
        return None;
    }

    Some(WorkspaceChangedPath {
        resource: classify_resource(&relative_path),
        relative_path,
    })
}

fn is_ignored_path(path: &Path) -> bool {
    if path.components().any(|component| {
        matches!(
            component,
            Component::Normal(value)
                if value == OsStr::new(".git") || value == OsStr::new(".apex")
        )
    }) {
        return true;
    }

    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    file_name == ".DS_Store"
        || file_name.starts_with(".#")
        || (file_name.starts_with('#') && file_name.ends_with('#'))
        || file_name.ends_with('~')
        || file_name.ends_with(".swp")
        || file_name.ends_with(".swo")
        || file_name.ends_with(".swx")
        || file_name.ends_with(".tmp")
}

fn classify_resource(path: &Path) -> WorkspaceResourceKind {
    if path == Path::new("apex.toml") {
        return WorkspaceResourceKind::Manifest;
    }
    if path == Path::new("variables.toml") {
        return WorkspaceResourceKind::Variables;
    }

    let Some(first) = path.components().next() else {
        return WorkspaceResourceKind::Other;
    };
    let Component::Normal(first) = first else {
        return WorkspaceResourceKind::Other;
    };
    if first == OsStr::new("environments") {
        WorkspaceResourceKind::Environment
    } else if first == OsStr::new("collections") {
        if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(".request.toml"))
        {
            WorkspaceResourceKind::Request
        } else {
            WorkspaceResourceKind::Collection
        }
    } else if first == OsStr::new("schemas") {
        WorkspaceResourceKind::Schema
    } else if first == OsStr::new("grpc") {
        WorkspaceResourceKind::Grpc
    } else if first == OsStr::new("mocks") {
        WorkspaceResourceKind::Mock
    } else if first == OsStr::new("profiles") {
        WorkspaceResourceKind::Profile
    } else {
        WorkspaceResourceKind::Other
    }
}

#[derive(Debug)]
pub enum WorkspaceWatchError {
    Io(std::io::Error),
    Backend(notify::Error),
    NotDirectory(PathBuf),
    Disconnected,
}

impl Display for WorkspaceWatchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "workspace watcher I/O failed: {error}"),
            Self::Backend(error) => write!(formatter, "workspace watcher backend failed: {error}"),
            Self::NotDirectory(path) => write!(
                formatter,
                "workspace watcher root is not a directory: {}",
                path.display()
            ),
            Self::Disconnected => write!(formatter, "workspace watcher event channel disconnected"),
        }
    }
}

impl std::error::Error for WorkspaceWatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Backend(error) => Some(error),
            Self::NotDirectory(_) | Self::Disconnected => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, Flag, RenameMode};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn normalizes_request_create_events_to_relative_paths() {
        let root = Path::new("/workspace/demo");
        let event = Event::new(EventKind::Create(CreateKind::File))
            .add_path(root.join("collections/users/get.request.toml"));

        let change = normalize_event(root, event).expect("relevant event");
        assert_eq!(change.kind, WorkspaceChangeKind::Created);
        assert_eq!(
            change.paths,
            vec![WorkspaceChangedPath {
                relative_path: PathBuf::from("collections/users/get.request.toml"),
                resource: WorkspaceResourceKind::Request,
            }]
        );
    }

    #[test]
    fn rename_events_preserve_source_and_destination_order() {
        let root = Path::new("/workspace/demo");
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(root.join("collections/users/old.request.toml"))
            .add_path(root.join("collections/users/new.request.toml"));

        let change = normalize_event(root, event).expect("relevant event");
        assert_eq!(change.kind, WorkspaceChangeKind::Renamed);
        assert_eq!(
            change
                .paths
                .iter()
                .map(|path| path.relative_path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new("collections/users/old.request.toml"),
                Path::new("collections/users/new.request.toml"),
            ]
        );
    }

    #[test]
    fn ignores_access_internal_and_temporary_events() {
        let root = Path::new("/workspace/demo");
        let access = Event::new(EventKind::Access(notify::event::AccessKind::Read))
            .add_path(root.join("apex.toml"));
        assert_eq!(normalize_event(root, access), None);

        for path in [
            ".git/index",
            ".apex/history.sqlite",
            "../outside.request.toml",
            "collections/users/.get.request.toml.123.tmp",
            "collections/users/get.request.toml.swp",
        ] {
            let event = Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join(path));
            assert_eq!(normalize_event(root, event), None, "path: {path}");
        }
    }

    #[test]
    fn queue_overflow_requires_a_rescan() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let overflowed = AtomicBool::new(false);
        let first = Event::new(EventKind::Modify(ModifyKind::Any));
        let second = Event::new(EventKind::Modify(ModifyKind::Any));

        try_enqueue_event(&sender, &overflowed, Ok(first.clone()));
        try_enqueue_event(&sender, &overflowed, Ok(second));

        let queued = receiver
            .try_recv()
            .expect("first queued event")
            .expect("successful watcher event");
        assert_eq!(queued, first);
        assert_eq!(take_overflow(&overflowed), Some(rescan_change()));
        assert_eq!(take_overflow(&overflowed), None);
    }

    #[test]
    fn rescan_events_are_never_dropped() {
        let event = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(
            normalize_event(Path::new("/workspace/demo"), event),
            Some(WorkspaceChange {
                kind: WorkspaceChangeKind::RescanRequired,
                paths: Vec::new(),
            })
        );
    }

    #[test]
    fn classifies_workspace_resource_families() {
        let cases = [
            ("apex.toml", WorkspaceResourceKind::Manifest),
            ("variables.toml", WorkspaceResourceKind::Variables),
            (
                "environments/development.toml",
                WorkspaceResourceKind::Environment,
            ),
            (
                "collections/users/collection.toml",
                WorkspaceResourceKind::Collection,
            ),
            ("schemas/users.json", WorkspaceResourceKind::Schema),
            ("grpc/users.proto", WorkspaceResourceKind::Grpc),
            ("mocks/users.toml", WorkspaceResourceKind::Mock),
            ("profiles/team.toml", WorkspaceResourceKind::Profile),
            ("README.md", WorkspaceResourceKind::Other),
        ];
        for (path, expected) in cases {
            assert_eq!(classify_resource(Path::new(path)), expected, "path: {path}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn observes_a_real_request_file_change() {
        let root = temporary_directory("watcher");
        let request_directory = root.join("collections/users");
        fs::create_dir_all(&request_directory).expect("create fixture directory");
        let repository = WorkspaceRepository::new(&root).expect("repository");
        let watcher = repository.watch().expect("start watcher");
        let request_path = request_directory.join("get.request.toml");
        fs::write(&request_path, "schema_version = 1\n").expect("write request fixture");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = None;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(change) = watcher.recv_timeout(remaining).expect("receive change") else {
                break;
            };
            if change.paths.iter().any(|path| {
                path.relative_path == Path::new("collections/users/get.request.toml")
                    && path.resource == WorkspaceResourceKind::Request
            }) {
                observed = Some(change);
                break;
            }
        }

        let change = observed.expect("request file event");
        assert!(matches!(
            change.kind,
            WorkspaceChangeKind::Created | WorkspaceChangeKind::Modified
        ));
        drop(watcher);
        fs::remove_dir_all(root).expect("remove fixture directory");
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "apex-workspace-{name}-{}-{nonce}-{sequence}",
            std::process::id()
        ))
    }
}
