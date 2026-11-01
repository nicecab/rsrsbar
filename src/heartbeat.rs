//! The app's liveness file and the decision `check-heartbeat` takes on it.
//!
//! The app writes [`Heartbeat`] every poll. `check-heartbeat`, an
//! independent launchd job, reads it with its own [`WatchState`] and
//! [`decide`]s: restart a stuck app once, then alert (at most every 6 h)
//! while it stays stuck; alert likewise while notifications are not
//! authorized.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::app::Authorization;
use crate::atomic::{read_optional, write_atomic};
use crate::health::elapsed;

/// How often the app polls.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// A heartbeat older than this means the app is stuck; a wake more recent
/// than this means a stale heartbeat is expected.
pub const STALE_AFTER: Duration = Duration::from_secs(3 * POLL_INTERVAL.as_secs());
/// The least time between two alerts of the same kind.
pub const ALERT_REPEAT: Duration = Duration::from_secs(6 * 3600);

/// The app's liveness record (`heartbeat.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    pub at: SystemTime,
    pub pid: u32,
    pub notifications: Authorization,
}

/// `heartbeat.json` on disk: the time as RFC 3339.
#[derive(Serialize, Deserialize)]
struct HeartbeatFile {
    at: String,
    pid: u32,
    notifications: Authorization,
}

impl Heartbeat {
    /// The heartbeat at `path`; missing, unreadable or corrupt → `None`.
    pub fn load(path: &Path) -> Option<Heartbeat> {
        let text = read_optional(path).ok()??;
        let file: HeartbeatFile = serde_json::from_str(&text).ok()?;
        Some(Heartbeat {
            at: humantime::parse_rfc3339(&file.at).ok()?,
            pid: file.pid,
            notifications: file.notifications,
        })
    }

    /// At most [`STALE_AFTER`] old at `now` (a future time counts as fresh).
    pub fn is_fresh(&self, now: SystemTime) -> bool {
        elapsed(self.at, now) <= STALE_AFTER
    }

    /// Writes the heartbeat atomically.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let file = HeartbeatFile {
            at: rfc3339(self.at),
            pid: self.pid,
            notifications: self.notifications,
        };
        write_json(path, &file)
    }
}

/// What `check-heartbeat` alerts about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Stuck,
    NotAuthorized,
}

impl AlertKind {
    /// The alert's text.
    pub fn text(self) -> &'static str {
        match self {
            AlertKind::Stuck => "rsrsbar is not responding; it was restarted and is still stuck",
            AlertKind::NotAuthorized => {
                "rsrsbar cannot show notifications; allow them in System Settings → Notifications"
            }
        }
    }
}

/// `check-heartbeat`'s own memory (`heartbeat-state.json`): when it last
/// restarted the app in the current episode, and when it last raised each
/// alert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchState {
    pub restarted_at: Option<SystemTime>,
    pub alerted: BTreeMap<AlertKind, SystemTime>,
}

/// `heartbeat-state.json` on disk: times as RFC 3339.
#[derive(Default, Serialize, Deserialize)]
struct WatchStateFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restarted_at: Option<String>,
    #[serde(default)]
    alerted: BTreeMap<AlertKind, String>,
}

impl WatchState {
    /// The state at `path`; missing, unreadable or corrupt → the default
    /// (nothing restarted, nothing alerted).
    pub fn load(path: &Path) -> WatchState {
        Self::read(path).unwrap_or_default()
    }

    fn read(path: &Path) -> Option<WatchState> {
        let text = read_optional(path).ok()??;
        let file: WatchStateFile = serde_json::from_str(&text).ok()?;
        let parse = |raw: &String| humantime::parse_rfc3339(raw).ok();
        let restarted_at = match &file.restarted_at {
            Some(raw) => Some(parse(raw)?),
            None => None,
        };
        let alerted = file
            .alerted
            .iter()
            .map(|(kind, raw)| Some((*kind, parse(raw)?)))
            .collect::<Option<_>>()?;
        Some(WatchState {
            restarted_at,
            alerted,
        })
    }

