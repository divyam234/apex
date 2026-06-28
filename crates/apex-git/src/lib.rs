#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum WorkspaceCapability {
    Scripts,
    Plugins,
    Mocks,
    Monitors,
    CustomCertificates,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum TrustState {
    Untrusted,
    Trusted,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct WorkspaceTrust {
    pub state: TrustState,
}
impl Default for WorkspaceTrust {
    fn default() -> Self {
        Self {
            state: TrustState::Untrusted,
        }
    }
}
impl WorkspaceTrust {
    pub fn authorize(&self, capability: WorkspaceCapability) -> Result<(), String> {
        if self.state == TrustState::Trusted {
            Ok(())
        } else {
            Err(format!("untrusted workspace cannot use {capability:?}"))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatus {
    pub available: bool,
    pub branch: Option<String>,
    pub entries: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct GitRepository {
    root: PathBuf,
}
impl GitRepository {
    pub fn discover(root: impl AsRef<Path>) -> Result<Option<Self>, String> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|e| format!("invalid workspace: {e}"))?;
        let out = Command::new("git")
            .args(["-C"])
            .arg(&root)
            .args(["rev-parse", "--show-toplevel"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let repo = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                Ok(Some(Self {
                    root: PathBuf::from(repo),
                }))
            }
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("could not invoke git: {e}")),
        }
    }
    pub fn status(&self) -> Result<GitStatus, String> {
        let branch = self.run(&["branch", "--show-current"])?;
        let porcelain = self.run(&["status", "--porcelain=v1"])?;
        Ok(GitStatus {
            available: true,
            branch: (!branch.trim().is_empty()).then(|| branch.trim().to_owned()),
            entries: porcelain.lines().map(str::to_owned).collect(),
        })
    }
    pub fn diff(&self, staged: bool) -> Result<String, String> {
        if staged {
            self.run(&["diff", "--cached", "--no-ext-diff"])
        } else {
            self.run(&["diff", "--no-ext-diff"])
        }
    }
    pub fn stage(&self, path: &Path) -> Result<(), String> {
        let path = safe_relative(path)?;
        self.run_os(&["add", "--"], Some(&path)).map(|_| ())
    }
    pub fn commit(&self, message: &str) -> Result<String, String> {
        if message.trim().is_empty() || message.contains(['\n', '\r', '\0']) {
            return Err("commit message must be a non-empty single line".into());
        }
        self.run(&["commit", "-m", message])
    }
    pub fn switch(&self, branch: &str, allow_dirty: bool) -> Result<(), String> {
        if branch.is_empty() || branch.starts_with('-') || branch.contains(['\n', '\r', '\0']) {
            return Err("invalid branch name".into());
        }
        if !allow_dirty && !self.status()?.entries.is_empty() {
            return Err("cannot switch branches with uncommitted changes".into());
        }
        self.run(&["switch", branch]).map(|_| ())
    }
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .map_err(|e| format!("could not invoke git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }
    fn run_os(&self, args: &[&str], path: Option<&Path>) -> Result<String, String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.root).args(args);
        if let Some(path) = path {
            cmd.arg(path);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("could not invoke git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }
}
fn safe_relative(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        Err("Git path must stay inside the repository".into())
    } else {
        Ok(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn untrusted_workspaces_deny_executable_capabilities() {
        let trust = WorkspaceTrust::default();
        for cap in [
            WorkspaceCapability::Scripts,
            WorkspaceCapability::Plugins,
            WorkspaceCapability::Mocks,
            WorkspaceCapability::Monitors,
            WorkspaceCapability::CustomCertificates,
        ] {
            assert!(trust.authorize(cap).is_err());
        }
        assert!(
            WorkspaceTrust {
                state: TrustState::Trusted
            }
            .authorize(WorkspaceCapability::Scripts)
            .is_ok()
        );
    }
    #[test]
    fn ordinary_non_git_workspace_is_supported() {
        let root = std::env::temp_dir().join(format!("apex-no-git-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert!(GitRepository::discover(&root).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn git_operations_are_shell_free_and_guard_dirty_switch() {
        let root = std::env::temp_dir().join(format!("apex-git-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        run(&["init"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        fs::write(root.join("a.txt"), "a").unwrap();
        run(&["add", "a.txt"]);
        run(&["-c", "commit.gpgsign=false", "commit", "-m", "initial"]);
        let repo = GitRepository::discover(&root).unwrap().unwrap();
        fs::write(root.join("a.txt"), "b").unwrap();
        assert!(!repo.status().unwrap().entries.is_empty());
        assert!(repo.switch("main", false).is_err());
        assert!(repo.stage(Path::new("../outside")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
