//! rsrsbar core library: pure building blocks for managing git-rsrs
//! autosync watchers on macOS.

pub mod app;
mod atomic;
mod bundle;
mod child;
pub mod commands;
mod episode;
mod git;
mod gitrsrs;
mod health;
pub mod heartbeat;
mod launchd;
mod observe;
mod paths;
mod registry;
mod service;
mod status;
mod system;
mod trust;

pub use app::Authorization;
pub use atomic::write_atomic;
pub use bundle::{bundle_root, info_plist};
pub use child::output_within;
pub use episode::{EpisodeKey, Notice, RecoveryPoint, RepoMemory, State, StateError, advance};
pub use git::{GitError, RepoProbe, current_branch, probe};
pub use gitrsrs::{
    GitRsrsError, MIN_GIT_RSRS, SyncRun, check_version, parse_version, sync_deadline, sync_once,
};
pub use health::{
    Color, Health, JobState, Notify, Observation, Row, classify, failure_run, unhealthy_in_window,
};
pub use launchd::{JobInfo, Launchctl, Launchd, LaunchdError, Recording, parse_print};
pub use observe::{ObserveError, RepoSnapshot, observe};
pub use paths::{APP_LABEL, HEARTBEAT_LABEL, Paths, PathsError, RepoId, watch_label};
pub use registry::{AppSettings, Registry, RegistryError, RepoEntry, Settings};
pub use service::{
    GIT_RSRS, ServiceError, app_plist, git_rsrs_config, heartbeat_plist, launch_agent_plist,
};
pub use status::{
    AttemptHealth, HookFailure, Outcome, RecentAttempt, StatusFile, StatusRead, read_status,
};
pub use system::{awake_since, parse_etime, parse_focus, parse_kern_time, process_age};
pub use trust::{
    Phase, hooks_digest, present_hooks, snapshot_digest, snapshot_hooks, snapshot_hooks_matching,
};
