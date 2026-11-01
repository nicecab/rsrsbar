mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use common::{Fixture, add_args, id};
use git_rsrs::config::Config;
use rsrsbar::commands::{AddArgs, add, remove};
use rsrsbar::{Launchd, Recording, Registry, RepoMemory, State, hooks_digest, watch_label};

fn registry(fx: &Fixture) -> Registry {
    Registry::load(&fx.paths.registry()).expect("registry")
}

#[test]
fn add_registers_and_bootstraps() {
    let fx = Fixture::new();
    let repo = fx.repo("demo-repo", true);
    let launchd = Recording::new();

    let report = add(&fx.ctx(&launchd), add_args(&repo)).expect("add");
    assert_eq!(report.exit_code, 0);
    let demo = id("demo-repo");
    let label = watch_label(&demo);
    assert!(report.text.contains("demo-repo"), "{}", report.text);
    assert!(report.text.contains(&label), "{}", report.text);
    assert!(report.text.contains("main"), "{}", report.text);
    assert!(report.text.contains("origin"), "{}", report.text);

    let registry = registry(&fx);
    let entry = registry.get(&demo).expect("registered");
    assert_eq!(entry.path, repo.canonicalize().expect("canonical"));
    assert_eq!(entry.branch, "main");
    assert_eq!(entry.remote, "origin");
    let empty = hooks_digest(&fx.root().join("nowhere")).expect("empty digest");
    assert_eq!(entry.allowed_hooks.as_deref(), Some(empty.as_str()));

    let plist = fx.paths.launch_agent(&demo);
    assert!(plist.is_file());
    let config = fs::read_to_string(fx.paths.git_rsrs_config(&demo)).expect("config");
    Config::parse(&config).expect("git-rsrs accepts the config");
    assert!(config.contains(common::RSRSBAR_EXE), "{config}");
    assert_eq!(
        launchd.calls(),
        vec![format!("bootstrap {}", plist.display())]
    );
    assert!(launchd.job(&label).expect("job").is_some());
}

/// Asserts `add` failed with `copy` and left nothing behind.
fn assert_refused(fx: &Fixture, launchd: &Recording, add_args: AddArgs, copy: &str) {
    let err = add(&fx.ctx(launchd), add_args).expect_err(copy);
    assert!(err.to_string().contains(copy), "expected {copy:?} in {err}");
    assert!(fx.home_files().is_empty(), "{:?}", fx.home_files());
    assert!(launchd.calls().is_empty(), "{:?}", launchd.calls());
}

#[test]
fn add_refusals() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let repo = fx.repo("repo", true);
    common::git(&repo, &["worktree", "add", "-q", "../wt", "-b", "feat"]);
    let wt = fx.root().join("wt").canonicalize().expect("canonical");
    assert_refused(
        &fx,
        &launchd,
        add_args(&wt),
        &format!(
            "{} is a linked worktree; register the primary checkout",
            wt.display()
        ),
    );

    let toplevel = repo.canonicalize().expect("canonical");
    let develop = AddArgs {
        branch: Some("develop".to_owned()),
        ..add_args(&repo)
    };
    let copy = format!(
        "{} is on main, not develop; check out develop first",
        toplevel.display()
    );
    assert_refused(&fx, &launchd, develop, &copy);

    let lonely = fx.repo("lonely", false);
    let copy = "has no remote origin; add it with git remote add origin <url>";
    assert_refused(&fx, &launchd, add_args(&lonely), copy);

    common::git(&repo, &["checkout", "-q", "--detach"]);
    assert_refused(
        &fx,
        &launchd,
        add_args(&repo),
        "has a detached HEAD; check out a branch first",
    );
    common::git(&repo, &["checkout", "-q", "main"]);

    assert_refused(
        &fx,
        &launchd,
        add_args(&fx.root().join("plain")),
        "not a git checkout",
    );

    let old = Fixture::with_git_rsrs("echo 'git-rsrs 0.1.1'");
    let repo = old.repo("repo", true);
    let copy = "git-rsrs 0.1.1 is older than";
    assert_refused(&old, &launchd, add_args(&repo), copy);
    let err = add(&old.ctx(&launchd), add_args(&repo)).expect_err("old git-rsrs");
    assert!(
        err.to_string()
            .contains("run brew upgrade nicecab/tap/git-rsrs"),
        "{err}"
    );
}

