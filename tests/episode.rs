use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime};

use git_rsrs::outcome::{HookFailure, Outcome as RsrsOutcome, SyncReport, Trigger};
use git_rsrs::status::{Status, next};
use rsrsbar::{
    Color, Health, JobState, Notice, Notify, Observation, RecoveryPoint, RepoId, RepoMemory, Row,
    Settings, State, StatusFile, StatusRead, advance, classify,
};

const MIN: Duration = Duration::from_secs(60);

fn start() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_500_000)
}

fn settings() -> Settings {
    Settings {
        interval: 10 * MIN,
        quiet_period: Duration::from_secs(30),
        git_timeout: Duration::from_secs(120),
        lock_timeout: Duration::from_secs(30),
        lock_name: None,
        stall_threshold: 30 * MIN,
        paused_limit: 24 * 60 * MIN,
        offline_limit: 2 * 60 * MIN,
        other_branch_limit: 24 * 60 * MIN,
    }
}

/// A git-rsrs report for `"H"`, `"F"` (timed out), `"O"` (offline) or
/// `"F:<phase>"` (a hook failure in that phase on an otherwise successful
/// attempt).
fn report(item: &str) -> SyncReport {
    let base = SyncReport::new(Trigger::Periodic);
    match item.split_once(':') {
        Some((_, phase)) => SyncReport {
            hook_failures: vec![HookFailure {
                phase: phase.to_owned(),
                timed_out: false,
                message: format!("{phase} failed"),
            }],
            ..base
        },
        None if item == "F" => SyncReport {
            outcome: RsrsOutcome::TimedOut,
            ..base
        },
        None if item == "O" => SyncReport {
            outcome: RsrsOutcome::Offline,
            ..base
        },
        None => base,
    }
}

/// One poll: the oracle's status after the item, its health and the notice.
struct Step {
    status: StatusFile,
    health: Health,
    notice: Option<Notice>,
    memory: RepoMemory,
}

fn to_ours(status: &Status) -> StatusFile {
    serde_json::from_str(&serde_json::to_string(status).expect("serialize")).expect("parse")
}

fn observe(status: &StatusFile, memory: &RepoMemory, now: SystemTime, trusted: bool) -> Health {
    let settings = settings();
    let read = StatusRead::Present(status.clone());
    classify(&Observation {
        now,
        awake_since: start() - Duration::from_secs(86_400),
        settings: &settings,
        paused_at: None,
        hooks_trusted: trusted,
        job: JobState::Running {
            up_for: Duration::from_secs(86_400),
        },
        status: &read,
        current_branch: Some("main"),
        memory,
    })
}

/// Feeds `items` through git-rsrs's `status::next`, `classify` and `advance`,
/// attempts 10 minutes apart, one poll after each attempt — except after an
/// item prefixed with `~`, which happens between polls.
fn drive(items: &[&str], trusted: bool) -> Vec<Step> {
    let mut previous: Option<Status> = None;
    let mut memory = RepoMemory::default();
    let mut steps = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let now = start() + 10 * MIN * u32::try_from(i).expect("small");
        let (polled, item) = item
            .strip_prefix('~')
            .map_or((true, *item), |rest| (false, rest));
        let raw = next(previous.as_ref(), &report(item), now, 42);
        previous = Some(raw.clone());
        if !polled {
            continue;
        }
        let status = to_ours(&raw);
        let health = observe(&status, &memory, now, trusted);
        let (advanced, notice) = advance(&memory, &health, Some(&status));
        memory = advanced;
        steps.push(Step {
            status,
            health,
            notice,
            memory: memory.clone(),
        });
    }
    steps
}

fn notices(steps: &[Step]) -> Vec<Option<Notice>> {
    steps.iter().map(|s| s.notice.clone()).collect()
}

fn alert(label: &str) -> Option<Notice> {
    Some(Notice::Alert {
        label: label.to_owned(),
    })
}

fn hand_built(row: Row, color: Color, notify: Notify, label: &str) -> Health {
    Health {
        row,
        color,
        notify,
        label: Some(label.to_owned()),
        phase: None,
    }
}

