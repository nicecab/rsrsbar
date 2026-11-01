//! Lenient, forward-compatible reader of git-rsrs status files.

use crate::atomic::read_optional;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer};

const STATUS_VERSION: u32 = 1;

/// The latest attempt's outcome. Words this version does not know (from a
/// newer git-rsrs) parse as `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Synced,
    Noop,
    BranchSkipped,
    Deferred,
    ValidationRejected,
    CheckFailed,
    ManualIntervention,
    LockTimeout,
    Offline,
    TimedOut,
    AuthenticationFailed,
    Failed,
    #[serde(other)]
    Unknown,
}

impl Outcome {
    /// The status-file word (`"unknown"` for words this version does not know).
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Synced => "synced",
            Outcome::Noop => "noop",
            Outcome::BranchSkipped => "branch_skipped",
            Outcome::Deferred => "deferred",
            Outcome::ValidationRejected => "validation_rejected",
            Outcome::CheckFailed => "check_failed",
            Outcome::ManualIntervention => "manual_intervention",
            Outcome::LockTimeout => "lock_timeout",
            Outcome::Offline => "offline",
            Outcome::TimedOut => "timed_out",
            Outcome::AuthenticationFailed => "authentication_failed",
            Outcome::Failed => "failed",
            Outcome::Unknown => "unknown",
        }
    }
}

/// A hook that failed or timed out during the latest attempt.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct HookFailure {
    pub phase: String,
    pub timed_out: bool,
    pub message: String,
}

/// One counted attempt in `StatusFile::recent` (newest first).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RecentAttempt {
    pub seq: u64,
    pub healthy: bool,
}

/// A parsed status file. Unknown fields are ignored; timestamps are RFC 3339.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct StatusFile {
    pub version: u32,
    #[serde(deserialize_with = "rfc3339")]
    pub last_attempt_at: SystemTime,
    pub last_outcome: Outcome,
    #[serde(deserialize_with = "optional_rfc3339")]
    pub last_success_at: Option<SystemTime>,
    pub message: Option<String>,
    pub pid: u32,
    pub git_rsrs_version: String,
    #[serde(default)]
    pub hook_failures: Vec<HookFailure>,
    /// When git-rsrs started its counters; an identity compared for equality,
    /// so it is kept as the raw string.
    pub counters_since: String,
    pub attempt_seq: u64,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    #[serde(default)]
    pub recent: Vec<RecentAttempt>,
}

/// The result of reading a status file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusRead {
    Missing,
    Unreadable(String),
    Present(StatusFile),
}

/// How git-rsrs counts the latest attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptHealth {
    Healthy,
    Unhealthy,
    /// Expected or transient states, judged by time instead.
    Uncounted,
}

impl StatusFile {
    /// Same rule as git-rsrs `status::health`; an unknown outcome is uncounted.
    pub fn latest_health(&self) -> AttemptHealth {
        match self.last_outcome {
            Outcome::BranchSkipped
            | Outcome::Offline
            | Outcome::Deferred
            | Outcome::LockTimeout
            | Outcome::ValidationRejected
            | Outcome::Unknown => AttemptHealth::Uncounted,
            Outcome::Synced | Outcome::Noop if self.hook_failures.is_empty() => {
                AttemptHealth::Healthy
            }
            _ => AttemptHealth::Unhealthy,
        }
    }

    /// `message` when present and non-empty, else the first hook failure's
    /// message (git-rsrs leaves `message` null for `check_failed`).
    pub fn display_message(&self) -> Option<&str> {
        self.message
            .as_deref()
            .filter(|m| !m.is_empty())
            .or_else(|| self.hook_failures.first().map(|f| f.message.as_str()))
    }
}

/// Reads a status file. Only a missing file is `Missing`; any other I/O
/// error, invalid JSON, a missing or mistyped field, or a version other
/// than 1 is `Unreadable`. The version is checked first, so a file from an
/// incompatible future format reports its version, not a field error.
pub fn read_status(path: &Path) -> StatusRead {
    let text = match read_optional(path) {
        Ok(Some(text)) => text,
        Ok(None) => return StatusRead::Missing,
        Err(e) => return StatusRead::Unreadable(e.to_string()),
    };
    match parse(&text) {
        Ok(status) => StatusRead::Present(status),
        Err(reason) => StatusRead::Unreadable(reason),
    }
}

fn parse(text: &str) -> Result<StatusFile, String> {
    #[derive(Deserialize)]
    struct VersionProbe {
        version: u32,
    }
    let probe: VersionProbe = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if probe.version != STATUS_VERSION {
        return Err(format!("unsupported status version {}", probe.version));
    }
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn rfc3339<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
    let raw = String::deserialize(d)?;
    humantime::parse_rfc3339(&raw).map_err(serde::de::Error::custom)
}

fn optional_rfc3339<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
    Option::<String>::deserialize(d)?
        .map(|raw| humantime::parse_rfc3339(&raw).map_err(serde::de::Error::custom))
        .transpose()
}
