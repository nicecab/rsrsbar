mod common;

use std::fs;
use std::time::{Duration, SystemTime};

use common::{Fixture, id, now};
use git_rsrs::outcome::{HookFailure, Outcome as RsrsOutcome, SyncReport, Trigger};
use git_rsrs::status::{Status, next};
use rsrsbar::commands::{Report, add, status};
use rsrsbar::{Recording, RepoId, watch_label};

const MIN: Duration = Duration::from_secs(60);

fn add_repo(fx: &Fixture, name: &str) -> RepoId {
    let args = common::add_args(&fx.repo(name, true));
    add(&fx.ctx(&Recording::new()), args).expect("add");
    id(name)
}

fn report(outcome: RsrsOutcome) -> SyncReport {
    SyncReport {
        outcome,
        ..SyncReport::new(Trigger::Periodic)
    }
}

/// Writes the status git-rsrs would after `attempts` (report, time).
fn write_status(fx: &Fixture, repo_id: &RepoId, attempts: &[(SyncReport, SystemTime)]) {
    let status = attempts
        .iter()
        .fold(None::<Status>, |previous, (report, at)| {
            Some(next(previous.as_ref(), report, *at, 42))
        })
        .expect("at least one attempt");
    let json = serde_json::to_string(&status).expect("json");
    fs::write(fx.paths.status_file(repo_id), json).expect("write status");
}

/// Both watchers loaded and running as this test process.
fn running(ids: &[&RepoId]) -> Recording {
    ids.iter().fold(Recording::new(), |launchd, repo_id| {
        launchd.with_loaded(&watch_label(repo_id), Some(std::process::id()))
    })
}

fn awake_long_ago() -> SystemTime {
    now() - Duration::from_secs(24 * 3600)
}

fn run_status(fx: &Fixture, launchd: &Recording) -> Report {
    status(&fx.ctx(launchd), awake_long_ago()).expect("status")
}

#[test]
fn status_lists_each_repo() {
    let fx = Fixture::new();
    let healthy = add_repo(&fx, "healthy");
    let stalled = add_repo(&fx, "stalled");
    write_status(&fx, &healthy, &[(report(RsrsOutcome::Synced), now())]);
    write_status(
        &fx,
        &stalled,
        &[
            (report(RsrsOutcome::Synced), now() - 31 * MIN),
            (report(RsrsOutcome::Deferred), now()),
        ],
    );

    let report = run_status(&fx, &running(&[&healthy, &stalled]));
    let text = &report.text;
    assert_eq!(report.exit_code, 1, "{text}");
    assert!(text.contains("healthy  GREEN"), "{text}");
    assert!(text.contains("stalled  RED  sync stalled"), "{text}");
    assert!(
        text.contains("last success 31 min ago · latest deferred"),
        "{text}"
    );
    assert!(text.contains("main → origin"), "{text}");
    assert!(text.contains("hooks: none"), "{text}");
    assert!(text.contains("heartbeat job: not loaded"), "{text}");
    assert!(
        text.contains("notifications: unknown (menu bar app not running)"),
        "{text}"
    );
    assert!(text.contains("focus: unknown"), "{text}");
    let healthy_at = text.find("healthy  GREEN").expect("healthy block");
    let stalled_at = text.find("stalled  RED").expect("stalled block");
    assert!(healthy_at < stalled_at, "registry order: {text}");
}

#[test]
fn status_all_green_exit_0() {
    let fx = Fixture::new();
    let alpha = add_repo(&fx, "alpha");
    write_status(&fx, &alpha, &[(report(RsrsOutcome::Synced), now() - MIN)]);
    let launchd = running(&[&alpha]).with_loaded("com.nicecab.rsrsbar.heartbeat", None);

    let report = run_status(&fx, &launchd);
    assert_eq!(report.exit_code, 0, "{}", report.text);
    assert!(report.text.contains("alpha  GREEN"), "{}", report.text);
    assert!(
        report
            .text
            .contains("last success 1 min ago · latest synced"),
        "{}",
        report.text
    );
    assert!(
        report.text.contains("heartbeat job: loaded"),
        "{}",
        report.text
    );
    assert!(
        launchd.calls().is_empty(),
        "status is read-only: {:?}",
        launchd.calls()
    );
    assert!(!fx.paths.state().exists(), "status never writes state");
}

#[test]
fn status_message_fallback() {
    let fx = Fixture::new();
    let alpha = add_repo(&fx, "alpha");
    let failed = SyncReport {
        hook_failures: vec![HookFailure {
            phase: "pre_commit".to_owned(),
            timed_out: false,
            message: "docsctl check failed".to_owned(),
        }],
        ..report(RsrsOutcome::CheckFailed)
    };
    write_status(&fx, &alpha, &[(failed, now())]);

    let report = run_status(&fx, &running(&[&alpha]));
    assert!(
        report.text.contains("  message: docsctl check failed"),
        "{}",
        report.text
    );
    assert!(
        report.text.contains("1 failure in a row"),
        "{}",
        report.text
    );
}

