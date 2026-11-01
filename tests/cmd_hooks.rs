mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;

use common::{Fixture, now, rsrsbar};
use git_rsrs::outcome::{Outcome as RsrsOutcome, SyncReport, Trigger};
use git_rsrs::status::{Status, next};
use rsrsbar::commands::{AddArgs, add, allow, allow_preview, remove};
use rsrsbar::{Recording, Registry, RepoEntry, RepoId, RepoMemory, Row, State, classify, observe};

const HOOK_CHANGED: &str = "hook changed — needs allow";

fn id(s: &str) -> RepoId {
    RepoId::new(s).expect("valid id")
}

fn write_hook(repo: &Path, phase: &str, body: &str) {
    let path = repo.join(".autosync").join(phase);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write hook");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A `pre-commit` that appends `$GIT_INDEX_FILE $RSRS_PHASE` to `marker`
/// and exits 3 (an intentional rejection).
fn marker_hook(repo: &Path, marker: &Path) {
    let body = format!(
        "echo \"$GIT_INDEX_FILE $RSRS_PHASE\" >> '{}'\nexit 3",
        marker.display()
    );
    write_hook(repo, "pre-commit", &body);
}

fn add_repo(fx: &Fixture, repo: &Path, allow_hooks: bool) {
    let args = AddArgs {
        path: repo.to_path_buf(),
        id: Some("demo".to_owned()),
        branch: None,
        remote: None,
        lock_name: None,
        interval: None,
        allow_hooks,
    };
    add(&fx.ctx(&Recording::new()), args).expect("add");
}

fn run_hook(fx: &Fixture, repo: &Path, id: &str, phase: &str) -> Output {
    rsrsbar(fx)
        .args(["run-hook", id, phase])
        .current_dir(repo)
        .env("GIT_INDEX_FILE", "/tmp/rsrs-index")
        .env("RSRS_PHASE", "pre_commit")
        .output()
        .expect("run rsrsbar")
}

fn allow_yes(fx: &Fixture, id: &str) -> Output {
    let output = rsrsbar(fx)
        .args(["allow", id, "--yes"])
        .output()
        .expect("run rsrsbar");
    assert!(output.status.success(), "{output:?}");
    output
}

fn stderr_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Exit 1, exactly one stderr line, the marker hook not run.
fn assert_fails_closed(output: &Output, marker: &Path, case: &str) {
    assert_eq!(output.status.code(), Some(1), "{case}: {output:?}");
    assert_eq!(stderr_lines(output).len(), 1, "{case}: {output:?}");
    assert!(!marker.exists(), "{case}: the hook ran");
}

fn entry(fx: &Fixture) -> RepoEntry {
    let registry = Registry::load(&fx.paths.registry()).expect("registry");
    registry.get(&id("demo")).expect("registered").clone()
}

#[test]
fn run_hook_fails_closed() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    let marker = fx.root().join("marker");
    marker_hook(&repo, &marker);

    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_fails_closed(&output, &marker, "no registry");

    let registry = fx.paths.registry();
    fs::create_dir_all(registry.parent().expect("parent")).expect("mkdir");
    fs::write(&registry, "version = [[[\nnot toml\n").expect("corrupt");
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_fails_closed(&output, &marker, "corrupt registry");
    fs::remove_file(&registry).expect("remove corrupt registry");

    add_repo(&fx, &repo, false);
    let output = run_hook(&fx, &repo, "other", "pre-commit");
    assert_fails_closed(&output, &marker, "unknown id");

    assert_eq!(entry(&fx).allowed_hooks, None);
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_fails_closed(&output, &marker, "no allow recorded");
    assert_eq!(stderr_lines(&output), vec![HOOK_CHANGED.to_owned()]);

    let output = run_hook(&fx, &repo, "demo", "bogus-phase");
    assert_fails_closed(&output, &marker, "unknown phase");

    // A non-empty allow with a missing or partial snapshot fails closed.
    write_hook(&repo, "post-integrate", "exit 0");
    allow_yes(&fx, "demo");
    let snapshot = fx.paths.allowed_hooks_dir(&id("demo"));
    fs::remove_file(snapshot.join("post-integrate")).expect("partial snapshot");
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_fails_closed(&output, &marker, "partial snapshot");
    fs::remove_dir_all(&snapshot).expect("missing snapshot");
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_fails_closed(&output, &marker, "missing snapshot");
    assert_eq!(stderr_lines(&output), vec![HOOK_CHANGED.to_owned()]);
}

#[test]
fn run_hook_runs_the_allowed_copy() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    let marker = fx.root().join("marker");
    marker_hook(&repo, &marker);
    add_repo(&fx, &repo, false);
    allow_yes(&fx, "demo");

    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let recorded = fs::read_to_string(&marker).expect("marker");
    assert_eq!(recorded, "/tmp/rsrs-index pre_commit\n");

    write_hook(&repo, "pre-commit", "echo changed >> /dev/null\nexit 0");
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stderr_lines(&output), vec![HOOK_CHANGED.to_owned()]);
    assert_eq!(fs::read_to_string(&marker).expect("marker"), recorded);
}

