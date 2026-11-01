//! `rsrsbar status`: every repository classified by the shared health
//! table. Read-only: it never advances episodes or writes state.

use std::time::{Duration, SystemTime};

use super::{CommandError, Ctx, Report, hooks_text};
use crate::app::Authorization;
use crate::episode::{RepoMemory, State};
use crate::health::{Color, Health, classify};
use crate::heartbeat::Heartbeat;
use crate::observe::{ObserveError, observe};
use crate::paths::HEARTBEAT_LABEL;
use crate::registry::{Registry, RepoEntry};
use crate::status::{StatusFile, StatusRead};
use crate::system::parse_focus;

const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 3600;
const SECS_PER_DAY: u64 = 86_400;
const RED_EXIT: i32 = 1;
/// More than this is not a Focus assertions file worth parsing.
const FOCUS_MAX_BYTES: u64 = 1024 * 1024;
const NO_REPOSITORIES: &str = "no repositories registered; add one with rsrsbar add <PATH>";

/// One block per repository in registry order, then the heartbeat,
/// notification and Focus lines. A corrupt `state.json` classifies with
/// empty memory behind one warning line; a repository that cannot be
/// observed is shown red with the error. Exit 1 iff any repository is shown
/// red, else 0.
pub fn status(ctx: &Ctx, awake_since: SystemTime) -> Result<Report, CommandError> {
    let registry = Registry::load(&ctx.paths.registry())?;
    let (state, warning) = match State::load(&ctx.paths.state()) {
        Ok(state) => (state, None),
        Err(err) => (
            State::default(),
            Some(format!("warning: ignoring the state file: {err}")),
        ),
    };
    let blocks: Vec<(String, bool)> = registry
        .repos
        .iter()
        .map(|entry| {
            let memory = state.repos.get(&entry.id).cloned().unwrap_or_default();
            block(ctx, entry, &memory, awake_since)
        })
        .collect();
    let failing = blocks.iter().any(|(_, red)| *red);
    let empty = registry
        .repos
        .is_empty()
        .then(|| NO_REPOSITORIES.to_owned());
    let lines: Vec<String> = warning
        .into_iter()
        .chain(empty)
        .chain(blocks.into_iter().map(|(text, _)| text))
        .chain(assumption_lines(ctx))
        .collect();
    Ok(Report {
        text: lines.join("\n"),
        exit_code: if failing { RED_EXIT } else { 0 },
    })
}

