mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

use rsrsbar::heartbeat::{Action, AlertKind, Heartbeat, WatchState, decide};
use rsrsbar::{Authorization, Recording, commands};

use common::{Fixture, now, script};

const SEC: Duration = Duration::from_secs(1);
const MIN: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);
const APP: &str = "com.nicecab.rsrsbar";

fn beat(at: SystemTime, notifications: Authorization) -> Heartbeat {
    Heartbeat {
        at,
        pid: 4242,
        notifications,
    }
}

fn fresh(at: SystemTime) -> Heartbeat {
    beat(at - MIN, Authorization::Authorized)
}

fn stale(at: SystemTime) -> Heartbeat {
    beat(at - 91 * SEC, Authorization::Authorized)
}

/// `decide` long after the last wake, with the app job loaded.
fn run(
    at: SystemTime,
    heartbeat: Option<&Heartbeat>,
    state: &WatchState,
) -> (Vec<Action>, WatchState) {
    decide(at, at - DAY, heartbeat, true, state)
}

#[test]
fn grace_after_wake() {
    let state = WatchState {
        restarted_at: Some(now() - HOUR),
        ..WatchState::default()
    };
    let (actions, next) = decide(now(), now() - 89 * SEC, Some(&stale(now())), true, &state);
    assert_eq!(actions, vec![Action::Nothing]);
    assert_eq!(next, state);
    let (actions, _) = decide(now(), now() - 89 * SEC, None, false, &WatchState::default());
    assert_eq!(actions, vec![Action::Nothing]);

    // Out of grace the same stale heartbeat is judged.
    let (actions, _) = decide(now(), now() - 90 * SEC, Some(&stale(now())), true, &state);
    assert_eq!(actions, vec![Action::Alert(AlertKind::Stuck)]);
}

#[test]
fn fresh_clears() {
    let state = WatchState {
        restarted_at: Some(now() - HOUR),
        alerted: [
            (AlertKind::Stuck, now() - HOUR),
            (AlertKind::NotAuthorized, now() - HOUR),
        ]
        .into(),
    };
    // Exactly 90 s old is still fresh (stale means older than that).
    let edge = beat(now() - 90 * SEC, Authorization::Authorized);
    let (actions, next) = run(now(), Some(&edge), &state);
    assert_eq!(actions, vec![Action::Nothing]);
    assert_eq!(next, WatchState::default());

    let (actions, next) = run(now(), Some(&fresh(now())), &WatchState::default());
    assert_eq!(actions, vec![Action::Nothing]);
    assert_eq!(next, WatchState::default());
}

#[test]
fn stale_kickstarts_once_then_alerts() {
    let t1 = now();
    let (actions, s1) = run(t1, Some(&stale(t1)), &WatchState::default());
    assert_eq!(actions, vec![Action::Kickstart]);
    assert_eq!(s1.restarted_at, Some(t1));

    let t2 = t1 + 15 * MIN;
    let (actions, s2) = run(t2, Some(&stale(t1)), &s1);
    assert_eq!(actions, vec![Action::Alert(AlertKind::Stuck)]);
    assert_eq!(s2.alerted.get(&AlertKind::Stuck), Some(&t2));

    let t3 = t2 + 6 * HOUR - SEC;
    let (actions, s3) = run(t3, Some(&stale(t1)), &s2);
    assert_eq!(actions, vec![Action::Nothing]);
    assert_eq!(s3, s2);

    let t4 = t2 + 6 * HOUR;
    let (actions, s4) = run(t4, Some(&stale(t1)), &s3);
    assert_eq!(actions, vec![Action::Alert(AlertKind::Stuck)]);
    assert_eq!(s4.alerted.get(&AlertKind::Stuck), Some(&t4));
    assert_eq!(
        s4.restarted_at,
        Some(t1),
        "no second restart within one episode"
    );
}