#[test]
fn run_hook_absent_phase_is_ok() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    marker_hook(&repo, &fx.root().join("marker"));
    add_repo(&fx, &repo, true);

    let output = run_hook(&fx, &repo, "demo", "on-result");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn added_hook_upstream_needs_allow() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    add_repo(&fx, &repo, false);
    assert!(
        entry(&fx).allowed_hooks.is_some(),
        "no .autosync: empty set allowed"
    );
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_eq!(output.status.code(), Some(0), "nothing to run: {output:?}");

    let marker = fx.root().join("marker");
    write_hook(
        &repo,
        "post-integrate",
        &format!("touch '{}'", marker.display()),
    );
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_fails_closed(&output, &marker, "hook added upstream");
    assert_eq!(stderr_lines(&output), vec![HOOK_CHANGED.to_owned()]);
}

fn classify_demo(fx: &Fixture) -> (bool, Row) {
    let snapshot = observe(&entry(fx), &fx.paths, &Recording::new()).expect("observe");
    let memory = RepoMemory::default();
    let row = classify(&snapshot.observation(now(), now(), &memory)).row;
    (snapshot.hooks_trusted, row)
}

#[test]
fn untrusted_add_is_red() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    marker_hook(&repo, &fx.root().join("marker"));
    add_repo(&fx, &repo, false);

    let (trusted, row) = classify_demo(&fx);
    assert!(!trusted);
    assert_eq!(row, Row::HooksChanged);
}

fn write_status_at_seq(fx: &Fixture, seq: u64) {
    let report = SyncReport {
        outcome: RsrsOutcome::TimedOut,
        ..SyncReport::new(Trigger::Periodic)
    };
    let status = (0..seq).fold(None::<Status>, |previous, _| {
        Some(next(previous.as_ref(), &report, now(), 42))
    });
    let path = fx.paths.status_file(&id("demo"));
    let json = serde_json::to_string(&status.expect("at least one attempt")).expect("json");
    fs::write(path, json).expect("write status");
}

#[test]
fn allow_moves_recovery_point_and_clears_row3() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    marker_hook(&repo, &fx.root().join("marker"));
    add_repo(&fx, &repo, false);
    write_status_at_seq(&fx, 7);
    let remembered = RepoMemory {
        last_row: Some(Row::HooksChanged),
        ..RepoMemory::default()
    };
    State::update(&fx.paths.state(), |s| {
        s.with_repo(&id("demo"), remembered.clone())
    })
    .expect("state");

    let refused = rsrsbar(&fx).args(["allow", "demo"]).output().expect("run");
    assert_eq!(
        refused.status.code(),
        Some(1),
        "no TTY and no --yes: {refused:?}"
    );
    assert_eq!(entry(&fx).allowed_hooks, None);

    allow_yes(&fx, "demo");
    let state = State::load(&fx.paths.state()).expect("state");
    let memory = state.repos.get(&id("demo")).expect("memory");
    assert_eq!(memory.recovery.as_ref().map(|r| r.seq), Some(7));
    assert_eq!(
        memory.last_row,
        Some(Row::HooksChanged),
        "rest of the memory kept"
    );
    let (trusted, _) = classify_demo(&fx);
    assert!(trusted);
}

#[test]
fn add_allow_hooks_trusts_at_once() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    let marker = fx.root().join("marker");
    marker_hook(&repo, &marker);
    add_repo(&fx, &repo, true);

    let (trusted, _) = classify_demo(&fx);
    assert!(trusted);
    let output = run_hook(&fx, &repo, "demo", "pre-commit");
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(marker.exists());
}

#[test]
fn allow_preview_shows_diff() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    write_hook(&repo, "pre-commit", "exit 0");
    add_repo(&fx, &repo, true);
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);

    let unchanged = allow_preview(&ctx, "demo").expect("preview").text;
    assert_eq!(unchanged, "no changes since the last allow");

    write_hook(&repo, "pre-commit", "exit 1");
    let preview = allow_preview(&ctx, "demo").expect("preview").text;
    assert!(preview.contains("-exit 0"), "{preview}");
    assert!(preview.contains("+exit 1"), "{preview}");
    assert!(allow_preview(&ctx, "unknown").is_err());
}

