//! rsrsbar core library: pure building blocks for managing git-rsrs
//! autosync watchers on macOS.

mod atomic;
mod paths;

pub use atomic::write_atomic;
pub use paths::{Paths, PathsError, RepoId, watch_label};
