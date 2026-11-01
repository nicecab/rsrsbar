use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use rsrsbar::{
    Color, Health, HookFailure, JobState, Notify, Observation, Outcome, RecentAttempt,
    RecoveryPoint, RepoEntry, RepoId, RepoMemory, Row, Settings, StatusFile, StatusRead, classify,
    failure_run, unhealthy_in_window,
};

const MIN: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);
const SEC: Duration = Duration::from_secs(1);

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_500_000)
}

fn settings() -> Settings {
    Settings {
        interval: 10 * MIN,
        quiet_period: 30 * SEC,
        git_timeout: 120 * SEC,
        lock_timeout: 30 * SEC,
        lock_name: None,
        stall_threshold: 30 * MIN,
        paused_limit: DAY,
        offline_limit: 2 * HOUR,
        other_branch_limit: DAY,
    }
}

fn status() -> StatusFile {
    StatusFile {
        version: 1,
        last_attempt_at: now() - MIN,
        last_outcome: Outcome::Noop,
        last_success_at: Some(now() - MIN),
        message: None,
        pid: 42,
        git_rsrs_version: "0.2.0".to_owned(),
        hook_failures: Vec::new(),
        counters_since: humantime::format_rfc3339_seconds(now() - 7 * DAY).to_string(),
        attempt_seq: 5,
        consecutive_failures: 0,
        consecutive_successes: 5,
        recent: (1..=5)
            .rev()
            .map(|seq| RecentAttempt { seq, healthy: true })
            .collect(),
    }
}

fn with_outcome(outcome: Outcome) -> StatusRead {
    present(StatusFile {
        last_outcome: outcome,
        ..status()
    })
}

/// The fixture's observation inputs, owned so tests can tweak them.
struct Fixture {
    awake_since: SystemTime,
    settings: Settings,
    paused_at: Option<SystemTime>,
    hooks_trusted: bool,
    job: JobState,
    status: StatusRead,
    current_branch: Option<&'static str>,
    memory: RepoMemory,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            awake_since: now() - DAY,
            settings: settings(),
            paused_at: None,
            hooks_trusted: true,
            job: JobState::Running { up_for: DAY },
            status: StatusRead::Present(status()),
            current_branch: Some("main"),
            memory: RepoMemory::default(),
        }
    }

    fn with(status: StatusRead) -> Fixture {
        Fixture {
            status,
            ..Fixture::new()
        }
    }

    fn classify(&self) -> Health {
        classify(&Observation {
            now: now(),
            awake_since: self.awake_since,
            settings: &self.settings,
            paused_at: self.paused_at,
            hooks_trusted: self.hooks_trusted,
            job: self.job,
            status: &self.status,
            current_branch: self.current_branch,
            memory: &self.memory,
        })
    }
}

fn assert_health(health: &Health, row: Row, color: Color, notify: Notify, label: Option<&str>) {
    assert_eq!(
        (
            health.row,
            health.color,
            health.notify,
            health.label.as_deref()
        ),
        (row, color, notify, label),
        "{health:?}"
    );
}

#[test]
fn row1_paused_over_24h() {
    let f = Fixture {
        paused_at: Some(now() - DAY - SEC),
        job: JobState::NotRunning,
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::PausedLong,
        Color::Yellow,
        Notify::Once,
        Some("paused for over 24 h"),
    );
}

#[test]
fn row2_paused() {
    let f = Fixture {
        paused_at: Some(now() - DAY),
        job: JobState::NotRunning,
        status: StatusRead::Missing,
        ..Fixture::new()
    };
    assert_health(&f.classify(), Row::Paused, Color::Grey, Notify::Never, None);
}

#[test]
fn row3_beats_manual_intervention() {
    let f = Fixture {
        hooks_trusted: false,
        status: with_outcome(Outcome::ManualIntervention),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::HooksChanged,
        Color::Red,
        Notify::Immediately,
        Some("hook changed — needs allow"),
    );
}

