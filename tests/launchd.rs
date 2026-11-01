use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rsrsbar::{JobInfo, Launchctl, Launchd, Recording, parse_print};

const LABEL: &str = "com.nicecab.rsrsbar.watch.demo";
const SELFTEST: &str = "com.nicecab.rsrsbar.watch.selftest";

// Trimmed real `launchctl print gui/501/<label>` output; the nested
// `pid = 99` inside a block must be ignored.
const PRINT_RUNNING: &str = "gui/501/com.nicecab.rsrsbar.watch.demo = {
\tactive count = 1
\tpath = /Users/tom/Library/LaunchAgents/com.nicecab.rsrsbar.watch.demo.plist
\ttype = LaunchAgent
\tstate = running

\tprogram = /opt/homebrew/bin/git-rsrs
\targuments = {
\t\t/opt/homebrew/bin/git-rsrs
\t\twatch
\t}

\tspawn = {
\t\tpid = 99
\t}

\tdomain = gui/501 [100017]
\tasid = 100017
\truns = 1
\tpid = 4321
\timmediate reason = speculative
\tlast exit code = (never exited)
}
";

const PRINT_NOT_RUNNING: &str = "gui/501/com.nicecab.rsrsbar.watch.demo = {
\tactive count = 0
\tpath = /Users/tom/Library/LaunchAgents/com.nicecab.rsrsbar.watch.demo.plist
\ttype = LaunchAgent
\tstate = not running

\tspawn = {
\t\tpid = 99
\t}

\tdomain = gui/501 [100017]
\tasid = 100017
\truns = 3
\tlast exit code = 1
}
";

fn plist_path(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!("{label}.plist"))
}

#[test]
fn parse_print_running() {
    assert_eq!(parse_print(PRINT_RUNNING), JobInfo { pid: Some(4321) });
}

#[test]
fn parse_print_not_running() {
    assert_eq!(parse_print(PRINT_NOT_RUNNING), JobInfo { pid: None });
    assert_eq!(parse_print(""), JobInfo { pid: None });
}

#[test]
fn recording_double() {
    let plist = plist_path(Path::new("/tmp/agents"), LABEL);
    let launchd = Recording::new();

    assert_eq!(launchd.job(LABEL).expect("job"), None);
    launchd.bootstrap(&plist).expect("bootstrap");
    assert!(launchd.job(LABEL).expect("job").is_some());

    launchd
        .bootout("com.nicecab.rsrsbar.watch.unknown")
        .expect("bootout of unknown is ok");

    launchd.bootout(LABEL).expect("bootout");
    launchd.fail_next_bootstrap();
    assert!(launchd.bootstrap(&plist).is_err());
    assert_eq!(launchd.job(LABEL).expect("job"), None);
    launchd
        .bootstrap(&plist)
        .expect("failure applies to the next bootstrap only");

    assert_eq!(
        launchd.calls(),
        vec![
            format!("bootstrap {}", plist.display()),
            "bootout com.nicecab.rsrsbar.watch.unknown".to_owned(),
            format!("bootout {LABEL}"),
            format!("bootstrap {}", plist.display()),
            format!("bootstrap {}", plist.display()),
        ]
    );
}

#[test]
fn recording_double_mirrors_launchd_refusals() {
    let plist = plist_path(Path::new("/tmp/agents"), LABEL);
    let launchd = Recording::new().with_loaded(LABEL, Some(7));

    assert_eq!(
        launchd.job(LABEL).expect("job"),
        Some(JobInfo { pid: Some(7) })
    );
    assert!(
        launchd.bootstrap(&plist).is_err(),
        "a loaded label refuses bootstrap"
    );
    assert!(
        launchd
            .kickstart("com.nicecab.rsrsbar.watch.unknown")
            .is_err()
    );

    launchd.kickstart(LABEL).expect("kickstart");
    let restarted = launchd.job(LABEL).expect("job").expect("still loaded");
    assert_ne!(restarted.pid, Some(7), "kickstart -k restarts the process");
    assert_eq!(launchd.calls().last(), Some(&format!("kickstart {LABEL}")));
}

fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return Some(value);
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

fn write_selftest_plist(dir: &Path) -> PathBuf {
    let mut dict = plist::Dictionary::new();
    dict.insert("Label".to_owned(), SELFTEST.into());
    dict.insert(
        "ProgramArguments".to_owned(),
        plist::Value::Array(vec!["/bin/sleep".into(), "600".into()]),
    );
    dict.insert("RunAtLoad".to_owned(), true.into());
    let path = plist_path(dir, SELFTEST);
    plist::Value::Dictionary(dict)
        .to_file_xml(&path)
        .expect("write plist");
    path
}

/// Boots the selftest job out when dropped, so a failed assertion midway
/// never leaves `/bin/sleep 600` loaded.
struct BootoutOnDrop<'a>(&'a Launchctl);

impl Drop for BootoutOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.0.bootout(SELFTEST) {
            eprintln!("cleanup: cannot boot out {SELFTEST}: {err}");
        }
    }
}

#[test]
#[ignore = "touches real launchd; run by hand"]
fn real_launchd_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plist = write_selftest_plist(tmp.path());
    let launchd = Launchctl::current();
    launchd
        .bootout(SELFTEST)
        .expect("clear a leftover selftest job");
    let _cleanup = BootoutOnDrop(&launchd);

    launchd.bootstrap(&plist).expect("bootstrap");
    let first = wait_for(|| launchd.job(SELFTEST).expect("job").and_then(|j| j.pid))
        .expect("selftest job got a pid");

    launchd.kickstart(SELFTEST).expect("kickstart");
    let second = wait_for(|| {
        let pid = launchd.job(SELFTEST).expect("job").and_then(|j| j.pid)?;
        (pid != first).then_some(pid)
    });
    assert!(second.is_some(), "kickstart -k replaced pid {first}");

    launchd.bootout(SELFTEST).expect("bootout");
    let gone = wait_for(|| launchd.job(SELFTEST).expect("job").is_none().then_some(()));
    assert!(gone.is_some(), "selftest job unloaded");
    fs::remove_file(&plist).expect("remove plist");
}
