use apex_domain::StableId;
use std::collections::VecDeque;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ResourceIdentity {
    Draft(StableId),
    WorkspaceRequest(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestTabState {
    pub resource: ResourceIdentity,
    pub title: String,
    pub dirty: bool,
    pub pinned: bool,
    pub preview: bool,
}

impl RequestTabState {
    pub fn saved(resource: ResourceIdentity, title: impl Into<String>) -> Self {
        Self {
            resource,
            title: title.into(),
            dirty: false,
            pinned: false,
            preview: false,
        }
    }

    pub fn draft(id: StableId, title: impl Into<String>) -> Self {
        Self {
            resource: ResourceIdentity::Draft(id),
            title: title.into(),
            dirty: true,
            pinned: false,
            preview: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloseTabError {
    InvalidIndex(usize),
    UnsavedChanges { index: usize, title: String },
}

impl Display for CloseTabError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIndex(index) => write!(formatter, "tab index {index} does not exist"),
            Self::UnsavedChanges { title, .. } => {
                write!(formatter, "tab '{title}' has unsaved changes")
            }
        }
    }
}

impl std::error::Error for CloseTabError {}

#[derive(Clone, Debug)]
pub struct WorkspaceSession {
    tabs: Vec<RequestTabState>,
    active_index: Option<usize>,
    recently_closed: VecDeque<RequestTabState>,
    maximum_recently_closed: usize,
}

impl Default for WorkspaceSession {
    fn default() -> Self {
        Self::new(20)
    }
}

impl WorkspaceSession {
    pub fn new(maximum_recently_closed: usize) -> Self {
        Self {
            tabs: Vec::new(),
            active_index: None,
            recently_closed: VecDeque::new(),
            maximum_recently_closed,
        }
    }

    pub fn tabs(&self) -> &[RequestTabState] {
        &self.tabs
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active_index
    }

    pub fn active(&self) -> Option<&RequestTabState> {
        self.active_index.and_then(|index| self.tabs.get(index))
    }

    pub fn activate(&mut self, index: usize) -> Result<(), CloseTabError> {
        if index >= self.tabs.len() {
            return Err(CloseTabError::InvalidIndex(index));
        }
        self.active_index = Some(index);
        Ok(())
    }

    pub fn open(&mut self, mut tab: RequestTabState) -> usize {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|existing| existing.resource == tab.resource)
        {
            self.active_index = Some(index);
            return index;
        }

        if tab.preview
            && let Some(index) = self
                .tabs
                .iter()
                .position(|existing| existing.preview && !existing.dirty && !existing.pinned)
        {
            self.tabs[index] = tab;
            self.active_index = Some(index);
            return index;
        }

        if tab.pinned {
            tab.preview = false;
        }
        self.tabs.push(tab);
        let index = self.tabs.len() - 1;
        self.active_index = Some(index);
        index
    }

    pub fn mark_dirty(&mut self, index: usize, dirty: bool) -> Result<(), CloseTabError> {
        let tab = self
            .tabs
            .get_mut(index)
            .ok_or(CloseTabError::InvalidIndex(index))?;
        tab.dirty = dirty;
        if dirty {
            tab.preview = false;
        }
        Ok(())
    }

    pub fn set_pinned(&mut self, index: usize, pinned: bool) -> Result<(), CloseTabError> {
        let tab = self
            .tabs
            .get_mut(index)
            .ok_or(CloseTabError::InvalidIndex(index))?;
        tab.pinned = pinned;
        if pinned {
            tab.preview = false;
        }
        Ok(())
    }

    pub fn reorder(&mut self, from: usize, to: usize) -> Result<(), CloseTabError> {
        if from >= self.tabs.len() {
            return Err(CloseTabError::InvalidIndex(from));
        }
        if to >= self.tabs.len() {
            return Err(CloseTabError::InvalidIndex(to));
        }
        if from == to {
            return Ok(());
        }
        let active_resource = self.active().map(|tab| tab.resource.clone());
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active_index = active_resource
            .and_then(|resource| self.tabs.iter().position(|tab| tab.resource == resource));
        Ok(())
    }

    pub fn close(&mut self, index: usize) -> Result<RequestTabState, CloseTabError> {
        let tab = self
            .tabs
            .get(index)
            .ok_or(CloseTabError::InvalidIndex(index))?;
        if tab.dirty {
            return Err(CloseTabError::UnsavedChanges {
                index,
                title: tab.title.clone(),
            });
        }
        self.force_close(index)
    }

    pub fn force_close(&mut self, index: usize) -> Result<RequestTabState, CloseTabError> {
        if index >= self.tabs.len() {
            return Err(CloseTabError::InvalidIndex(index));
        }
        let active_resource = self.active().map(|tab| tab.resource.clone());
        let removed = self.tabs.remove(index);
        self.remember_closed(removed.clone());
        self.active_index = if self.tabs.is_empty() {
            None
        } else if active_resource.as_ref() == Some(&removed.resource) {
            Some(index.min(self.tabs.len() - 1))
        } else {
            active_resource
                .and_then(|resource| self.tabs.iter().position(|tab| tab.resource == resource))
        };
        Ok(removed)
    }

    pub fn close_others(&mut self, keep: usize) -> Result<(), CloseTabError> {
        if keep >= self.tabs.len() {
            return Err(CloseTabError::InvalidIndex(keep));
        }
        if let Some((index, tab)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(index, tab)| *index != keep && tab.dirty)
        {
            return Err(CloseTabError::UnsavedChanges {
                index,
                title: tab.title.clone(),
            });
        }
        let kept_resource = self.tabs[keep].resource.clone();
        let removed = self
            .tabs
            .extract_if(.., |tab| tab.resource != kept_resource)
            .collect::<Vec<_>>();
        for tab in removed {
            self.remember_closed(tab);
        }
        self.active_index = Some(0);
        Ok(())
    }

    pub fn close_to_right(&mut self, index: usize) -> Result<(), CloseTabError> {
        if index >= self.tabs.len() {
            return Err(CloseTabError::InvalidIndex(index));
        }
        if let Some((offset, tab)) = self.tabs[index + 1..]
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.dirty)
        {
            return Err(CloseTabError::UnsavedChanges {
                index: index + 1 + offset,
                title: tab.title.clone(),
            });
        }
        let removed = self.tabs.drain(index + 1..).collect::<Vec<_>>();
        for tab in removed {
            self.remember_closed(tab);
        }
        self.active_index = Some(self.active_index.unwrap_or(index).min(index));
        Ok(())
    }

    pub fn reopen_closed(&mut self) -> Option<usize> {
        let tab = self.recently_closed.pop_front()?;
        Some(self.open(tab))
    }

    fn remember_closed(&mut self, tab: RequestTabState) {
        if self.maximum_recently_closed == 0 {
            return;
        }
        self.recently_closed.push_front(tab);
        self.recently_closed.truncate(self.maximum_recently_closed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> StableId {
        StableId::parse(value).expect("valid identifier")
    }

    #[test]
    fn opening_same_resource_activates_existing_tab() {
        let mut session = WorkspaceSession::default();
        let resource = ResourceIdentity::WorkspaceRequest(PathBuf::from("users/get.request.toml"));
        assert_eq!(
            session.open(RequestTabState::saved(resource.clone(), "Get user")),
            0
        );
        assert_eq!(session.open(RequestTabState::saved(resource, "Renamed")), 0);
        assert_eq!(session.tabs().len(), 1);
        assert_eq!(
            session.active().map(|tab| tab.title.as_str()),
            Some("Get user")
        );
    }

    #[test]
    fn dirty_preview_becomes_permanent_and_cannot_close_silently() {
        let mut session = WorkspaceSession::default();
        let mut tab = RequestTabState::saved(
            ResourceIdentity::WorkspaceRequest(PathBuf::from("users/get.request.toml")),
            "Get user",
        );
        tab.preview = true;
        let index = session.open(tab);
        session.mark_dirty(index, true).expect("marks dirty");
        assert!(!session.tabs()[index].preview);
        assert!(matches!(
            session.close(index),
            Err(CloseTabError::UnsavedChanges { .. })
        ));
    }

    #[test]
    fn preview_reuses_only_clean_unpinned_preview_tab() {
        let mut session = WorkspaceSession::default();
        let mut first = RequestTabState::saved(
            ResourceIdentity::WorkspaceRequest(PathBuf::from("users/a.request.toml")),
            "A",
        );
        first.preview = true;
        session.open(first);
        let mut second = RequestTabState::saved(
            ResourceIdentity::WorkspaceRequest(PathBuf::from("users/b.request.toml")),
            "B",
        );
        second.preview = true;
        let index = session.open(second);
        assert_eq!(index, 0);
        assert_eq!(session.tabs().len(), 1);
        assert_eq!(session.tabs()[0].title, "B");
    }

    #[test]
    fn close_right_is_atomic_when_a_dirty_tab_exists() {
        let mut session = WorkspaceSession::default();
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("a")),
            "A",
        ));
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("b")),
            "B",
        ));
        session.open(RequestTabState::draft(id("c"), "C"));
        let error = session
            .close_to_right(0)
            .expect_err("dirty tab blocks close");
        assert!(matches!(
            error,
            CloseTabError::UnsavedChanges { index: 2, .. }
        ));
        assert_eq!(session.tabs().len(), 3);
    }

    #[test]
    fn closed_tab_can_be_reopened_with_identity_intact() {
        let mut session = WorkspaceSession::default();
        let resource = ResourceIdentity::WorkspaceRequest(PathBuf::from("users/get.request.toml"));
        session.open(RequestTabState::saved(resource.clone(), "Get user"));
        session.close(0).expect("closes");
        let index = session.reopen_closed().expect("reopens");
        assert_eq!(index, 0);
        assert_eq!(session.tabs()[0].resource, resource);
    }

    #[test]
    fn closing_inactive_tab_preserves_active_resource() {
        let mut session = WorkspaceSession::default();
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("a")),
            "A",
        ));
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("b")),
            "B",
        ));
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("c")),
            "C",
        ));
        session.activate(1).expect("activates B");

        session.close(0).expect("closes inactive A");

        assert_eq!(session.active().map(|tab| tab.title.as_str()), Some("B"));
    }

    #[test]
    fn closing_active_tab_selects_nearest_remaining_tab() {
        let mut session = WorkspaceSession::default();
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("a")),
            "A",
        ));
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("b")),
            "B",
        ));
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("c")),
            "C",
        ));
        session.activate(1).expect("activates B");

        session.close(1).expect("closes active B");

        assert_eq!(session.active().map(|tab| tab.title.as_str()), Some("C"));
    }

    #[test]
    fn reorder_preserves_active_resource() {
        let mut session = WorkspaceSession::default();
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("a")),
            "A",
        ));
        session.open(RequestTabState::saved(
            ResourceIdentity::Draft(id("b")),
            "B",
        ));
        session.reorder(0, 1).expect("reorders");
        assert_eq!(session.active().map(|tab| tab.title.as_str()), Some("B"));
    }
}
