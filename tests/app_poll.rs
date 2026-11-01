mod common;

use std::fs;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use common::{Fixture, add_repo, id, now, running, sync_report, write_status};
use git_rsrs::outcome::{HookFailure, Outcome as RsrsOutcome, SyncReport};
use rsrsbar::app::poll::{PollOutcome, acquire_instance, poll};
use rsrsbar::commands::Ctx;
use rsrsbar::heartbeat::Heartbeat;
use rsrsbar::{
    Authorization, Color, Notice, Recording, RecoveryPoint, RepoId, RepoMemory, Row, State,
};

const MIN: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(86_400);

/// One poll at `at` by a fresh "process": a new `Ctx` over `launchd`, long
/// after the last wake, notifications authorized.
fn poll_at(fx: &Fixture, launchd: &Recording, at: SystemTime) -> PollOutcome {
    let ctx = Ctx {
        now: at,
        ..fx.ctx(launchd)
    };
    poll(&ctx, at - DAY, Authorization::Authorized)
}

fn on_result_failure() -> SyncReport {
    SyncReport {
        hook_failures: vec![HookFailure {
            phase: "on_result".to_owned(),
            timed_out: false,
            message: "notify failed".to_owned(),
        }],
        ..sync_report(RsrsOutcome::CheckFailed)
    }
}

fn alert(repo: &RepoId, label: &str) -> (RepoId, Notice) {
    (
        repo.clone(),
        Notice::Alert {
            label: label.to_owned(),
        },
    )
}

#[test]
fn first_poll_alerts_second_does_not() {
    let fx = Fixture::new();
    let repo = add_repo(&fx, "alpha");
    let failure = on_result_failure();
    write_status(
        &fx,
        &repo,
        &[(failure.clone(), now() - MIN), (failure, now())],
    );
    let launchd = running(&[&repo]);

    let first = poll_at(&fx, &launchd, now());
    assert_eq!(first.notices, vec![alert(&repo, "check failed: on_result")]);
    assert_eq!(first.repos.len(), 1);
    assert_eq!(first.repos[0].entry.id, repo);
    assert_eq!(first.repos[0].health.row, Row::FailureRun);
    assert!(first.repos[0].snapshot.is_some());
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);

    let second = poll_at(&fx, &launchd, now() + MIN);
    assert!(second.notices.is_empty(), "{:?}", second.notices);
    assert_eq!(second.repos[0].health.color, Color::Red);
}

#[test]
fn restart_does_not_resend() {
    let fx = Fixture::new();
    let repo = add_repo(&fx, "alpha");
    let failure = on_result_failure();
    write_status(
        &fx,
        &repo,
        &[(failure.clone(), now() - MIN), (failure, now())],
    );

    let before = poll_at(&fx, &running(&[&repo]), now());
    assert_eq!(before.notices.len(), 1);

    // A new process: new launchd connection, everything reloaded from disk.
    let after = poll_at(&fx, &running(&[&repo]), now() + 2 * MIN);
    assert!(after.notices.is_empty(), "{:?}", after.notices);
    assert_eq!(after.repos[0].health.row, Row::FailureRun);
}

#[test]
fn concurrent_allow_survives_poll() {
    let fx = Fixture::new();
    let repo = add_repo(&fx, "alpha");
    write_status(&fx, &repo, &[(sync_report(RsrsOutcome::Synced), now())]);
    let state_path = fx.paths.state();

    thread::scope(|scope| {
        scope.spawn(|| {
            let launchd = running(&[&repo]);
            for i in 0..50u64 {
                poll_at(&fx, &launchd, now() + Duration::from_secs(i));
            }
        });
        scope.spawn(|| {
            for seq in 1..=50 {
                State::update(&state_path, |state| {
                    let memory = state.repos.get(&repo).cloned().unwrap_or_default();
                    let point = RecoveryPoint {
                        counters_since: "2026-11-01T00:00:00Z".to_owned(),
                        seq,
                    };
                    state.with_repo(
                        &repo,
                        RepoMemory {
                            recovery: Some(point),
                            ..memory
                        },
                    )
                })
                .expect("allow-style update");
            }
        });
    });

    let state = State::load(&state_path).expect("state");
    let memory = state.repos.get(&repo).expect("memory");
    assert_eq!(memory.recovery.as_ref().map(|point| point.seq), Some(50));
}

#[test]
fn unobservable_repo_is_red() {
    let fx = Fixture::new();
    let repo = add_repo(&fx, "alpha");
    fs::remove_dir_all(fx.root().join("alpha")).expect("delete the checkout");
    let launchd = running(&[&repo]);

    let first = poll_at(&fx, &launchd, now());
    let view = &first.repos[0];
    assert_eq!(view.health.row, Row::Unobservable);
    assert_eq!(view.health.color, Color::Red);
    assert_eq!(
        view.health.label.as_deref(),
        Some("cannot observe repository")
    );
    assert!(view.snapshot.is_none());
    let error = view.error.as_deref().expect("error kept for the menu");
    assert!(error.contains("alpha"), "{error}");
    assert_eq!(
        first.notices,
        vec![alert(&repo, "cannot observe repository")]
    );

    let second = poll_at(&fx, &launchd, now() + MIN);
    assert!(second.notices.is_empty(), "{:?}", second.notices);
}

