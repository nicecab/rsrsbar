//! `rsrsbar status`: every repository classified by the shared health
//! table. Read-only: it never advances episodes or writes state.

use std::time::{Duration, SystemTime};

use super::{CommandError, Ctx, Report, hooks_text};
use crate::episode::{RepoMemory, State};
use crate::health::{Color, Health, classify};
use crate::observe::{ObserveError, observe};
use crate::registry::{Registry, RepoEntry};
use crate::status::{StatusFile, StatusRead};

const HEARTBEAT_LABEL: &str = "com.nicecab.rsrsbar.heartbeat";
const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 3600;
const SECS_PER_DAY: u64 = 86_400;
const RED_EXIT: i32 = 1;
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

/// What the CLI cannot know without the menu bar app (plan 3 fills them in).
fn assumption_lines(ctx: &Ctx) -> Vec<String> {
    let heartbeat = match ctx.launchd.job(HEARTBEAT_LABEL) {
        Ok(Some(_)) => "loaded".to_owned(),
        Ok(None) => "not loaded".to_owned(),
        Err(err) => format!("unknown ({err})"),
    };
    vec![
        format!("heartbeat job: {heartbeat}"),
        "notifications: unknown (menu bar app not running)".to_owned(),
        "focus: unknown".to_owned(),
    ]
}