#[test]
fn app_not_loaded_bootstraps() {
    let t1 = now();
    let (actions, s1) = decide(
        t1,
        t1 - DAY,
        Some(&stale(t1)),
        false,
        &WatchState::default(),
    );
    assert_eq!(actions, vec![Action::Bootstrap]);
    assert_eq!(s1.restarted_at, Some(t1));

    let t2 = t1 + 15 * MIN;
    let (actions, _) = decide(t2, t2 - DAY, Some(&stale(t1)), false, &s1);
    assert_eq!(actions, vec![Action::Alert(AlertKind::Stuck)]);
}

#[test]
fn missing_heartbeat_is_stale() {
    let (actions, state) = run(now(), None, &WatchState::default());
    assert_eq!(actions, vec![Action::Kickstart]);
    assert_eq!(state.restarted_at, Some(now()));

    let (actions, _) = run(now() + 15 * MIN, None, &state);
    assert_eq!(actions, vec![Action::Alert(AlertKind::Stuck)]);
}

#[test]
fn not_authorized_alerts_with_repeat() {
    for notifications in [
        Authorization::Denied,
        Authorization::NotDetermined,
        Authorization::Unknown,
    ] {
        let t1 = now();
        let off = |at: SystemTime| beat(at - MIN, notifications);
        let (actions, s1) = run(t1, Some(&off(t1)), &WatchState::default());
        assert_eq!(actions, vec![Action::Alert(AlertKind::NotAuthorized)]);
        assert_eq!(s1.alerted.get(&AlertKind::NotAuthorized), Some(&t1));

        let t2 = t1 + 6 * HOUR - SEC;
        let (actions, s2) = run(t2, Some(&off(t2)), &s1);
        assert_eq!(actions, vec![Action::Nothing]);
        assert_eq!(s2, s1);

        let t3 = t1 + 6 * HOUR;
        let (actions, s3) = run(t3, Some(&off(t3)), &s2);
        assert_eq!(actions, vec![Action::Alert(AlertKind::NotAuthorized)]);

        // Authorized again: the episode clears, so the next denial alerts at once.
        let t4 = t3 + MIN;
        let (actions, s4) = run(t4, Some(&fresh(t4)), &s3);
        assert_eq!(actions, vec![Action::Nothing]);
        assert_eq!(s4, WatchState::default());
        let (actions, _) = run(t4 + MIN, Some(&off(t4 + MIN)), &s4);
        assert_eq!(actions, vec![Action::Alert(AlertKind::NotAuthorized)]);
    }
}

#[test]
fn recovery_clears_episode() {
    let t1 = now();
    let (actions, s1) = run(t1, Some(&stale(t1)), &WatchState::default());
    assert_eq!(actions, vec![Action::Kickstart]);

    let t2 = t1 + 15 * MIN;
    let (actions, s2) = run(t2, Some(&fresh(t2)), &s1);
    assert_eq!(actions, vec![Action::Nothing]);
    assert_eq!(s2, WatchState::default());

    let t3 = t2 + HOUR;
    let (actions, s3) = run(t3, Some(&stale(t3)), &s2);
    assert_eq!(
        actions,
        vec![Action::Kickstart],
        "a new episode restarts first"
    );
    assert_eq!(s3.restarted_at, Some(t3));
}

/// Serializes every test that reads or changes the environment or spawns
/// a process; the other tests only call `decide`.
static ENV: Mutex<()> = Mutex::new(());

/// A fixture whose `PATH` starts with a fake `osascript` that writes each
/// argument on its own line to the returned file, then exits `exit_code`.
/// The environment stays this test's while the guard lives.
fn with_fake_osascript(exit_code: i32) -> (MutexGuard<'static, ()>, Fixture, PathBuf) {
    static ORIGINAL_PATH: OnceLock<String> = OnceLock::new();
    let guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let fx = Fixture::new();
    let bin = fx.root().join("fake-bin");
    let argv = fx.root().join("osascript-argv");
    script(
        &bin.join("osascript"),
        &format!(
            "for arg in \"$@\"; do printf '%s\\n' \"$arg\"; done >> '{}'\nexit {exit_code}",
            argv.display()
        ),
    );
    let original = ORIGINAL_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
    // SAFETY: every environment access in this test binary happens while
    // holding `ENV`, so no other thread reads or writes it concurrently.
    unsafe { std::env::set_var("PATH", format!("{}:{original}", bin.display())) };
    (guard, fx, argv)
}