#[test]
fn add_duplicate_spellings() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let repo = fx.repo("repo", true);
    add(&fx.ctx(&launchd), add_args(&repo)).expect("first add");
    let sub = repo.join("sub");
    fs::create_dir_all(&sub).expect("mkdir sub");
    let link = fx.root().join("link");
    symlink(&repo, &link).expect("symlink");
    let slash = PathBuf::from(format!("{}/", repo.display()));

    for spelling in [&sub, &slash, &link] {
        let same_id = add(&fx.ctx(&launchd), add_args(spelling)).expect_err("duplicate");
        assert!(
            same_id.to_string().contains("already registered"),
            "{same_id}"
        );
        let other_id = AddArgs {
            id: Some("other".to_owned()),
            ..add_args(spelling)
        };
        let err = add(&fx.ctx(&launchd), other_id).expect_err("duplicate path");
        assert!(
            err.to_string().contains("is already registered as repo"),
            "{err}"
        );
    }
    assert_eq!(registry(&fx).repos.len(), 1);
    assert_eq!(launchd.calls().len(), 1, "{:?}", launchd.calls());
    assert!(!fx.paths.launch_agent(&id("other")).exists());
}

#[test]
fn add_bootstrap_failure_rolls_back() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    let launchd = Recording::new();
    launchd.fail_next_bootstrap();

    assert!(add(&fx.ctx(&launchd), add_args(&repo)).is_err());
    assert!(registry(&fx).repos.is_empty());
    assert!(fx.home_files().is_empty(), "{:?}", fx.home_files());
    assert!(!fx.paths.repo_dir(&id("repo")).exists());

    let leftover = Recording::new().with_loaded(&watch_label(&id("repo")), Some(4321));
    assert!(
        add(&fx.ctx(&leftover), add_args(&repo)).is_err(),
        "label left loaded"
    );
    assert!(registry(&fx).repos.is_empty());
    assert!(fx.home_files().is_empty(), "{:?}", fx.home_files());
}

#[test]
fn remove_tolerates_unloaded_job() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);
    add(&ctx, add_args(&repo)).expect("add");
    let repo_id = id("repo");
    let label = watch_label(&repo_id);
    let status = fx.paths.status_file(&repo_id);
    fs::write(&status, "{}").expect("status");
    State::update(&fx.paths.state(), |s| {
        s.with_repo(&repo_id, RepoMemory::default())
            .with_repo(&id("keep"), RepoMemory::default())
    })
    .expect("state");
    launchd.bootout(&label).expect("booted out by hand");

    let report = remove(&ctx, "repo").expect("remove");
    assert_eq!(report.exit_code, 0);
    assert!(registry(&fx).repos.is_empty());
    assert!(!fx.paths.launch_agent(&repo_id).exists());
    assert!(!fx.paths.git_rsrs_config(&repo_id).exists());
    assert!(status.exists(), "status history is kept");
    assert!(
        report
            .text
            .contains(&fx.paths.repo_dir(&repo_id).display().to_string())
    );
    let state = State::load(&fx.paths.state()).expect("state");
    assert!(!state.repos.contains_key(&repo_id));
    assert!(state.repos.contains_key(&id("keep")));
    assert_eq!(launchd.calls().last(), Some(&format!("bootout {label}")));

    assert!(remove(&ctx, "repo").is_err(), "unknown id");
}

#[test]
fn add_refuses_a_loaded_unregistered_label() {
    let fx = Fixture::new();
    let repo = fx.repo("repo", true);
    let repo_id = id("repo");
    let label = watch_label(&repo_id);
    let plist = fx.paths.launch_agent(&repo_id);
    let config = fx.paths.git_rsrs_config(&repo_id);
    for (path, text) in [(&plist, "orphan plist"), (&config, "orphan config")] {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("orphan file");
    }
    let launchd = Recording::new().with_loaded(&label, Some(4321));

    let err = add(&fx.ctx(&launchd), add_args(&repo)).expect_err("orphan job");
    let copy = format!(
        "{label} is already loaded but not registered; run launchctl bootout gui/$(id -u)/{label} first"
    );
    assert_eq!(err.to_string(), copy);
    assert!(launchd.calls().is_empty(), "{:?}", launchd.calls());
    assert_eq!(fs::read_to_string(&plist).expect("plist"), "orphan plist");
    assert_eq!(
        fs::read_to_string(&config).expect("config"),
        "orphan config"
    );
    assert!(registry(&fx).repos.is_empty());
}