#[test]
fn two_hook_failures_notify_once() {
    let steps = drive(&["F:on_result", "F:on_result", "F:on_result"], true);
    assert_eq!(
        notices(&steps),
        [None, alert("check failed: on_result"), None]
    );
}

#[test]
fn phase_change_is_a_new_episode() {
    let steps = drive(&["F:on_result", "F:on_result", "F:post_integrate"], true);
    assert_eq!(
        notices(&steps),
        [
            None,
            alert("check failed: on_result"),
            alert("check failed: post_integrate")
        ]
    );
}

#[test]
fn intermittent_episode() {
    let steps = drive(&["F", "H", "F", "H", "F", "H", "F", "H", "H", "F"], true);
    assert_eq!(
        notices(&steps),
        [
            None,
            None,
            None,
            None,
            alert("failing intermittently: 3 of last 10"),
            None,
            None,
            None,
            Some(Notice::Recovered),
            None,
        ]
    );
    assert_eq!(steps[8].health.row, Row::Healthy);
    let recovery = steps[8].memory.recovery.clone().expect("recovery point");
    assert_eq!(recovery.seq, steps[8].status.attempt_seq);
    assert_eq!(steps[9].health.row, Row::SingleFailure);
}

#[test]
fn restart_does_not_resend() {
    let steps = drive(&["F:on_result", "F:on_result"], true);
    let last = steps.last().expect("steps");
    assert_eq!(last.notice, alert("check failed: on_result"));

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("state.json");
    let id = RepoId::new("obaidoc").expect("id");
    State::default()
        .with_repo(&id, last.memory.clone())
        .save(&path)
        .expect("save");
    let reloaded = State::load(&path).expect("load");
    let memory = reloaded.repos.get(&id).expect("memory").clone();
    assert_eq!(memory, last.memory);

    let health = observe(&last.status, &memory, start() + 20 * MIN, true);
    let (_, notice) = advance(&memory, &health, Some(&last.status));
    assert_eq!(notice, None);
}

#[test]
fn counters_reset_discards_recovery_point() {
    let steps = drive(&["F", "F"], true);
    let status = &steps[1].status;
    assert_ne!(status.counters_since, "A");
    let memory = RepoMemory {
        recovery: Some(RecoveryPoint {
            counters_since: "A".to_owned(),
            seq: 40,
        }),
        ..RepoMemory::default()
    };
    assert_eq!(memory.recovery_seq(status), None);
    assert_eq!(
        observe(status, &memory, start() + 10 * MIN, true).row,
        Row::FailureRun
    );
}

#[test]
fn allow_clears_without_waiting() {
    let steps = drive(&["F:pre_commit", "F:pre_commit", "F:pre_commit"], false);
    assert!(steps.iter().all(|s| s.health.row == Row::HooksChanged));
    let last = steps.last().expect("steps");
    let allowed = last.memory.after_allow(Some(&last.status));
    assert_eq!(
        allowed.recovery,
        Some(RecoveryPoint {
            counters_since: last.status.counters_since.clone(),
            seq: last.status.attempt_seq,
        })
    );
    assert_eq!(
        observe(&last.status, &allowed, start() + 20 * MIN, true).row,
        Row::Healthy
    );
    assert_eq!(allowed.after_allow(None), allowed);
}

#[test]
fn flicker_does_not_realert() {
    let long = hand_built(
        Row::OtherBranchLong,
        Color::Yellow,
        Notify::Once,
        "on feature/x, no sync for over 24 h",
    );
    let grey = Health {
        label: None,
        ..hand_built(Row::OtherBranch, Color::Grey, Notify::Never, "")
    };
    let mut memory = RepoMemory::default();
    let mut alerts = 0;
    for health in [&long, &grey, &long] {
        let (next, notice) = advance(&memory, health, None);
        alerts += usize::from(notice.is_some());
        memory = next;
    }
    assert_eq!(alerts, 1);
}

