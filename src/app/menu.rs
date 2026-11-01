//! The menu as plain data, built from one poll: the shell only renders it.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use super::Authorization;
use super::actions::Action;
use super::poll::{PollOutcome, RepoView};
use crate::health::{Color, Row};
use crate::paths::RepoId;
use crate::registry::AppSettings;
use crate::status::{StatusFile, StatusRead};

const NOTIFICATIONS_OFF: &str = "Notifications are off — alerts cannot reach you";
const MESSAGE_CHARS: usize = 80;
const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 3600;
const SECS_PER_DAY: u64 = 86_400;

/// One menu entry. Labels are shown disabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Label(String),
    Action {
        title: String,
        action: Action,
        enabled: bool,
    },
    Submenu {
        title: String,
        color: Color,
        items: Vec<Item>,
    },
    Separator,
}

/// What the status item shows: the number of red repositories and whether
/// notifications cannot be delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconState {
    pub red: usize,
    pub warning: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuModel {
    pub icon: IconState,
    pub items: Vec<Item>,
}

/// The menu for one poll: the notifications warning (when not
/// authorized), one submenu per repository in registry order with `first`
/// moved to the top, then the version. There is no Quit item: quitting
/// would silently remove the watchdog (`rsrsbar stop` is the way out).
pub fn build(
    outcome: &PollOutcome,
    app: &AppSettings,
    notifications: Authorization,
    busy: &BTreeSet<RepoId>,
    first: Option<&RepoId>,
    now: SystemTime,
) -> MenuModel {
    let warning = notifications != Authorization::Authorized;
    let red = outcome
        .repos
        .iter()
        .filter(|view| view.health.color == Color::Red)
        .count();
    let is_first = |view: &&RepoView| Some(&view.entry.id) == first;
    let ordered = outcome
        .repos
        .iter()
        .filter(is_first)
        .chain(outcome.repos.iter().filter(|view| !is_first(view)));
    let items = warning
        .then(|| Item::Label(NOTIFICATIONS_OFF.to_owned()))
        .into_iter()
        .chain(ordered.map(|view| submenu(view, app, busy.contains(&view.entry.id), now)))
        .chain([
            Item::Separator,
            Item::Label(format!("rsrsbar {}", env!("CARGO_PKG_VERSION"))),
        ])
        .collect();
    MenuModel {
        icon: IconState { red, warning },
        items,
    }
}

fn submenu(view: &RepoView, app: &AppSettings, busy: bool, now: SystemTime) -> Item {
    let items = details(view, now)
        .into_iter()
        .map(Item::Label)
        .chain([Item::Separator])
        .chain(actions(view, app, busy))
        .collect();
    Item::Submenu {
        title: format!("{} — {}", view.entry.id, headline(view)),
        color: view.health.color,
        items,
    }
}

/// The health label; rows without one name their state.
fn headline(view: &RepoView) -> &str {
    if let Some(label) = view.health.label.as_deref() {
        return label;
    }
    match view.health.row {
        Row::Paused => "paused",
        Row::Offline => "offline",
        Row::OtherBranch => "on another branch",
        _ => "healthy",
    }
}

/// Status detail, the error when the repository could not be observed,
/// then where it lives and its hooks.
fn details(view: &RepoView, now: SystemTime) -> Vec<String> {
    let status_lines = match (&view.snapshot, &view.error) {
        (Some(snapshot), _) => status_details(&snapshot.status, now),
        (None, Some(error)) => vec![format!("error: {}", shorten(error))],
        (None, None) => Vec::new(),
    };
    let entry = &view.entry;
    let hooks: Vec<&str> = view.hooks.iter().map(|phase| phase.file_name()).collect();
    let hooks = if hooks.is_empty() {
        "none".to_owned()
    } else {
        hooks.join(", ")
    };
    status_lines
        .into_iter()
        .chain([
            entry.path.display().to_string(),
            format!("{} → {}", entry.branch, entry.remote),
            format!("hooks: {hooks}"),
        ])
        .collect()
}

fn status_details(status: &StatusRead, now: SystemTime) -> Vec<String> {
    let status = match status {
        StatusRead::Present(status) => status,
        StatusRead::Missing => return vec!["no status file yet".to_owned()],
        StatusRead::Unreadable(err) => {
            return vec![format!("status file unreadable: {}", shorten(err))];
        }
    };
    [
        Some(last_success(status, now)),
        Some(format!("latest {}", status.last_outcome.as_str())),
        failures(status.consecutive_failures),
        status
            .display_message()
            .map(|message| format!("message: {}", shorten(message))),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn last_success(status: &StatusFile, now: SystemTime) -> String {
    match status.last_success_at {
        Some(at) => format!("last success {} ago", ago(now, at)),
        None => "no success yet".to_owned(),
    }
}

fn failures(count: u32) -> Option<String> {
    match count {
        0 => None,
        1 => Some("1 failure in a row".to_owned()),
        n => Some(format!("{n} failures in a row")),
    }
}

/// The first line, cut to [`MESSAGE_CHARS`] characters (an ellipsis marks
/// the cut).
fn shorten(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= MESSAGE_CHARS {
        return line.to_owned();
    }
    let kept: String = line.chars().take(MESSAGE_CHARS - 1).collect();
    format!("{kept}…")
}

/// `now - then` in the largest whole unit (s, min, h, d); a future `then`
/// reads as 0 s.
fn ago(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or(Duration::ZERO).as_secs();
    match secs {
        s if s < SECS_PER_MINUTE => format!("{s} s"),
        s if s < SECS_PER_HOUR => format!("{} min", s / SECS_PER_MINUTE),
        s if s < SECS_PER_DAY => format!("{} h", s / SECS_PER_HOUR),
        s => format!("{} d", s / SECS_PER_DAY),
    }
}

fn actions(view: &RepoView, app: &AppSettings, busy: bool) -> Vec<Item> {
    let id = &view.entry.id;
    let item = |title: String, action: Action| Item::Action {
        title,
        action,
        enabled: true,
    };
    let sync = Item::Action {
        title: if busy { "Syncing…" } else { "Sync now" }.to_owned(),
        action: Action::SyncNow(id.clone()),
        enabled: !busy,
    };
    let editor = app
        .editor
        .as_ref()
        .map(|editor| item(format!("Open in {editor}"), Action::OpenEditor(id.clone())));
    let toggle = if view.entry.paused_at.is_some() {
        item("Resume".to_owned(), Action::Resume(id.clone()))
    } else {
        item("Pause".to_owned(), Action::Pause(id.clone()))
    };
    let allow = (view.health.row == Row::HooksChanged)
        .then(|| item("Allow hooks…".to_owned(), Action::AllowHooks(id.clone())));
    [
        Some(sync),
        Some(item("Open log".to_owned(), Action::OpenLog(id.clone()))),
        Some(item(
            format!("Open in {}", app.terminal_app()),
            Action::OpenTerminal(id.clone()),
        )),
        editor,
        Some(item(
            "Restart watcher".to_owned(),
            Action::Restart(id.clone()),
        )),
        Some(toggle),
        allow,
    ]
    .into_iter()
    .flatten()
    .collect()
}