#[test]
fn row4_manual_intervention() {
    let f = Fixture::with(with_outcome(Outcome::ManualIntervention));
    assert_health(
        &f.classify(),
        Row::ManualIntervention,
        Color::Red,
        Notify::Immediately,
        Some("manual intervention needed"),
    );
}

#[test]
fn row5_job_not_running() {
    let f = Fixture {
        job: JobState::NotRunning,
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::WatcherNotRunning,
        Color::Red,
        Notify::Immediately,
        Some("watcher not running"),
    );
}

#[test]
fn row5_status_missing_after_threshold() {
    for status in [
        StatusRead::Missing,
        StatusRead::Unreadable("garbage".to_owned()),
    ] {
        let f = Fixture {
            job: JobState::Running {
                up_for: 30 * MIN + SEC,
            },
            status,
            ..Fixture::new()
        };
        assert_health(
            &f.classify(),
            Row::WatcherNotRunning,
            Color::Red,
            Notify::Immediately,
            Some("watcher not running"),
        );
    }
}

#[test]
fn row5_missing_status_waits_out_wake_grace() {
    let f = Fixture {
        awake_since: now() - 9 * MIN,
        job: JobState::Running {
            up_for: 30 * MIN + SEC,
        },
        status: StatusRead::Missing,
        ..Fixture::new()
    };
    assert_eq!(f.classify().row, Row::Starting);

    let not_running = Fixture {
        awake_since: now() - 9 * MIN,
        job: JobState::NotRunning,
        ..Fixture::new()
    };
    assert_eq!(not_running.classify().row, Row::WatcherNotRunning);
}

#[test]
fn starting_within_threshold() {
    let f = Fixture {
        job: JobState::Running { up_for: 30 * MIN },
        status: StatusRead::Missing,
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::Starting,
        Color::Grey,
        Notify::Never,
        Some("waiting for first sync"),
    );
}

#[test]
fn row6_auth_and_failed() {
    for outcome in [Outcome::AuthenticationFailed, Outcome::Failed] {
        let f = Fixture::with(with_outcome(outcome));
        assert_health(
            &f.classify(),
            Row::SyncFailed,
            Color::Red,
            Notify::Immediately,
            Some("sync failed"),
        );
    }
}

#[test]
fn row15_healthy() {
    assert_health(
        &Fixture::new().classify(),
        Row::Healthy,
        Color::Green,
        Notify::Never,
        None,
    );
}

#[test]
fn precedence_follows_table_order() {
    let paused_untrusted = Fixture {
        paused_at: Some(now() - HOUR),
        hooks_trusted: false,
        ..Fixture::new()
    };
    assert_eq!(paused_untrusted.classify().row, Row::Paused);

    let untrusted_stopped = Fixture {
        hooks_trusted: false,
        job: JobState::NotRunning,
        ..Fixture::new()
    };
    assert_eq!(untrusted_stopped.classify().row, Row::HooksChanged);

    let manual_stopped = Fixture {
        job: JobState::NotRunning,
        status: with_outcome(Outcome::ManualIntervention),
        ..Fixture::new()
    };
    assert_eq!(manual_stopped.classify().row, Row::ManualIntervention);

    let stopped_failed = Fixture {
        job: JobState::NotRunning,
        status: with_outcome(Outcome::Failed),
        ..Fixture::new()
    };
    assert_eq!(stopped_failed.classify().row, Row::WatcherNotRunning);
}

#[test]
fn clock_skew_does_not_panic() {
    let f = Fixture {
        paused_at: Some(now() + HOUR),
        awake_since: now() + HOUR,
        ..Fixture::new()
    };
    assert_eq!(f.classify().row, Row::Paused);
    let g = Fixture {
        awake_since: now() + HOUR,
        ..Fixture::new()
    };
    assert_eq!(g.classify().row, Row::Healthy);
}

