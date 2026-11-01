mod common;

use std::path::{Path, PathBuf};

use common::{Fixture, plist_dict as dict, plist_string as string};
use plist::Value;
use rsrsbar::commands::{Ctx, start, stop};
use rsrsbar::{Paths, Recording, app_plist, heartbeat_plist};

const BUNDLE: &str = "/Users/t/Applications/rsrsbar.app";

fn arguments(dict: &plist::Dictionary) -> Vec<&str> {
    dict.get("ProgramArguments")
        .and_then(Value::as_array)
        .expect("ProgramArguments")
        .iter()
        .map(|v| v.as_string().expect("string argument"))
        .collect()
}

#[test]
fn app_plist_shape() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let plist = dict(&app_plist(Path::new(BUNDLE), &paths).expect("plist"));
    assert_eq!(string(&plist, "Label"), "com.nicecab.rsrsbar");
    assert_eq!(
        arguments(&plist),
        [format!("{BUNDLE}/Contents/MacOS/rsrsbar").as_str(), "app"]
    );
    assert_eq!(plist.get("KeepAlive"), Some(&Value::Boolean(true)));
    assert_eq!(plist.get("RunAtLoad"), Some(&Value::Boolean(true)));
    assert_eq!(string(&plist, "ProcessType"), "Interactive");
    assert_eq!(
        string(&plist, "StandardOutPath"),
        "/Users/t/Library/Logs/com.nicecab.rsrsbar.log"
    );
    assert_eq!(
        string(&plist, "StandardErrorPath"),
        "/Users/t/Library/Logs/com.nicecab.rsrsbar.error.log"
    );
}

#[test]
fn heartbeat_plist_shape() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let plist = dict(&heartbeat_plist(Path::new(BUNDLE), &paths).expect("plist"));
    assert_eq!(string(&plist, "Label"), "com.nicecab.rsrsbar.heartbeat");
    assert_eq!(
        arguments(&plist),
        [
            format!("{BUNDLE}/Contents/MacOS/rsrsbar").as_str(),
            "check-heartbeat"
        ]
    );
    assert_eq!(
        plist.get("StartInterval"),
        Some(&Value::Integer(900.into()))
    );
    assert_eq!(plist.get("RunAtLoad"), Some(&Value::Boolean(false)));
    assert_eq!(plist.get("KeepAlive"), None);
    assert_eq!(
        string(&plist, "StandardOutPath"),
        "/Users/t/Library/Logs/com.nicecab.rsrsbar.heartbeat.log"
    );
    assert_eq!(
        string(&plist, "StandardErrorPath"),
        "/Users/t/Library/Logs/com.nicecab.rsrsbar.heartbeat.error.log"
    );
}

#[test]
fn start_refuses_outside_bundle() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let ctx = Ctx {
        rsrsbar_exe: PathBuf::from("/x/rsrsbar/target/release/rsrsbar"),
        ..fx.ctx(&launchd)
    };
    let err = start(&ctx).expect_err("outside a bundle");
    assert!(
        err.to_string()
            .contains("run rsrsbar start from rsrsbar.app/Contents/MacOS/rsrsbar"),
        "{err}"
    );
    assert!(launchd.calls().is_empty(), "{:?}", launchd.calls());
    assert!(fx.home_files().is_empty(), "{:?}", fx.home_files());
}

#[test]
fn start_then_stop_order() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);
    let agents = fx.paths.home.join("Library/LaunchAgents");
    let app = agents.join("com.nicecab.rsrsbar.plist");
    let heartbeat = agents.join("com.nicecab.rsrsbar.heartbeat.plist");

    start(&ctx).expect("start");
    assert_eq!(
        launchd.calls(),
        [
            format!("bootstrap {}", app.display()),
            format!("bootstrap {}", heartbeat.display()),
        ]
    );
    let written = dict(&std::fs::read(&app).expect("app plist written"));
    assert_eq!(arguments(&written)[0], common::RSRSBAR_EXE);
    assert!(heartbeat.exists());

    // Already loaded: restarted rather than bootstrapped twice.
    start(&ctx).expect("start again");
    assert_eq!(
        launchd.calls()[2..],
        [
            "kickstart com.nicecab.rsrsbar".to_owned(),
            "kickstart com.nicecab.rsrsbar.heartbeat".to_owned(),
        ]
    );

    let report = stop(&ctx).expect("stop");
    assert_eq!(report.text, "rsrsbar stopped; repositories keep syncing");
    assert_eq!(
        launchd.calls()[4..],
        [
            "bootout com.nicecab.rsrsbar.heartbeat".to_owned(),
            "bootout com.nicecab.rsrsbar".to_owned(),
        ]
    );
}

#[test]
fn start_reloads_a_changed_plist() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    start(&fx.ctx(&launchd)).expect("first start");
    let agents = fx.paths.home.join("Library/LaunchAgents");

    // The bundle moved: both plists change, so the loaded jobs are reloaded
    // (a kickstart would keep running the old plist).
    let moved = Ctx {
        rsrsbar_exe: PathBuf::from("/Users/t/Other/rsrsbar.app/Contents/MacOS/rsrsbar"),
        ..fx.ctx(&launchd)
    };
    start(&moved).expect("start from the moved bundle");
    assert_eq!(
        launchd.calls()[2..],
        [
            "bootout com.nicecab.rsrsbar".to_owned(),
            format!(
                "bootstrap {}",
                agents.join("com.nicecab.rsrsbar.plist").display()
            ),
            "bootout com.nicecab.rsrsbar.heartbeat".to_owned(),
            format!(
                "bootstrap {}",
                agents.join("com.nicecab.rsrsbar.heartbeat.plist").display()
            ),
        ]
    );
    let written = dict(&std::fs::read(agents.join("com.nicecab.rsrsbar.plist")).expect("plist"));
    assert_eq!(
        arguments(&written)[0],
        "/Users/t/Other/rsrsbar.app/Contents/MacOS/rsrsbar"
    );

    // Unchanged since: a plain restart.
    start(&moved).expect("start again");
    assert_eq!(
        launchd.calls()[6..],
        [
            "kickstart com.nicecab.rsrsbar".to_owned(),
            "kickstart com.nicecab.rsrsbar.heartbeat".to_owned(),
        ]
    );
}

#[test]
fn failed_reload_is_retried() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    start(&fx.ctx(&launchd)).expect("first start");
    let app = fx
        .paths
        .home
        .join("Library/LaunchAgents/com.nicecab.rsrsbar.plist");
    let before = std::fs::read_to_string(&app).expect("plist");

    let moved = Ctx {
        rsrsbar_exe: PathBuf::from("/Users/t/Other/rsrsbar.app/Contents/MacOS/rsrsbar"),
        ..fx.ctx(&launchd)
    };
    launchd.fail_next_bootstrap();
    start(&moved).expect_err("the reload fails");
    assert_eq!(
        std::fs::read_to_string(&app).expect("plist"),
        before,
        "the old plist is put back, so the change is seen again"
    );

    let calls_before = launchd.calls().len();
    start(&moved).expect("retry");
    let retry = &launchd.calls()[calls_before..];
    assert_eq!(
        retry.first().map(String::as_str),
        Some(format!("bootstrap {}", app.display()).as_str()),
        "{retry:?}"
    );
    assert!(
        !retry
            .iter()
            .any(|call| call == "kickstart com.nicecab.rsrsbar"),
        "{retry:?}"
    );
    let written = dict(&std::fs::read(&app).expect("plist"));
    assert_eq!(
        arguments(&written)[0],
        "/Users/t/Other/rsrsbar.app/Contents/MacOS/rsrsbar"
    );
}
