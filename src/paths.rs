//! Repository ids, the support-directory layout and launchd names.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_ID_LEN: usize = 63;
const LABEL_PREFIX: &str = "com.nicecab.rsrsbar.watch.";
/// The launchd label of the menu bar app's job.
pub const APP_LABEL: &str = "com.nicecab.rsrsbar";
/// The launchd label of the independent `check-heartbeat` job.
pub const HEARTBEAT_LABEL: &str = "com.nicecab.rsrsbar.heartbeat";

/// Errors from building repository ids.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// The string does not match `^[a-z0-9][a-z0-9-]{0,62}$`.
    #[error("invalid repository id {0:?}: use 1-63 of a-z, 0-9 and '-', not starting with '-'")]
    InvalidId(String),
    /// No valid id can be derived from the repository path.
    #[error("cannot derive a repository id from {}", .0.display())]
    NoSuggestion(PathBuf),
}

/// A validated repository id, used in file names and launchd labels.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoId(String);

impl RepoId {
    /// Accepts exactly `^[a-z0-9][a-z0-9-]{0,62}$`.
    pub fn new(s: &str) -> Result<RepoId, PathsError> {
        let is_id_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        let valid = s.len() <= MAX_ID_LEN
            && s.chars().all(is_id_char)
            && s.chars().next().is_some_and(|c| c != '-');
        if valid {
            Ok(RepoId(s.to_owned()))
        } else {
            Err(PathsError::InvalidId(s.to_owned()))
        }
    }

    /// Derives an id from the repository's basename: lowercased, every run of
    /// characters outside `[a-z0-9]` replaced by one `-`, trimmed of `-`.
    pub fn suggest(repo: &Path) -> Result<RepoId, PathsError> {
        let no_suggestion = || PathsError::NoSuggestion(repo.to_path_buf());
        let base = repo
            .file_name()
            .ok_or_else(no_suggestion)?
            .to_string_lossy();
        let mut slug = String::with_capacity(base.len());
        for c in base.chars().flat_map(char::to_lowercase) {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                slug.push(c);
            } else if !slug.ends_with('-') {
                slug.push('-');
            }
        }
        RepoId::new(slug.trim_matches('-')).map_err(|_| no_suggestion())
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoId {
    type Error = PathsError;

    fn try_from(s: String) -> Result<RepoId, PathsError> {
        RepoId::new(&s)
    }
}

impl From<RepoId> for String {
    fn from(id: RepoId) -> String {
        id.0
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The launchd label of a repository's watcher job.
pub fn watch_label(id: &RepoId) -> String {
    format!("{LABEL_PREFIX}{id}")
}

/// Every file location rsrsbar uses, rooted at an explicit home directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new(home: PathBuf) -> Paths {
        Paths { home }
    }

    /// `<home>/Library/Application Support/rsrsbar`.
    pub fn support_dir(&self) -> PathBuf {
        self.home.join("Library/Application Support/rsrsbar")
    }

    pub fn registry(&self) -> PathBuf {
        self.support_dir().join("registry.toml")
    }

    pub fn state(&self) -> PathBuf {
        self.support_dir().join("state.json")
    }

    /// The app's liveness file, written every poll.
    pub fn heartbeat(&self) -> PathBuf {
        self.support_dir().join("heartbeat.json")
    }

    /// `check-heartbeat`'s own state.
    pub fn heartbeat_state(&self) -> PathBuf {
        self.support_dir().join("heartbeat-state.json")
    }

    /// Held (`flock`) by the running menu bar app: one instance only.
    pub fn app_lock(&self) -> PathBuf {
        self.support_dir().join("app.lock")
    }

    /// The menu bar app's LaunchAgent plist.
    pub fn app_agent(&self) -> PathBuf {
        self.agent(APP_LABEL)
    }

    /// The heartbeat job's LaunchAgent plist.
    pub fn heartbeat_agent(&self) -> PathBuf {
        self.agent(HEARTBEAT_LABEL)
    }

    /// `<home>/Library/Logs/<label>.log`, a job's standard output.
    pub fn job_log(&self, label: &str) -> PathBuf {
        self.logs_dir().join(format!("{label}.log"))
    }

    /// `<home>/Library/Logs/<label>.error.log`, a job's standard error.
    pub fn job_error_log(&self, label: &str) -> PathBuf {
        self.logs_dir().join(format!("{label}.error.log"))
    }

    /// Focus / Do Not Disturb assertions (an undocumented macOS file).
    pub fn focus_assertions(&self) -> PathBuf {
        self.home.join("Library/DoNotDisturb/DB/Assertions.json")
    }

    fn agent(&self, label: &str) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    }

    pub fn repo_dir(&self, id: &RepoId) -> PathBuf {
        self.support_dir().join("repos").join(id.as_str())
    }

    pub fn git_rsrs_config(&self, id: &RepoId) -> PathBuf {
        self.repo_dir(id).join("git-rsrs.toml")
    }

    pub fn status_file(&self, id: &RepoId) -> PathBuf {
        self.repo_dir(id).join("status.json")
    }

    /// The snapshot of `.autosync/` taken by the last allow, which
    /// `run-hook` executes.
    pub fn allowed_hooks_dir(&self, id: &RepoId) -> PathBuf {
        self.repo_dir(id).join("allowed-hooks")
    }

    pub fn launch_agent(&self, id: &RepoId) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", watch_label(id)))
    }

    pub fn stdout_log(&self, id: &RepoId) -> PathBuf {
        self.logs_dir().join(format!("{}.log", watch_label(id)))
    }

    pub fn stderr_log(&self, id: &RepoId) -> PathBuf {
        self.logs_dir()
            .join(format!("{}.error.log", watch_label(id)))
    }

    pub fn mise_shims(&self) -> PathBuf {
        self.home.join(".local/share/mise/shims")
    }

    fn logs_dir(&self) -> PathBuf {
        self.home.join("Library/Logs")
    }
}