/// `recent` from a newest-first pattern of `H` / `F`, seqs counting down
/// from `newest`.
fn recent(pattern: &str, newest: u64) -> Vec<RecentAttempt> {
    pattern
        .chars()
        .zip((1..=newest).rev())
        .map(|(c, seq)| RecentAttempt {
            seq,
            healthy: c == 'H',
        })
        .collect()
}

fn hook_failure(phase: &str) -> HookFailure {
    HookFailure {
        phase: phase.to_owned(),
        timed_out: false,
        message: format!("{phase} failed"),
    }
}

/// A status with `pattern` as its `recent`, newest seq = pattern length.
fn status_with(pattern: &str, outcome: Outcome) -> StatusFile {
    let newest = pattern.len() as u64;
    StatusFile {
        last_outcome: outcome,
        attempt_seq: newest,
        consecutive_successes: pattern.chars().take_while(|c| *c == 'H').count() as u32,
        consecutive_failures: pattern.chars().take_while(|c| *c == 'F').count() as u32,
        recent: recent(pattern, newest),
        ..status()
    }
}

/// The fixture status with `outcome`, last success `ago` before `now`.
fn succeeded_ago(outcome: Outcome, ago: Duration) -> StatusRead {
    present(StatusFile {
        last_outcome: outcome,
        last_success_at: Some(now() - ago),
        ..status()
    })
}

fn present(status: StatusFile) -> StatusRead {
    StatusRead::Present(status)
}

fn red_memory(label: &str) -> RepoMemory {
    RepoMemory {
        last_row: Some(Row::FailureRun),
        last_color: Some(Color::Red),
        label: Some(label.to_owned()),
        phase: None,
        ..RepoMemory::default()
    }
}

#[test]
fn row7_two_timeouts() {
    let f = Fixture::with(present(status_with("FFHHHH", Outcome::TimedOut)));
    assert_health(
        &f.classify(),
        Row::FailureRun,
        Color::Red,
        Notify::Yes,
        Some("sync timed out"),
    );
}

#[test]
fn row7_on_result_failures() {
    let f = Fixture {
        status: present(StatusFile {
            hook_failures: vec![hook_failure("on_result")],
            ..status_with("FFHHH", Outcome::Noop)
        }),
        ..Fixture::new()
    };
    let health = f.classify();
    assert_health(
        &health,
        Row::FailureRun,
        Color::Red,
        Notify::Yes,
        Some("check failed: on_result"),
    );
    assert_eq!(health.phase.as_deref(), Some("on_result"));
}

#[test]
fn row7_survives_offline_between() {
    let f = Fixture {
        status: present(StatusFile {
            last_success_at: Some(now() - 31 * MIN),
            ..status_with("FFHHH", Outcome::Offline)
        }),
        memory: red_memory("sync timed out"),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::FailureRun,
        Color::Red,
        Notify::Yes,
        Some("sync timed out"),
    );

    let forgotten = Fixture {
        memory: RepoMemory::default(),
        ..f
    };
    assert_eq!(forgotten.classify().label.as_deref(), Some("sync failing"));
}

#[test]
fn row7_label_without_hook_failures_is_check_failed() {
    let f = Fixture::with(present(status_with("FFHHH", Outcome::CheckFailed)));
    assert_eq!(f.classify().label.as_deref(), Some("check failed"));
}

#[test]
fn row7a_intermittent() {
    let f = Fixture::with(present(status_with("HFHFHFHHHH", Outcome::Noop)));
    assert_health(
        &f.classify(),
        Row::Intermittent,
        Color::Red,
        Notify::Yes,
        Some("failing intermittently: 3 of last 10"),
    );
}

#[test]
fn row7a_newest_failure_still_intermittent() {
    let f = Fixture {
        status: present(StatusFile {
            hook_failures: vec![hook_failure("pre_commit")],
            ..status_with("FHFHFHHHHH", Outcome::CheckFailed)
        }),
        ..Fixture::new()
    };
    assert_eq!(f.classify().row, Row::Intermittent);
}

