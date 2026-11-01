//! rsrsbar core library: pure building blocks for managing git-rsrs
//! autosync watchers on macOS.

mod atomic;
mod episode;
mod health;
mod paths;
mod registry;
mod service;
mod status;
mod trust;

pub use atomic::write_atomic;
pub use episode::{EpisodeKey, Notice, RecoveryPoint, RepoMemory, State, StateError, advance};
pub use health::{
    Color, Health, JobState, Notify, Observation, Row, classify, failure_run, unhealthy_in_window,
};
pub use paths::{Paths, PathsError, RepoId, watch_label};
pub use registry::{Registry, RegistryError, RepoEntry, Settings};
pub use service::{GIT_RSRS, ServiceError, git_rsrs_config, launch_agent_plist};
pub use status::{
    AttemptHealth, HookFailure, Outcome, RecentAttempt, StatusFile, StatusRead, read_status,
};
pub use trust::{Phase, hooks_digest, present_hooks};