#[test]
fn status_survives_corrupt_state() {
    let fx = Fixture::new();
    let alpha = add_repo(&fx, "alpha");
    write_status(&fx, &alpha, &[(report(RsrsOutcome::Synced), now())]);
    let state = fx.paths.state();
    fs::write(&state, "{").expect("corrupt state");

    let report = run_status(&fx, &running(&[&alpha]));
    assert_eq!(report.exit_code, 0, "{}", report.text);
    let warnings: Vec<_> = report
        .text
        .lines()
        .filter(|line| line.starts_with("warning:"))
        .collect();
    assert_eq!(warnings.len(), 1, "{}", report.text);
    assert!(report.text.contains("alpha  GREEN"), "{}", report.text);
    assert_eq!(
        fs::read_to_string(&state).expect("state"),
        "{",
        "left as it was"
    );
}

#[test]
fn status_shows_an_unobservable_repo_as_red() {
    let fx = Fixture::new();
    let alpha = add_repo(&fx, "alpha");
    let demo = add_repo(&fx, "demo");
    write_status(&fx, &alpha, &[(report(RsrsOutcome::Synced), now())]);
    rsrsbar::Registry::update(&fx.paths.registry(), |r| {
        r.with_entry(&demo, |e| rsrsbar::RepoEntry {
            paused_at: Some("not a time".to_owned()),
            ..e.clone()
        })
    })
    .expect("corrupt paused_at");

    let report = run_status(&fx, &running(&[&alpha, &demo]));
    let text = &report.text;
    assert_eq!(report.exit_code, 1, "{text}");
    assert!(text.contains("alpha  GREEN"), "{text}");
    assert!(
        text.contains("demo  RED  cannot observe repository"),
        "{text}"
    );
    assert!(
        text.contains("  error: demo: paused_at \"not a time\""),
        "{text}"
    );
    assert!(text.contains("  fix: run rsrsbar resume demo"), "{text}");
    assert!(!text.contains("UNKNOWN"), "{text}");
}

#[test]
fn status_with_no_repositories() {
    let fx = Fixture::new();
    let report = run_status(&fx, &Recording::new());
    assert_eq!(report.exit_code, 0);
    let first = report.text.lines().next().expect("a line");
    assert_eq!(
        first,
        "no repositories registered; add one with rsrsbar add <PATH>"
    );
    assert!(
        report.text.contains("heartbeat job: not loaded"),
        "{}",
        report.text
    );
}

#[test]
fn status_reads_heartbeat() {
    use rsrsbar::Authorization;
    use rsrsbar::heartbeat::Heartbeat;

    let fx = Fixture::new();
    let launchd = Recording::new();
    let beat = |at: SystemTime, notifications| Heartbeat {
        at,
        pid: 4242,
        notifications,
    };
    let lines = |fx: &Fixture| run_status(fx, &launchd).text;

    beat(now() - MIN / 2, Authorization::Authorized)
        .save(&fx.paths.heartbeat())
        .expect("save heartbeat");
    assert!(
        lines(&fx).contains("notifications: authorized"),
        "{}",
        lines(&fx)
    );

    beat(now() - MIN / 2, Authorization::NotDetermined)
        .save(&fx.paths.heartbeat())
        .expect("save heartbeat");
    assert!(
        lines(&fx).contains("notifications: not determined"),
        "{}",
        lines(&fx)
    );

    beat(now() - 5 * MIN, Authorization::Authorized)
        .save(&fx.paths.heartbeat())
        .expect("save heartbeat");
    let text = lines(&fx);
    assert!(
        text.contains("notifications: unknown (no heartbeat for 5 min)"),
        "{text}"
    );

    // Focus comes from the home's DoNotDisturb assertions.
    assert!(text.contains("focus: unknown"), "{text}");
    let assertions = fx
        .paths
        .home
        .join("Library/DoNotDisturb/DB/Assertions.json");
    fs::create_dir_all(assertions.parent().expect("parent")).expect("mkdir");
    fs::write(&assertions, include_str!("fixtures/focus_on.json")).expect("write");
    assert!(lines(&fx).contains("focus: on"), "{}", lines(&fx));
    fs::write(
        &assertions,
        include_str!("fixtures/focus_off_no_records.json"),
    )
    .expect("write");
    assert!(lines(&fx).contains("focus: off"), "{}", lines(&fx));
}

#[test]
fn parse_focus() {
    assert_eq!(
        rsrsbar::parse_focus(include_str!("fixtures/focus_on.json")),
        Some(true)
    );
    assert_eq!(
        rsrsbar::parse_focus(include_str!("fixtures/focus_off.json")),
        Some(false)
    );
    assert_eq!(
        rsrsbar::parse_focus(include_str!("fixtures/focus_off_no_records.json")),
        Some(false)
    );
    assert_eq!(
        rsrsbar::parse_focus(include_str!("fixtures/focus_garbage.json")),
        None
    );
    // A record with neither assertions nor invalidations is not a shape
    // macOS was seen to write: unknown, not off.
    assert_eq!(
        rsrsbar::parse_focus(include_str!("fixtures/focus_unknown_record.json")),
        None
    );
    assert_eq!(rsrsbar::parse_focus(r#"{"data": "not a list"}"#), None);
    assert_eq!(rsrsbar::parse_focus(r#"{"header": {}}"#), None);
}