#[test]
fn row7b_stays_red() {
    let f = Fixture {
        status: present(status_with("HFFHH", Outcome::Noop)),
        memory: red_memory("sync timed out"),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::StaysRed,
        Color::Red,
        Notify::Never,
        Some("sync timed out"),
    );
}

#[test]
fn row7b_released_by_two_successes() {
    let f = Fixture {
        status: present(status_with("HHFFH", Outcome::Noop)),
        memory: red_memory("sync timed out"),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::Healthy,
        Color::Green,
        Notify::Never,
        None,
    );
}

#[test]
fn row8_single_failure() {
    let f = Fixture {
        status: present(StatusFile {
            hook_failures: vec![hook_failure("post_integrate")],
            ..status_with("FHHHH", Outcome::Noop)
        }),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::SingleFailure,
        Color::Yellow,
        Notify::Never,
        Some("check failed: post_integrate"),
    );
}

#[test]
fn recovery_point_hides_old_failures() {
    let status = status_with("FFF", Outcome::Noop);
    let memory = RepoMemory {
        recovery: Some(RecoveryPoint {
            counters_since: status.counters_since.clone(),
            seq: 3,
        }),
        ..RepoMemory::default()
    };
    let f = Fixture {
        status: present(status.clone()),
        memory,
        ..Fixture::new()
    };
    assert_eq!(f.classify().row, Row::Healthy);
    assert_eq!(failure_run(&status, Some(3)), 0);
    assert_eq!(failure_run(&status, Some(1)), 2);
    assert_eq!(failure_run(&status, None), 3);
}

#[test]
fn window_helpers_read_newest_first() {
    let status = status_with("HFHFHFHHHH", Outcome::Noop);
    assert_eq!(failure_run(&status, None), 0);
    assert_eq!(unhealthy_in_window(&status, None), 3);
    assert_eq!(unhealthy_in_window(&status, Some(7)), 1);
    let run = status_with("FFHF", Outcome::Noop);
    assert_eq!(failure_run(&run, None), 2);
    assert_eq!(unhealthy_in_window(&run, None), 3);
}

#[test]
fn row9_offline_over_2h() {
    let f = Fixture::with(succeeded_ago(Outcome::Offline, 2 * HOUR + SEC));
    assert_health(
        &f.classify(),
        Row::OfflineLong,
        Color::Red,
        Notify::Yes,
        Some("offline for over 2 h"),
    );
}

#[test]
fn row10_offline_beats_stall() {
    let f = Fixture::with(succeeded_ago(Outcome::Offline, HOUR));
    assert_health(
        &f.classify(),
        Row::Offline,
        Color::Grey,
        Notify::Never,
        None,
    );
}

#[test]
fn row11_other_branch_over_24h() {
    let f = Fixture {
        status: succeeded_ago(Outcome::BranchSkipped, DAY + SEC),
        current_branch: Some("feature/x"),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::OtherBranchLong,
        Color::Yellow,
        Notify::Once,
        Some("on feature/x, no sync for over 24 h"),
    );

    let unknown = Fixture {
        current_branch: None,
        ..f
    };
    assert_eq!(
        unknown.classify().label.as_deref(),
        Some("on another branch, no sync for over 24 h")
    );
}

#[test]
fn row12_other_branch() {
    let f = Fixture {
        status: succeeded_ago(Outcome::BranchSkipped, HOUR),
        current_branch: Some("feature/x"),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::OtherBranch,
        Color::Grey,
        Notify::Never,
        None,
    );
}

fn stalled() -> Fixture {
    Fixture {
        status: present(StatusFile {
            last_attempt_at: now() - 30 * MIN - SEC,
            last_success_at: Some(now() - 30 * MIN - SEC),
            ..status()
        }),
        ..Fixture::new()
    }
}

