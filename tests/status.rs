use std::fs;
use std::time::{Duration, SystemTime};

use git_rsrs::outcome::{HookFailure, Outcome as RsrsOutcome, SyncReport, Trigger};
use git_rsrs::status::{Health, health, next};
use rsrsbar::{AttemptHealth, Outcome, RecentAttempt, StatusFile, StatusRead, read_status};

/// Every git-rsrs outcome, listed explicitly: a new upstream variant must be
/// added here (and `ALL_OUTCOMES.len()` bumped) rather than silently skipped.
const ALL_OUTCOMES: [RsrsOutcome; 12] = [
    RsrsOutcome::Synced,
    RsrsOutcome::Noop,
    RsrsOutcome::BranchSkipped,
    RsrsOutcome::Deferred,
    RsrsOutcome::ValidationRejected,
    RsrsOutcome::CheckFailed,
    RsrsOutcome::ManualIntervention,
    RsrsOutcome::LockTimeout,
    RsrsOutcome::Offline,
    RsrsOutcome::TimedOut,
    RsrsOutcome::AuthenticationFailed,
    RsrsOutcome::Failed,
];

/// Compile-time guard: no wildcard arm, so a new upstream `Outcome` variant
/// fails to compile here until it is added to `ALL_OUTCOMES`.
#[allow(dead_code)]
fn listed(outcome: RsrsOutcome) {
    match outcome {
        RsrsOutcome::Synced
        | RsrsOutcome::Noop
        | RsrsOutcome::BranchSkipped
        | RsrsOutcome::Deferred
        | RsrsOutcome::ValidationRejected
        | RsrsOutcome::CheckFailed
        | RsrsOutcome::ManualIntervention
        | RsrsOutcome::LockTimeout
        | RsrsOutcome::Offline
        | RsrsOutcome::TimedOut
        | RsrsOutcome::AuthenticationFailed
        | RsrsOutcome::Failed => {}
    }
}

fn report(outcome: RsrsOutcome, hook_failures: Vec<HookFailure>) -> SyncReport {
    SyncReport {
        outcome,
        hook_failures,
        ..SyncReport::new(Trigger::Periodic)
    }
}

fn deadline_failure() -> HookFailure {
    HookFailure {
        phase: "post_integrate".to_owned(),
        timed_out: true,
        message: "deadline".to_owned(),
    }
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_520_000)
}

fn read_json(json: &str) -> StatusRead {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("status.json");
    fs::write(&path, json).expect("write");
    read_status(&path)
}

fn present(read: StatusRead) -> StatusFile {
    match read {
        StatusRead::Present(status) => status,
        other => panic!("expected Present, got {other:?}"),
    }
}

fn written_by_git_rsrs(report: &SyncReport) -> String {
    serde_json::to_string(&next(None, report, now(), 42)).expect("serialize")
}

#[test]
fn parses_what_git_rsrs_writes() {
    let json = written_by_git_rsrs(&report(RsrsOutcome::CheckFailed, vec![deadline_failure()]));
    let status = present(read_json(&json));

    assert_eq!(status.version, 1);
    assert_eq!(status.last_outcome, Outcome::CheckFailed);
    assert_eq!(status.last_attempt_at, now());
    assert_eq!(status.attempt_seq, 1);
    assert_eq!(
        status.recent,
        [RecentAttempt {
            seq: 1,
            healthy: false
        }]
    );
    assert_eq!(status.last_success_at, None);
    assert_eq!(status.message, None);
    assert_eq!(status.display_message(), Some("deadline"));
    assert_eq!(status.pid, 42);
    assert_eq!(status.consecutive_failures, 1);
    assert_eq!(status.consecutive_successes, 0);
    assert_eq!(status.hook_failures.len(), 1);
    assert_eq!(status.hook_failures[0].phase, "post_integrate");
    assert!(status.hook_failures[0].timed_out);
    assert_eq!(
        status.counters_since,
        humantime::format_rfc3339_seconds(now()).to_string()
    );

    let synced = present(read_json(&written_by_git_rsrs(&report(
        RsrsOutcome::Synced,
        vec![],
    ))));
    assert_eq!(synced.last_success_at, Some(now()));
}

#[test]
fn health_matches_git_rsrs() {
    for outcome in ALL_OUTCOMES {
        for failures in [vec![], vec![deadline_failure()]] {
            let report = report(outcome, failures);
            let ours = present(read_json(&written_by_git_rsrs(&report))).latest_health();
            let expected = match health(&report) {
                Health::Healthy => AttemptHealth::Healthy,
                Health::Unhealthy => AttemptHealth::Unhealthy,
                Health::Uncounted => AttemptHealth::Uncounted,
            };
            assert_eq!(
                ours, expected,
                "{outcome:?} with {:?}",
                report.hook_failures
            );
        }
    }
}

#[test]
fn outcome_names_match_git_rsrs() {
    for outcome in ALL_OUTCOMES {
        let json = written_by_git_rsrs(&report(outcome, vec![]));
        assert_eq!(
            present(read_json(&json)).last_outcome.as_str(),
            outcome.as_str()
        );
    }
    assert_eq!(Outcome::Unknown.as_str(), "unknown");
}

#[test]
fn missing_and_garbage() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(matches!(
        read_status(&tmp.path().join("absent.json")),
        StatusRead::Missing
    ));
    assert!(matches!(read_status(tmp.path()), StatusRead::Unreadable(_)));
    assert!(matches!(read_json("{not json"), StatusRead::Unreadable(_)));

    let v2 = r#"{"version": 2, "entirely": "different"}"#;
    match read_json(v2) {
        StatusRead::Unreadable(reason) => {
            assert!(reason.contains("unsupported status version 2"), "{reason}")
        }
        other => panic!("expected Unreadable, got {other:?}"),
    }

    let valid = written_by_git_rsrs(&report(RsrsOutcome::Synced, vec![]));
    let mut value: serde_json::Value = serde_json::from_str(&valid).expect("json");
    let object = value.as_object_mut().expect("object");
    object.remove("pid");
    assert!(matches!(
        read_json(&value.to_string()),
        StatusRead::Unreadable(_)
    ));

    let mut value: serde_json::Value = serde_json::from_str(&valid).expect("json");
    value["last_attempt_at"] = serde_json::json!("yesterday");
    assert!(matches!(
        read_json(&value.to_string()),
        StatusRead::Unreadable(_)
    ));
}

#[test]
fn forward_compatible() {
    let valid = written_by_git_rsrs(&report(RsrsOutcome::Synced, vec![]));
    let mut value: serde_json::Value = serde_json::from_str(&valid).expect("json");
    value["new_field"] = serde_json::json!(1);
    value["last_outcome"] = serde_json::json!("rate_limited");
    let status = present(read_json(&value.to_string()));
    assert_eq!(status.last_outcome, Outcome::Unknown);
    assert_eq!(status.latest_health(), AttemptHealth::Uncounted);
}

#[test]
fn display_message_prefers_a_nonempty_message() {
    let base = present(read_json(&written_by_git_rsrs(&report(
        RsrsOutcome::CheckFailed,
        vec![deadline_failure()],
    ))));
    let with_message = StatusFile {
        message: Some("push rejected".to_owned()),
        ..base.clone()
    };
    assert_eq!(with_message.display_message(), Some("push rejected"));
    let empty = StatusFile {
        message: Some(String::new()),
        ..base.clone()
    };
    assert_eq!(empty.display_message(), Some("deadline"));
    let nothing = StatusFile {
        hook_failures: vec![],
        ..base
    };
    assert_eq!(nothing.display_message(), None);
}
