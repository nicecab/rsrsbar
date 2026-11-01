//! rsrsbar core library: pure building blocks for managing git-rsrs
//! autosync watchers on macOS.

mod atomic;
mod paths;
mod registry;
mod service;
mod trust;

pub use atomic::write_atomic;
pub use paths::{Paths, PathsError, RepoId, watch_label};
pub use registry::{Registry, RegistryError, RepoEntry, Settings};
pub use service::{GIT_RSRS, ServiceError, git_rsrs_config, launch_agent_plist};
pub use trust::{Phase, hooks_digest, present_hooks};