#[test]
fn row13_stalled() {
    assert_health(
        &stalled().classify(),
        Row::Stalled,
        Color::Red,
        Notify::Yes,
        Some("sync stalled"),
    );
    let at_threshold = Fixture {
        status: present(StatusFile {
            last_success_at: Some(now() - 30 * MIN),
            ..status()
        }),
        ..Fixture::new()
    };
    assert_eq!(at_threshold.classify().row, Row::Healthy);
}

#[test]
fn row13_lock_held() {
    let f = Fixture::with(succeeded_ago(Outcome::LockTimeout, 31 * MIN));
    assert_health(
        &f.classify(),
        Row::Stalled,
        Color::Red,
        Notify::Yes,
        Some("sync stalled (lock held)"),
    );
}

#[test]
fn row13_never_succeeded() {
    let f = Fixture {
        status: present(StatusFile {
            last_outcome: Outcome::Deferred,
            last_success_at: None,
            counters_since: humantime::format_rfc3339_seconds(now() - 31 * MIN).to_string(),
            attempt_seq: 0,
            consecutive_successes: 0,
            recent: Vec::new(),
            ..status()
        }),
        ..Fixture::new()
    };
    assert_health(
        &f.classify(),
        Row::Stalled,
        Color::Red,
        Notify::Yes,
        Some("sync stalled"),
    );
}

#[test]
fn unparsable_counters_since_counts_as_no_success_known() {
    let f = Fixture {
        status: present(StatusFile {
            last_outcome: Outcome::Deferred,
            last_success_at: None,
            counters_since: "not a time".to_owned(),
            recent: Vec::new(),
            ..status()
        }),
        ..Fixture::new()
    };
    assert_eq!(f.classify().row, Row::Stalled);
}

#[test]
fn row14_transient() {
    for outcome in [
        Outcome::LockTimeout,
        Outcome::Deferred,
        Outcome::ValidationRejected,
    ] {
        let f = Fixture::with(succeeded_ago(outcome, 5 * MIN));
        assert_health(
            &f.classify(),
            Row::Transient,
            Color::Yellow,
            Notify::Never,
            Some(outcome.as_str()),
        );
    }
}

#[test]
fn wake_grace_defers_time_rows() {
    let just_woke = now() - 9 * MIN;
    let stall = Fixture {
        awake_since: just_woke,
        ..stalled()
    };
    assert_ne!(stall.classify().row, Row::Stalled);

    let offline = Fixture {
        awake_since: just_woke,
        status: succeeded_ago(Outcome::Offline, 2 * HOUR + SEC),
        ..Fixture::new()
    };
    assert_ne!(offline.classify().row, Row::OfflineLong);

    let branch = Fixture {
        awake_since: just_woke,
        status: succeeded_ago(Outcome::BranchSkipped, DAY + SEC),
        ..Fixture::new()
    };
    assert_ne!(branch.classify().row, Row::OtherBranchLong);
}

/// A registry entry with every override left at its default.
fn plain_entry() -> RepoEntry {
    RepoEntry {
        id: RepoId::new("obaidoc").expect("valid id"),
        path: PathBuf::from("/x/obaidoc"),
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        lock_name: None,
        interval: None,
        quiet_period: None,
        git_timeout: None,
        lock_timeout: None,
        stall_threshold: None,
        paused_limit: None,
        offline_limit: None,
        other_branch_limit: None,
        paused_at: None,
        allowed_hooks: None,
    }
}

/// The paused, offline and other-branch rows for `settings`, each observed
/// `over` past its limit.
fn long_rows(settings: &Settings, over: Duration) -> [Health; 3] {
    let with = |paused_at, status| Fixture {
        settings: settings.clone(),
        paused_at,
        status,
        ..Fixture::new()
    };
    [
        with(
            Some(now() - settings.paused_limit - over),
            with_outcome(Outcome::Noop),
        ),
        with(
            None,
            succeeded_ago(Outcome::Offline, settings.offline_limit + over),
        ),
        with(
            None,
            succeeded_ago(Outcome::BranchSkipped, settings.other_branch_limit + over),
        ),
    ]
    .map(|f| f.classify())
}

