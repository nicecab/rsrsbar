use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use rsrsbar::{
    GitRsrsError, MIN_GIT_RSRS, RepoEntry, RepoId, check_version, parse_version, sync_deadline,
    sync_once,
};

/// Writes an executable `/bin/sh` script named `name` into `dir`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn version_text((major, minor, patch): (u64, u64, u64)) -> String {
    format!("{major}.{minor}.{patch}")
}

#[test]
fn version_parse_and_floor() {
    assert_eq!(parse_version("git-rsrs 0.1.1\n"), Some((0, 1, 1)));
    assert_eq!(parse_version("git-rsrs 12.3.45"), Some((12, 3, 45)));
    assert_eq!(parse_version("git-rsrs 0.1"), None);
    assert_eq!(parse_version("something 0.1.1"), None);
    assert_eq!(parse_version(""), None);
    assert!(MIN_GIT_RSRS > (0, 1, 1));

    let tmp = tempfile::tempdir().expect("tempdir");
    let old = script(tmp.path(), "old", "echo 'git-rsrs 0.1.1'");
    let message = check_version(&old)
        .expect_err("0.1.1 is too old")
        .to_string();
    assert!(
        message.contains("brew upgrade nicecab/tap/git-rsrs"),
        "{message}"
    );
    assert!(message.contains("0.1.1"), "{message}");
    assert!(message.contains(&version_text(MIN_GIT_RSRS)), "{message}");

    let current = format!("echo 'git-rsrs {}'", version_text(MIN_GIT_RSRS));
    check_version(&script(tmp.path(), "current", &current)).expect("floor passes");

    assert!(check_version(&script(tmp.path(), "garbage", "echo nonsense")).is_err());
    assert!(check_version(&tmp.path().join("missing")).is_err());
}

#[test]
fn deadline_formula() {
    let entry = RepoEntry {
        id: RepoId::new("demo").expect("valid id"),
        path: PathBuf::from("/x/demo"),
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        lock_name: None,
        interval: None,
        quiet_period: None,
        git_timeout: None,
        lock_timeout: None,
        stall_threshold: None,
        paused_at: None,
        allowed_hooks: None,
    };
    let defaults = entry.settings().expect("defaults");
    assert_eq!(sync_deadline(&defaults), Duration::from_secs(520));

    let slow = RepoEntry {
        git_timeout: Some("5m".to_owned()),
        lock_timeout: Some("1m".to_owned()),
        ..entry
    };
    let slow = slow.settings().expect("overrides");
    assert_eq!(sync_deadline(&slow), Duration::from_secs(60 + 900 + 130));
}

fn process_alive(pid: &str) -> bool {
    Command::new("/bin/kill")
        .args(["-0", pid])
        .output()
        .expect("run kill")
        .status
        .success()
}

#[test]
fn sync_once_passes_exit_code_and_kills_on_deadline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let args_file = tmp.path().join("args");
    let failing = script(
        tmp.path(),
        "failing",
        &format!(
            "echo \"$@\" > '{}'\necho out; echo err >&2\nexit 7",
            args_file.display()
        ),
    );
    let repo = Path::new("/x/repo");
    let config = Path::new("/x/config.toml");
    let run = sync_once(&failing, repo, config, Duration::from_secs(10)).expect("run");
    assert_eq!(run.exit_code, 7);
    assert!(
        run.output.contains("out") && run.output.contains("err"),
        "{}",
        run.output
    );
    let args = fs::read_to_string(&args_file).expect("args");
    assert_eq!(args.trim(), "--repo /x/repo --config /x/config.toml sync");

    let pid_file = tmp.path().join("sleep.pid");
    let hanging = script(
        tmp.path(),
        "hanging",
        &format!("sleep 60 &\necho $! > '{}'\nwait", pid_file.display()),
    );
    let started = Instant::now();
    let result = sync_once(&hanging, repo, config, Duration::from_secs(1));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(result, Err(GitRsrsError::Deadline(d)) if d == Duration::from_secs(1)),
        "{result:?}"
    );
    let message = result.expect_err("deadline").to_string();
    assert_eq!(message, "sync did not finish within 1 s");

    let pid = fs::read_to_string(&pid_file).expect("pid");
    let pid = pid.trim();
    let gone = Instant::now() + Duration::from_secs(2);
    while process_alive(pid) && Instant::now() < gone {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_alive(pid),
        "child sleep {pid} survived the deadline"
    );
}
