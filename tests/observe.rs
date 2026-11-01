use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use git_rsrs::outcome::{Outcome as RsrsOutcome, SyncReport, Trigger};
use git_rsrs::status::next;
use rsrsbar::{
    JobState, Outcome, Paths, Recording, RepoEntry, RepoId, RepoMemory, Row, StatusRead, classify,
    hooks_digest, observe, process_age, watch_label,
};

const HOOK: &[u8] = b"#!/bin/sh\nexit 0\n";

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_520_000)
}

/// A checkout (no git needed) with an executable `.autosync/pre-commit`.
fn repo_with_hook(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    let hook = repo.join(".autosync/pre-commit");
    fs::create_dir_all(hook.parent().expect("parent")).expect("mkdir");
    fs::write(&hook, HOOK).expect("write hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod");
    repo
}

fn entry(repo: &Path, allowed_hooks: Option<String>) -> RepoEntry {
    RepoEntry {
        id: RepoId::new("demo").expect("valid id"),
        path: repo.to_path_buf(),
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        lock_name: None,
        interval: None,
        quiet_period: None,
        git_timeout: None,
        lock_timeout: None,
        stall_threshold: None,
        paused_at: None,
        allowed_hooks,
    }
}

fn write_status(paths: &Paths, id: &RepoId, outcome: RsrsOutcome) {
    let report = SyncReport {
        outcome,
        ..SyncReport::new(Trigger::Periodic)
    };
    let json = serde_json::to_string(&next(None, &report, now(), 42)).expect("serialize");
    let path = paths.status_file(id);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, json).expect("write status");
}

/// `ps` reports whole seconds, so wait until this process is at least one
/// second old before asserting a non-zero uptime.
fn wait_until_a_second_old(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let age = process_age(pid).expect("ps").expect("own process exists");
        if age > Duration::ZERO {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn observe_untrusted_and_not_running() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_with_hook(tmp.path());
    let paths = Paths::new(tmp.path().join("home"));
    let untrusted = entry(&repo, None);

    let snapshot = observe(&untrusted, &paths, &Recording::new()).expect("observe");
    assert!(!snapshot.hooks_trusted);
    assert_eq!(snapshot.job, JobState::NotRunning);
    assert_eq!(snapshot.status, StatusRead::Missing);
    assert_eq!(snapshot.paused_at, None);
    assert_eq!(snapshot.current_branch, None);

    let memory = RepoMemory::default();
    let health = classify(&snapshot.observation(now(), now(), &memory));
    assert_eq!(health.row, Row::HooksChanged);
    assert_eq!(health.label.as_deref(), Some("hook changed — needs allow"));

    let stale = entry(&repo, Some("sha256:stale".to_owned()));
    let snapshot = observe(&stale, &paths, &Recording::new()).expect("observe");
    assert!(!snapshot.hooks_trusted, "a stale digest is untrusted");
    assert_eq!(
        paths.allowed_hooks_dir(&untrusted.id),
        paths.repo_dir(&untrusted.id).join("allowed-hooks")
    );
}

#[test]
fn observe_running_trusted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_with_hook(tmp.path());
    let paths = Paths::new(tmp.path().join("home"));
    let entry = RepoEntry {
        paused_at: Some("2026-11-01T00:00:00Z".to_owned()),
        ..entry(&repo, Some(hooks_digest(&repo).expect("digest")))
    };
    write_status(&paths, &entry.id, RsrsOutcome::Synced);
    let pid = std::process::id();
    wait_until_a_second_old(pid);
    let launchd = Recording::new().with_loaded(&watch_label(&entry.id), Some(pid));

    let snapshot = observe(&entry, &paths, &launchd).expect("observe");
    assert!(snapshot.hooks_trusted);
    assert!(
        matches!(snapshot.job, JobState::Running { up_for } if up_for > Duration::ZERO),
        "{:?}",
        snapshot.job
    );
    assert!(
        matches!(&snapshot.status, StatusRead::Present(s) if s.last_outcome == Outcome::Synced)
    );
    assert_eq!(
        snapshot.paused_at,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_491_200))
    );
    assert_eq!(snapshot.current_branch, None, "read only on branch_skipped");
    assert_eq!(snapshot.settings, entry.settings().expect("settings"));
}

#[test]
fn observe_reads_the_branch_only_when_skipped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).expect("mkdir");
    let status = std::process::Command::new("git")
        .args(["init", "-q", "-b", "feat"])
        .arg(&repo)
        .status()
        .expect("git init");
    assert!(status.success());
    let paths = Paths::new(tmp.path().join("home"));
    let entry = entry(&repo, Some(hooks_digest(&repo).expect("digest")));
    write_status(&paths, &entry.id, RsrsOutcome::BranchSkipped);
    let launchd = Recording::new().with_loaded(&watch_label(&entry.id), None);

    let snapshot = observe(&entry, &paths, &launchd).expect("observe");
    assert_eq!(snapshot.current_branch.as_deref(), Some("feat"));
    assert_eq!(snapshot.job, JobState::NotRunning, "loaded without a PID");
}