#[test]
fn long_limit_defaults() {
    let defaults = plain_entry().settings().expect("defaults parse");
    assert_eq!(defaults.paused_limit, DAY);
    assert_eq!(defaults.offline_limit, 2 * HOUR);
    assert_eq!(defaults.other_branch_limit, DAY);

    let [paused, offline, branch] = long_rows(&defaults, SEC);
    assert_health(
        &paused,
        Row::PausedLong,
        Color::Yellow,
        Notify::Once,
        Some("paused for over 24 h"),
    );
    assert_health(
        &offline,
        Row::OfflineLong,
        Color::Red,
        Notify::Yes,
        Some("offline for over 2 h"),
    );
    assert_health(
        &branch,
        Row::OtherBranchLong,
        Color::Yellow,
        Notify::Once,
        Some("on main, no sync for over 24 h"),
    );

    let [paused, offline, branch] = long_rows(&defaults, Duration::ZERO);
    assert_eq!(
        [paused.row, offline.row, branch.row],
        [Row::Paused, Row::Offline, Row::OtherBranch]
    );
}

#[test]
fn long_limit_overrides() {
    let entry = RepoEntry {
        paused_limit: Some("5m".to_owned()),
        offline_limit: Some("10m".to_owned()),
        other_branch_limit: Some("90m".to_owned()),
        ..plain_entry()
    };
    let settings = entry.settings().expect("overrides parse");
    assert_eq!(settings.paused_limit, 5 * MIN);
    assert_eq!(settings.offline_limit, 10 * MIN);
    assert_eq!(settings.other_branch_limit, 90 * MIN);

    let [paused, offline, branch] = long_rows(&settings, SEC);
    assert_health(
        &paused,
        Row::PausedLong,
        Color::Yellow,
        Notify::Once,
        Some("paused for over 5 min"),
    );
    assert_health(
        &offline,
        Row::OfflineLong,
        Color::Red,
        Notify::Yes,
        Some("offline for over 10 min"),
    );
    assert_health(
        &branch,
        Row::OtherBranchLong,
        Color::Yellow,
        Notify::Once,
        Some("on main, no sync for over 90 min"),
    );

    let [paused, offline, branch] = long_rows(&settings, Duration::ZERO);
    assert_eq!(
        [paused.row, offline.row, branch.row],
        [Row::Paused, Row::Offline, Row::OtherBranch]
    );

    let bad = RepoEntry {
        offline_limit: Some("soon".to_owned()),
        ..plain_entry()
    };
    let err = bad.settings().expect_err("bad limit must fail");
    assert!(err.to_string().contains("offline_limit"), "{err}");

    // Zero, and anything finer than whole seconds, is refused: a label
    // could not name it exactly.
    for value in ["0s", "1500ms", "1m 1ns"] {
        let entries = [
            (
                "paused_limit",
                RepoEntry {
                    paused_limit: Some(value.to_owned()),
                    ..plain_entry()
                },
            ),
            (
                "offline_limit",
                RepoEntry {
                    offline_limit: Some(value.to_owned()),
                    ..plain_entry()
                },
            ),
            (
                "other_branch_limit",
                RepoEntry {
                    other_branch_limit: Some(value.to_owned()),
                    ..plain_entry()
                },
            ),
        ];
        for (key, entry) in entries {
            let err = entry.settings().expect_err("limit must be refused");
            let text = err.to_string();
            assert!(
                text.contains("obaidoc") && text.contains(key),
                "{value}: {text}"
            );
        }
    }
    let ninety_s = RepoEntry {
        offline_limit: Some("90s".to_owned()),
        ..plain_entry()
    };
    let settings = ninety_s.settings().expect("whole seconds parse");
    let [_, offline, _] = long_rows(&settings, SEC);
    assert_eq!(offline.label.as_deref(), Some("offline for over 90 s"));
}