/// The repository's block and whether it counts as red.
fn block(
    ctx: &Ctx,
    entry: &RepoEntry,
    memory: &RepoMemory,
    awake_since: SystemTime,
) -> (String, bool) {
    let snapshot = match observe(entry, &ctx.paths, ctx.launchd) {
        Ok(snapshot) => snapshot,
        Err(err) => return (unobservable(entry, &err), true),
    };
    let health = classify(&snapshot.observation(ctx.now, awake_since, memory));
    let headline = match &health.label {
        Some(label) => format!("{}  {}  {label}", entry.id, color_name(health.color)),
        None => format!("{}  {}", entry.id, color_name(health.color)),
    };
    let hooks = hooks_text(&entry.path);
    let message = match &snapshot.status {
        StatusRead::Present(status) => status.display_message().map(str::to_owned),
        _ => None,
    };
    let lines: Vec<String> = [
        Some(headline),
        Some(format!("  {}", summary(&snapshot.status, ctx.now))),
        Some(format!(
            "  {} · {} → {}",
            entry.path.display(),
            entry.branch,
            entry.remote
        )),
        Some(format!("  hooks: {hooks}")),
        message.map(|m| format!("  message: {m}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (lines.join("\n"), health.color == Color::Red)
}

/// A repository that cannot be observed is shown red, like any state that
/// needs attention: the headline names the problem, the detail lines carry
/// the error and, where one is obvious, the fix.
fn unobservable(entry: &RepoEntry, err: &ObserveError) -> String {
    let fix = match err {
        ObserveError::PausedAt { .. } => Some(format!("  fix: run rsrsbar resume {}", entry.id)),
        ObserveError::Settings(_) | ObserveError::Launchd(_) | ObserveError::Checkout { .. } => {
            None
        }
    };
    let health = Health::unobservable();
    let label = health.label.as_deref().unwrap_or_default();
    [
        Some(format!(
            "{}  {}  {label}",
            entry.id,
            color_name(health.color)
        )),
        Some(format!("  error: {err}")),
        fix,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

fn color_name(color: Color) -> &'static str {
    match color {
        Color::Green => "GREEN",
        Color::Grey => "GREY",
        Color::Yellow => "YELLOW",
        Color::Red => "RED",
    }
}

/// "last success 42 min ago · latest noop · 3 failures in a row".
fn summary(status: &StatusRead, now: SystemTime) -> String {
    let status = match status {
        StatusRead::Present(status) => status,
        StatusRead::Missing => return "no status file yet".to_owned(),
        StatusRead::Unreadable(err) => return format!("status file unreadable: {err}"),
    };
    [
        Some(last_success(status, now)),
        Some(format!("latest {}", status.last_outcome.as_str())),
        failures(status.consecutive_failures),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

pub(crate) fn last_success(status: &StatusFile, now: SystemTime) -> String {
    match status.last_success_at {
        Some(at) => format!("last success {} ago", ago(now, at)),
        None => "no success yet".to_owned(),
    }
}

pub(crate) fn failures(count: u32) -> Option<String> {
    match count {
        0 => None,
        1 => Some("1 failure in a row".to_owned()),
        n => Some(format!("{n} failures in a row")),
    }
}

/// `now - then` in the largest whole unit (s, min, h, d); a future `then`
/// reads as 0 s.
pub(crate) fn ago(now: SystemTime, then: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or(Duration::ZERO).as_secs();
    match secs {
        s if s < SECS_PER_MINUTE => format!("{s} s"),
        s if s < SECS_PER_HOUR => format!("{} min", s / SECS_PER_MINUTE),
        s if s < SECS_PER_DAY => format!("{} h", s / SECS_PER_HOUR),
        s => format!("{} d", s / SECS_PER_DAY),
    }
}

/// The three assumptions behind "no alert means healthy": the heartbeat
/// job is loaded, notifications are authorized (as the app last wrote into
/// `heartbeat.json`), and Focus is not holding them back.
fn assumption_lines(ctx: &Ctx) -> Vec<String> {
    let heartbeat = match ctx.launchd.job(HEARTBEAT_LABEL) {
        Ok(Some(_)) => "loaded".to_owned(),
        Ok(None) => "not loaded".to_owned(),
        Err(err) => format!("unknown ({err})"),
    };
    let focus = match read_focus_file(&ctx.paths.focus_assertions()).and_then(|j| parse_focus(&j)) {
        Some(true) => "on",
        Some(false) => "off",
        None => "unknown",
    };
    vec![
        format!("heartbeat job: {heartbeat}"),
        format!("notifications: {}", notifications_text(ctx)),
        format!("focus: {focus}"),
    ]
}

/// The Focus assertions file, only if it is a regular file, at most
/// [`FOCUS_MAX_BYTES`] of it; anything else (missing, a FIFO, too large,
/// unreadable) is `None` — "unknown" — and never blocks `status`.
fn read_focus_file(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    // Non-blocking open: opening a FIFO with no writer would otherwise
    // wait forever; the type is checked on the opened file itself.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()
        .filter(|file| file.metadata().is_ok_and(|meta| meta.is_file()))?;
    let mut text = String::new();
    file.take(FOCUS_MAX_BYTES + 1)
        .read_to_string(&mut text)
        .ok()
        .filter(|&read| read as u64 <= FOCUS_MAX_BYTES)?;
    Some(text)
}

/// From a fresh heartbeat ([`Heartbeat::is_fresh`]); otherwise unknown,
/// saying why.
fn notifications_text(ctx: &Ctx) -> String {
    let Some(heartbeat) = Heartbeat::load(&ctx.paths.heartbeat()) else {
        return "unknown (menu bar app not running)".to_owned();
    };
    if !heartbeat.is_fresh(ctx.now) {
        return format!("unknown (no heartbeat for {})", ago(ctx.now, heartbeat.at));
    }
    match heartbeat.notifications {
        Authorization::Authorized => "authorized",
        Authorization::Denied => "denied",
        Authorization::NotDetermined => "not determined",
        Authorization::Unknown => "unknown",
    }
    .to_owned()
}
