//! rsrsbar core library: pure building blocks for managing git-rsrs
//! autosync watchers on macOS.

mod atomic;
mod paths;
mod registry;

pub use atomic::write_atomic;
pub use paths::{Paths, PathsError, RepoId, watch_label};
pub use registry::{Registry, RegistryError, RepoEntry, Settings};