#[test]
fn unregistered_memory_dropped() {
    let fx = Fixture::new();
    let repo = add_repo(&fx, "alpha");
    write_status(&fx, &repo, &[(sync_report(RsrsOutcome::Synced), now())]);
    let ghost = id("ghost");
    State::default()
        .with_repo(&ghost, RepoMemory::default())
        .save(&fx.paths.state())
        .expect("seed state");

    poll_at(&fx, &running(&[&repo]), now());

    let state = State::load(&fx.paths.state()).expect("state");
    assert!(!state.repos.contains_key(&ghost), "{state:?}");
    assert!(state.repos.contains_key(&repo), "{state:?}");
}

#[test]
fn heartbeat_written_even_without_registry() {
    let fx = Fixture::new();
    let registry = fx.paths.registry();
    fs::create_dir_all(registry.parent().expect("parent")).expect("mkdir");
    fs::write(&registry, "this is [[[ not toml").expect("corrupt registry");

    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);
    let outcome = poll(&ctx, now() - DAY, Authorization::Denied);
    assert!(outcome.repos.is_empty());
    assert!(outcome.notices.is_empty());
    assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
    assert!(
        outcome.warnings[0].contains("registry.toml"),
        "{:?}",
        outcome.warnings
    );

    let heartbeat = Heartbeat::load(&fx.paths.heartbeat()).expect("heartbeat written");
    assert_eq!(heartbeat.at, now());
    assert_eq!(heartbeat.pid, std::process::id());
    assert_eq!(heartbeat.notifications, Authorization::Denied);
}

#[test]
fn corrupt_state_is_set_aside() {
    let fx = Fixture::new();
    let repo = add_repo(&fx, "alpha");
    write_status(&fx, &repo, &[(sync_report(RsrsOutcome::Synced), now())]);
    fs::write(fx.paths.state(), "{ corrupt").expect("corrupt state");

    let outcome = poll_at(&fx, &running(&[&repo]), now());
    assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
    assert!(
        outcome.warnings[0].contains("state.json.corrupt"),
        "{:?}",
        outcome.warnings
    );
    assert_eq!(outcome.repos[0].health.row, Row::Healthy);
    let kept = fx.paths.state().with_extension("json.corrupt");
    assert_eq!(fs::read_to_string(kept).expect("kept"), "{ corrupt");
    assert!(
        State::load(&fx.paths.state())
            .expect("fresh state")
            .repos
            .contains_key(&repo)
    );
    // A second, separate corruption keeps the first copy and adds another.
    fs::write(fx.paths.state(), "{ corrupt again").expect("corrupt again");
    let again = poll_at(&fx, &running(&[&repo]), now() + MIN);
    assert!(
        again.warnings[0].contains("state.json.corrupt-2"),
        "{:?}",
        again.warnings
    );
    let kept = fx.paths.state().with_extension("json.corrupt");
    assert_eq!(fs::read_to_string(kept).expect("first copy"), "{ corrupt");
    let second = fx.paths.state().with_extension("json.corrupt-2");
    assert_eq!(
        fs::read_to_string(second).expect("second copy"),
        "{ corrupt again"
    );
}

#[test]
fn second_instance_refused() {
    let fx = Fixture::new();
    let first = acquire_instance(&fx.paths)
        .expect("lock")
        .expect("first instance");
    assert!(acquire_instance(&fx.paths).expect("lock").is_none());
    drop(first);
    // Released on drop. Other tests in this binary spawn processes
    // concurrently; a child forked before its exec briefly shares every
    // descriptor (they are close-on-exec, so never past the exec), which
    // can hold the lock for a moment after the drop — so allow a short wait.
    let deadline = Instant::now() + Duration::from_secs(5);
    let reacquired = std::iter::repeat_with(|| {
        let lock = acquire_instance(&fx.paths).expect("lock");
        if lock.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        lock
    })
    .find(|lock| lock.is_some() || Instant::now() > deadline)
    .flatten();
    assert!(reacquired.is_some(), "the lock is released on drop");
}

#[test]
fn unreadable_registry_leaves_state_alone() {
    let fx = Fixture::new();
    let repo = add_repo(&fx, "alpha");
    let failure = on_result_failure();
    write_status(
        &fx,
        &repo,
        &[(failure.clone(), now() - MIN), (failure, now())],
    );
    let launchd = running(&[&repo]);
    assert_eq!(poll_at(&fx, &launchd, now()).notices.len(), 1);
    let before = fs::read_to_string(fx.paths.state()).expect("state");

    let registry = fx.paths.registry();
    let good = fs::read_to_string(&registry).expect("registry");
    fs::write(&registry, "this is [[[ not toml").expect("corrupt registry");
    let blind = poll_at(&fx, &launchd, now() + MIN);
    assert!(blind.repos.is_empty() && blind.notices.is_empty());
    assert_eq!(blind.warnings.len(), 1, "{:?}", blind.warnings);
    assert_eq!(fs::read_to_string(fx.paths.state()).expect("state"), before);

    fs::write(&registry, good).expect("restore registry");
    let restored = poll_at(&fx, &launchd, now() + 2 * MIN);
    assert!(restored.notices.is_empty(), "{:?}", restored.notices);
    assert_eq!(restored.repos[0].health.row, Row::FailureRun);
}
