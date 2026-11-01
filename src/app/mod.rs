//! The resident menu bar app. Everything that decides is plain library
//! code; [`notify`] is the macOS-only bridge to `UNUserNotificationCenter`.

use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
pub mod notify;

/// Whether the app may show notifications, as macOS reports it. `Unknown`
/// when the answer did not arrive in time or the process has no bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    Authorized,
    Denied,
    NotDetermined,
    Unknown,
}

impl Authorization {
    /// The on-disk (`snake_case`) word for this value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::Denied => "denied",
            Self::NotDetermined => "not_determined",
            Self::Unknown => "unknown",
        }
    }
}