#[test]
fn allow_preview_shows_mode_and_link_changes() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    write_hook(&repo, "pre-commit", "exit 0");
    let hook = repo.join(".autosync/pre-commit");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o644)).expect("chmod");
    std::os::unix::fs::symlink("pre-commit", repo.join(".autosync/on-result")).expect("link");
    add_repo(&fx, &repo, true);
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);

    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod");
    fs::remove_file(repo.join(".autosync/on-result")).expect("unlink");
    std::os::unix::fs::symlink("post-integrate", repo.join(".autosync/on-result")).expect("link");
    let preview = allow_preview(&ctx, "demo").expect("preview").text;
    assert_ne!(preview, "no changes since the last allow");
    assert!(
        preview.contains("mode 0644 -> 0755: pre-commit"),
        "{preview}"
    );
    assert!(
        preview.contains("symlink on-result: pre-commit -> post-integrate"),
        "{preview}"
    );
}

#[test]
fn allow_is_bound_to_the_previewed_hooks() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    let marker = fx.root().join("marker");
    marker_hook(&repo, &marker);
    add_repo(&fx, &repo, false);
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);

    let preview = allow_preview(&ctx, "demo").expect("preview");
    write_hook(
        &repo,
        "post-integrate",
        &format!("touch '{}'", marker.display()),
    );
    let err = allow(&ctx, "demo", &preview.digest).expect_err("changed while waiting");
    assert_eq!(
        err.to_string(),
        "hooks changed while waiting for confirmation; run rsrsbar allow demo again"
    );
    assert_eq!(entry(&fx).allowed_hooks, None, "nothing recorded");
    let output = run_hook(&fx, &repo, "demo", "post-integrate");
    assert_fails_closed(&output, &marker, "unreviewed hook");

    let preview = allow_preview(&ctx, "demo").expect("preview again");
    allow(&ctx, "demo", &preview.digest).expect("allow what was shown");
    assert_eq!(entry(&fx).allowed_hooks, Some(preview.digest));
}

#[test]
fn failed_add_keeps_a_retained_snapshot() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    write_hook(&repo, "pre-commit", "exit 0");
    add_repo(&fx, &repo, true);
    remove(&fx.ctx(&Recording::new()), "demo").expect("remove");
    let snapshot = fx.paths.allowed_hooks_dir(&id("demo"));
    let retained = fs::read(snapshot.join("pre-commit")).expect("retained snapshot");

    write_hook(&repo, "pre-commit", "exit 1");
    let launchd = Recording::new();
    launchd.fail_next_bootstrap();
    let args = AddArgs {
        path: repo.clone(),
        id: Some("demo".to_owned()),
        branch: None,
        remote: None,
        lock_name: None,
        interval: None,
        allow_hooks: true,
    };
    assert!(add(&fx.ctx(&launchd), args).is_err());
    assert_eq!(
        fs::read(snapshot.join("pre-commit")).expect("snapshot"),
        retained
    );
    let leftovers: Vec<_> = fs::read_dir(fx.paths.repo_dir(&id("demo")))
        .expect("repo dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("allowed-hooks")]);
}

#[test]
fn rejected_confirmation_keeps_the_last_allow() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    write_hook(&repo, "pre-commit", "exit 0");
    add_repo(&fx, &repo, true);
    let allowed_a = entry(&fx).allowed_hooks;
    let snapshot = fx.paths.allowed_hooks_dir(&id("demo"));
    let snapshot_a = fs::read(snapshot.join("pre-commit")).expect("snapshot A");
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);

    write_hook(&repo, "pre-commit", "exit 1");
    let preview_b = allow_preview(&ctx, "demo").expect("preview B");
    write_hook(&repo, "pre-commit", "exit 2");
    assert!(allow(&ctx, "demo", &preview_b.digest).is_err());
    assert_eq!(entry(&fx).allowed_hooks, allowed_a);
    assert_eq!(
        fs::read(snapshot.join("pre-commit")).expect("snapshot"),
        snapshot_a
    );

    let preview_c = allow_preview(&ctx, "demo").expect("preview C").text;
    assert_ne!(preview_c, "no changes since the last allow");
    assert!(preview_c.contains("-exit 0"), "{preview_c}");
    assert!(preview_c.contains("+exit 2"), "{preview_c}");
}

#[test]
fn preview_does_not_trust_an_unverified_snapshot() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    write_hook(&repo, "pre-commit", "exit 0");
    add_repo(&fx, &repo, true);
    write_hook(&repo, "pre-commit", "exit 2");
    // The snapshot on disk no longer matches the recorded allow.
    let snapshot = fx.paths.allowed_hooks_dir(&id("demo"));
    fs::copy(
        repo.join(".autosync/pre-commit"),
        snapshot.join("pre-commit"),
    )
    .expect("tamper");
    let launchd = Recording::new();

    let preview = allow_preview(&fx.ctx(&launchd), "demo")
        .expect("preview")
        .text;
    assert_ne!(preview, "no changes since the last allow");
    assert!(
        preview.contains("does not match the recorded allow"),
        "{preview}"
    );
    assert!(preview.contains("+exit 2"), "{preview}");
}