fn heartbeat_file(fx: &Fixture) -> PathBuf {
    fx.paths.heartbeat()
}

#[test]
fn check_heartbeat_performs_actions() {
    let (_env, fx, argv) = with_fake_osascript(0);

    let awake = Some(now() - DAY);
    let launchd = Recording::new().with_loaded(APP, Some(4242));
    stale(now())
        .save(
            &fx.paths
                .home
                .join("Library/Application Support/rsrsbar/heartbeat.json"),
        )
        .expect("save heartbeat");

    // Run 1: stale → kickstart, no alert, state saved.
    let report = commands::check_heartbeat(&fx.ctx(&launchd), awake).expect("run 1");
    assert_eq!(report.exit_code, 0);
    assert_eq!(launchd.calls(), vec![format!("kickstart {APP}")]);
    assert!(!argv.exists(), "no alert on the first stale run");
    let state_file = fx
        .paths
        .home
        .join("Library/Application Support/rsrsbar/heartbeat-state.json");
    assert_eq!(WatchState::load(&state_file).restarted_at, Some(now()));

    // Run 2: still stale → osascript alert, the text passed as an argument.
    let ctx = commands::Ctx {
        now: now() + 15 * MIN,
        ..fx.ctx(&launchd)
    };
    commands::check_heartbeat(&ctx, awake).expect("run 2");
    assert_eq!(launchd.calls().len(), 1, "no second restart");
    let args = fs::read_to_string(&argv).expect("osascript ran");
    let args: Vec<&str> = args.lines().collect();
    let text = "rsrsbar is not responding; it was restarted and is still stuck";
    assert_eq!(args.last(), Some(&text), "{args:?}");
    let sources: Vec<&str> = args
        .windows(2)
        .filter(|pair| pair[0] == "-e")
        .map(|pair| pair[1])
        .collect();
    assert!(!sources.is_empty(), "{args:?}");
    assert!(
        sources.iter().all(|source| !source.contains(text)),
        "{args:?}"
    );
    assert!(
        sources
            .iter()
            .any(|source| source.contains("item 1 of argv")),
        "{args:?}"
    );
    fs::remove_file(&argv).expect("reset argv");

    // App job not loaded: bootstrap its LaunchAgent instead of a kickstart.
    let unloaded = Recording::new();
    fs::remove_file(&state_file).expect("reset state");
    commands::check_heartbeat(&fx.ctx(&unloaded), awake).expect("bootstrap run");
    let plist = fx
        .paths
        .home
        .join("Library/LaunchAgents/com.nicecab.rsrsbar.plist");
    assert_eq!(
        unloaded.calls(),
        vec![format!("bootstrap {}", plist.display())]
    );

    // Fresh but not authorized: the notifications alert, no launchd call.
    let quiet = Recording::new().with_loaded(APP, Some(4242));
    beat(now() - MIN, Authorization::Denied)
        .save(
            &fx.paths
                .home
                .join("Library/Application Support/rsrsbar/heartbeat.json"),
        )
        .expect("save heartbeat");
    commands::check_heartbeat(&fx.ctx(&quiet), awake).expect("not authorized run");
    assert!(quiet.calls().is_empty());
    let args = fs::read_to_string(&argv).expect("osascript ran");
    assert_eq!(
        args.lines().last(),
        Some("rsrsbar cannot show notifications; allow them in System Settings → Notifications")
    );
}