    /// Writes the state atomically.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let file = WatchStateFile {
            restarted_at: self.restarted_at.map(rfc3339),
            alerted: self
                .alerted
                .iter()
                .map(|(kind, at)| (*kind, rfc3339(*at)))
                .collect(),
        };
        write_json(path, &file)
    }

    /// The same state with `kind` recorded as alerted at `now`.
    fn with_alerted(&self, kind: AlertKind, now: SystemTime) -> WatchState {
        self.with_alert_record(kind, Some(now))
    }

    /// The same state without an alert record for `kind`.
    fn without_alerted(&self, kind: AlertKind) -> WatchState {
        self.with_alert_record(kind, None)
    }

    /// The same state with `kind`'s alert record taken from `previous`: an
    /// alert that could not be delivered must not count as sent.
    pub fn with_alert_record_of(&self, previous: &WatchState, kind: AlertKind) -> WatchState {
        self.with_alert_record(kind, previous.alerted.get(&kind).copied())
    }

    /// A copy whose record for `kind` is `at` (`None`: no record).
    fn with_alert_record(&self, kind: AlertKind, at: Option<SystemTime>) -> WatchState {
        let mut alerted = self.alerted.clone();
        match at {
            Some(at) => alerted.insert(kind, at),
            None => alerted.remove(&kind),
        };
        WatchState {
            alerted,
            ..self.clone()
        }
    }

    /// `kind` was alerted less than [`ALERT_REPEAT`] before `now`.
    fn alerted_recently(&self, kind: AlertKind, now: SystemTime) -> bool {
        self.alerted
            .get(&kind)
            .is_some_and(|at| elapsed(*at, now) < ALERT_REPEAT)
    }
}

/// One thing `check-heartbeat` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// `launchctl kickstart -k` the loaded app job.
    Kickstart,
    /// Load the app's LaunchAgent (the job is not loaded).
    Bootstrap,
    Alert(AlertKind),
}

/// The actions for one `check-heartbeat` run and the state to save after
/// them. Within [`STALE_AFTER`] of a wake nothing is judged. A fresh
/// heartbeat (at most [`STALE_AFTER`] old) ends a stuck episode and alerts
/// when notifications are not authorized; a stale or missing one restarts
/// the app once per episode, then alerts. Alerts repeat at most every
/// [`ALERT_REPEAT`].
pub fn decide(
    now: SystemTime,
    awake_since: SystemTime,
    heartbeat: Option<&Heartbeat>,
    app_loaded: bool,
    state: &WatchState,
) -> (Vec<Action>, WatchState) {
    if elapsed(awake_since, now) < STALE_AFTER {
        return (vec![Action::Nothing], state.clone());
    }
    match heartbeat.filter(|beat| beat.is_fresh(now)) {
        Some(beat) => fresh(now, beat.notifications, state),
        None => stale(now, app_loaded, state),
    }
}

fn fresh(
    now: SystemTime,
    notifications: Authorization,
    state: &WatchState,
) -> (Vec<Action>, WatchState) {
    let cleared = WatchState {
        restarted_at: None,
        ..state.without_alerted(AlertKind::Stuck)
    };
    let kind = AlertKind::NotAuthorized;
    if notifications == Authorization::Authorized {
        (vec![Action::Nothing], cleared.without_alerted(kind))
    } else {
        alert(now, kind, &cleared)
    }
}

fn stale(now: SystemTime, app_loaded: bool, state: &WatchState) -> (Vec<Action>, WatchState) {
    if state.restarted_at.is_some() {
        return alert(now, AlertKind::Stuck, state);
    }
    let restart = if app_loaded {
        Action::Kickstart
    } else {
        Action::Bootstrap
    };
    let restarted = WatchState {
        restarted_at: Some(now),
        ..state.clone()
    };
    (vec![restart], restarted)
}

/// Alerts `kind` unless it was alerted recently.
fn alert(now: SystemTime, kind: AlertKind, state: &WatchState) -> (Vec<Action>, WatchState) {
    if state.alerted_recently(kind, now) {
        (vec![Action::Nothing], state.clone())
    } else {
        (vec![Action::Alert(kind)], state.with_alerted(kind, now))
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, text.as_bytes())
}

/// Full precision: the 90 s and 6 h boundaries must hold after a reload.
fn rfc3339(time: SystemTime) -> String {
    humantime::format_rfc3339_nanos(time).to_string()
}