#[test]
fn green_ends_episode() {
    let stalled = hand_built(Row::Stalled, Color::Red, Notify::Yes, "sync stalled");
    let green = Health {
        label: None,
        ..hand_built(Row::Healthy, Color::Green, Notify::Never, "")
    };
    let mut memory = RepoMemory::default();
    let mut alerts = Vec::new();
    for health in [&stalled, &green, &stalled] {
        let (next, notice) = advance(&memory, health, None);
        alerts.push(notice);
        memory = next;
    }
    assert_eq!(alerts, [alert("sync stalled"), None, alert("sync stalled")]);
}

#[test]
fn advance_is_immutable_and_remembers_failure_labels() {
    let memory = RepoMemory::default();
    let failure = Health {
        phase: Some("on_result".to_owned()),
        ..hand_built(
            Row::SingleFailure,
            Color::Yellow,
            Notify::Never,
            "check failed: on_result",
        )
    };
    let (next, notice) = advance(&memory, &failure, None);
    assert_eq!(memory, RepoMemory::default());
    assert_eq!(notice, None);
    assert_eq!(next.label.as_deref(), Some("check failed: on_result"));
    assert_eq!(next.phase.as_deref(), Some("on_result"));
    assert_eq!(next.last_row, Some(Row::SingleFailure));

    let stays = hand_built(Row::StaysRed, Color::Red, Notify::Never, "other");
    let (kept, _) = advance(&next, &stays, None);
    assert_eq!(kept.label.as_deref(), Some("check failed: on_result"));
}

#[test]
fn corrupt_state_is_an_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("state.json");
    assert_eq!(
        State::load(&path).expect("missing is empty"),
        State::default()
    );
    fs::write(&path, "{").expect("write");
    assert!(State::load(&path).is_err());
    fs::write(&path, r#"{"version": 2, "repos": {}}"#).expect("write");
    assert!(State::load(&path).is_err());
}

#[test]
fn alert_wins_when_recovery_coincides() {
    let steps = drive(&["H", "H"], true);
    let status = &steps[1].status;
    let red = RepoMemory {
        last_row: Some(Row::Stalled),
        last_color: Some(Color::Red),
        ..RepoMemory::default()
    };
    let long = hand_built(
        Row::OtherBranchLong,
        Color::Yellow,
        Notify::Once,
        "on feature/x, no sync for over 24 h",
    );
    let (next, notice) = advance(&red, &long, Some(status));
    assert_eq!(notice, alert("on feature/x, no sync for over 24 h"));
    assert_eq!(next.recovery.map(|r| r.seq), Some(status.attempt_seq));

    let grey = hand_built(Row::Offline, Color::Grey, Notify::Never, "");
    let (silent, notice) = advance(&red, &grey, Some(&steps[0].status));
    assert_eq!(notice, None, "leaving red with < 2 successes is silent");
    assert_eq!(silent.recovery, None);
}

#[test]
fn episode_end_forgets_the_failure_label() {
    let steps = drive(
        &[
            "F:on_result",
            "F:on_result",
            "H",
            "H",
            "~F:post_integrate",
            "~F:post_integrate",
            "O",
        ],
        true,
    );
    assert_eq!(
        notices(&steps),
        [
            None,
            alert("check failed: on_result"),
            None,
            Some(Notice::Recovered),
            alert("sync failing"),
        ]
    );
    assert_eq!(steps[3].memory.label, None);
    assert_eq!(steps[3].memory.phase, None);
    let last = steps.last().expect("steps");
    assert_eq!(last.health.row, Row::FailureRun);
    assert_eq!(last.health.phase, None);
}

#[test]
fn state_update_is_locked() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = Arc::new(tmp.path().join("support/state.json"));
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|n| {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let id = RepoId::new(&format!("repo-{n}")).expect("valid id");
                barrier.wait();
                State::update(&path, |s| s.with_repo(&id, RepoMemory::default())).expect("update");
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread");
    }
    let state = State::load(&path).expect("load");
    assert_eq!(state.repos.len(), 8, "no update was lost");

    fs::write(path.as_ref(), "{ not json").expect("corrupt");
    assert!(
        State::update(&path, State::clone).is_err(),
        "a corrupt state is not silently replaced"
    );
}