#[test]
fn failed_restart_still_reaches_alert() {
    let (_env, fx, argv) = with_fake_osascript(0);
    let awake = Some(now() - DAY);
    let launchd = Recording::new().with_loaded(APP, Some(4242));
    launchd.fail_next_kickstart();
    stale(now())
        .save(&heartbeat_file(&fx))
        .expect("save heartbeat");

    let err = commands::check_heartbeat(&fx.ctx(&launchd), awake).expect_err("kickstart fails");
    assert!(err.to_string().contains("kickstart"), "{err}");
    let state = WatchState::load(&fx.paths.heartbeat_state());
    assert_eq!(state.restarted_at, Some(now()), "the attempt is recorded");

    let ctx = commands::Ctx {
        now: now() + 15 * MIN,
        ..fx.ctx(&launchd)
    };
    commands::check_heartbeat(&ctx, awake).expect("run 2 alerts");
    assert_eq!(launchd.calls().len(), 1, "no second restart");
    let args = fs::read_to_string(&argv).expect("osascript ran");
    assert_eq!(args.lines().last(), Some(AlertKind::Stuck.text()));
}

#[test]
fn failed_alert_is_not_recorded() {
    let (_env, fx, argv) = with_fake_osascript(1);
    let awake = Some(now() - DAY);
    let launchd = Recording::new().with_loaded(APP, Some(4242));
    beat(now() - MIN, Authorization::Denied)
        .save(&heartbeat_file(&fx))
        .expect("save heartbeat");
    let earlier = WatchState {
        restarted_at: Some(now() - HOUR),
        alerted: [(AlertKind::NotAuthorized, now() - 7 * HOUR)].into(),
    };
    earlier
        .save(&fx.paths.heartbeat_state())
        .expect("save state");

    let err = commands::check_heartbeat(&fx.ctx(&launchd), awake).expect_err("osascript fails");
    assert!(err.to_string().contains("osascript"), "{err}");
    assert!(argv.exists(), "the alert was attempted");
    let state = WatchState::load(&fx.paths.heartbeat_state());
    assert_eq!(
        state,
        WatchState {
            restarted_at: None,
            alerted: [(AlertKind::NotAuthorized, now() - 7 * HOUR)].into(),
        },
        "the fresh heartbeat still clears the episode; the failed alert is not recorded"
    );
}

#[test]
fn unknown_wake_time_means_no_grace() {
    let _env = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let fx = Fixture::new();
    let launchd = Recording::new().with_loaded(APP, Some(4242));
    stale(now())
        .save(&heartbeat_file(&fx))
        .expect("save heartbeat");

    // The wake time could not be read: judge at once rather than not at all.
    let report = commands::check_heartbeat(&fx.ctx(&launchd), None).expect("decides");
    assert_eq!(launchd.calls(), vec![format!("kickstart {APP}")]);
    assert!(report.text.contains("wake time unknown"), "{}", report.text);
}

#[test]
fn saved_times_keep_subsecond_precision() {
    let _env = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let at = now() + Duration::from_millis(900);

    let heartbeat = beat(at, Authorization::Authorized);
    let path = dir.path().join("heartbeat.json");
    heartbeat.save(&path).expect("save heartbeat");
    let loaded = Heartbeat::load(&path).expect("load heartbeat");
    assert_eq!(loaded, heartbeat);
    // 89.2 s after the beat: still fresh after the round trip.
    let later = at + 89 * SEC + Duration::from_millis(200);
    let (actions, _) = run(later, Some(&loaded), &WatchState::default());
    assert_eq!(actions, vec![Action::Nothing]);

    let state = WatchState {
        restarted_at: Some(at),
        alerted: [(AlertKind::Stuck, at)].into(),
    };
    let path = dir.path().join("heartbeat-state.json");
    state.save(&path).expect("save state");
    let loaded = WatchState::load(&path);
    assert_eq!(loaded, state);
    // 0.2 s short of six hours after the alert: not repeated yet.
    let early = at + 6 * HOUR - Duration::from_millis(200);
    let (actions, _) = run(early, None, &loaded);
    assert_eq!(actions, vec![Action::Nothing]);
}
