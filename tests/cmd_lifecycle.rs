mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::Fixture;
use rsrsbar::commands::{AddArgs, CommandError, Ctx, add, pause, refresh, restart, resume, sync};
use rsrsbar::{
    GitRsrsError, Launchd, MIN_GIT_RSRS, Recording, Registry, RepoEntry, RepoId, watch_label,
};

fn id(s: &str) -> RepoId {
    RepoId::new(s).expect("valid id")
}

fn add_repo(fx: &Fixture, ctx: &Ctx, name: &str) -> RepoId {
    let repo = fx.repo(name, true);
    let args = AddArgs {
        path: repo,
        id: None,
        branch: None,
        remote: None,
        lock_name: None,
        interval: None,
        allow_hooks: false,
    };
    add(ctx, args).expect("add");
    id(name)
}

fn entry(fx: &Fixture, repo_id: &RepoId) -> RepoEntry {
    let registry = Registry::load(&fx.paths.registry()).expect("registry");
    registry.get(repo_id).expect("registered").clone()
}

fn paused_at(fx: &Fixture, repo_id: &RepoId) -> Option<std::time::SystemTime> {
    entry(fx, repo_id)
        .paused_at
        .map(|t| humantime::parse_rfc3339(&t).expect("RFC 3339"))
}

fn config(fx: &Fixture, repo_id: &RepoId) -> String {
    fs::read_to_string(fx.paths.git_rsrs_config(repo_id)).expect("config")
}

#[test]
fn pause_then_resume() {
    let fx = Fixture::new();
    let demo = add_repo(&fx, &fx.ctx(&Recording::new()), "demo");
    let label = watch_label(&demo);
    let launchd = Recording::new().with_loaded(&label, Some(4321));
    let ctx = fx.ctx(&launchd);

    let report = pause(&ctx, "demo").expect("pause");
    assert_eq!(report.exit_code, 0);
    assert_eq!(launchd.calls(), vec![format!("bootout {label}")]);
    assert_eq!(paused_at(&fx, &demo), Some(common::now()));

    let again = pause(&ctx, "demo").expect("pause again");
    assert_eq!(again.exit_code, 0);
    assert!(
        again.text.contains("already paused since"),
        "{}",
        again.text
    );
    assert_eq!(launchd.calls().len(), 1, "no second bootout");

    resume(&ctx, "demo").expect("resume");
    let plist = fx.paths.launch_agent(&demo);
    assert_eq!(
        launchd.calls().last(),
        Some(&format!("bootstrap {}", plist.display()))
    );
    assert_eq!(paused_at(&fx, &demo), None);

    let note = resume(&ctx, "demo").expect("resume when not paused");
    assert_eq!(note.exit_code, 0);
    assert!(note.text.contains("is not paused"), "{}", note.text);
    assert!(pause(&ctx, "unknown").is_err());
}

#[test]
fn pause_when_not_loaded() {
    let fx = Fixture::new();
    let demo = add_repo(&fx, &fx.ctx(&Recording::new()), "demo");
    let launchd = Recording::new();

    pause(&fx.ctx(&launchd), "demo").expect("pause tolerates an unloaded job");
    assert_eq!(paused_at(&fx, &demo), Some(common::now()));
    let err = restart(&fx.ctx(&launchd), "demo").expect_err("paused");
    assert!(
        err.to_string().contains("demo is paused; resume it first"),
        "{err}"
    );
}

#[test]
fn refresh_rewrites_changed_only() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);
    let alpha = add_repo(&fx, &ctx, "alpha");
    let beta = add_repo(&fx, &ctx, "beta");
    let beta_before = config(&fx, &beta);
    Registry::update(&fx.paths.registry(), |r| {
        r.with_entry(&alpha, |e| RepoEntry {
            interval: Some("5m".to_owned()),
            ..e.clone()
        })
    })
    .expect("edit interval");
    let calls_before = launchd.calls().len();

    let report = refresh(&ctx, None).expect("refresh");
    assert_eq!(
        launchd.calls()[calls_before..],
        [format!("kickstart {}", watch_label(&alpha))]
    );
    assert!(
        config(&fx, &alpha).contains("interval = \"5m\""),
        "{}",
        config(&fx, &alpha)
    );
    assert_eq!(config(&fx, &beta), beta_before);
    assert!(report.text.contains("alpha"), "{}", report.text);
    assert!(report.text.contains("beta"), "{}", report.text);

    let calls_before = launchd.calls().len();
    refresh(&ctx, Some("beta")).expect("refresh one");
    assert_eq!(launchd.calls().len(), calls_before, "nothing changed");
    assert!(refresh(&ctx, Some("unknown")).is_err());
}

#[test]
fn refresh_after_exe_move() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let alpha = add_repo(&fx, &fx.ctx(&launchd), "alpha");
    let beta = add_repo(&fx, &fx.ctx(&launchd), "beta");
    let gamma = add_repo(&fx, &fx.ctx(&launchd), "gamma");
    pause(&fx.ctx(&launchd), "gamma").expect("pause gamma");
    launchd
        .bootout(&watch_label(&beta))
        .expect("beta crashed out");
    let moved =
        PathBuf::from("/opt/homebrew/Caskroom/rsrsbar/1.0/rsrsbar.app/Contents/MacOS/rsrsbar");
    let ctx = Ctx {
        rsrsbar_exe: moved.clone(),
        ..fx.ctx(&launchd)
    };
    let calls_before = launchd.calls().len();

    refresh(&ctx, None).expect("refresh");
    for repo_id in [&alpha, &beta] {
        let text = config(&fx, repo_id);
        assert!(text.contains(moved.to_str().expect("utf-8")), "{text}");
        assert!(!text.contains(common::RSRSBAR_EXE), "{text}");
    }
    assert!(
        config(&fx, &gamma).contains(common::RSRSBAR_EXE),
        "paused repos are skipped"
    );
    let plist = fx.paths.launch_agent(&beta);
    assert_eq!(
        launchd.calls()[calls_before..],
        [
            format!("kickstart {}", watch_label(&alpha)),
            format!("bootstrap {}", plist.display()),
        ]
    );
}

fn fake_git_rsrs(sync_body: &str) -> Fixture {
    let (major, minor, patch) = MIN_GIT_RSRS;
    Fixture::with_git_rsrs(&format!(
        "if [ \"$1\" = --version ]; then echo 'git-rsrs {major}.{minor}.{patch}'; exit 0; fi\n{sync_body}"
    ))
}

#[test]
fn sync_runs_and_passes_exit_code() {
    let fx = fake_git_rsrs("echo \"syncing $2\"\necho 'push rejected' >&2\nexit 6");
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);
    let demo = add_repo(&fx, &ctx, "demo");

    let report = sync(&ctx, "demo").expect("sync");
    assert_eq!(report.exit_code, 6);
    let repo: &Path = &entry(&fx, &demo).path;
    assert!(
        report.text.contains(&format!("syncing {}", repo.display())),
        "{}",
        report.text
    );
    assert!(report.text.contains("push rejected"), "{}", report.text);

    pause(&ctx, "demo").expect("pause");
    let err = sync(&ctx, "demo").expect_err("paused");
    assert_eq!(err.to_string(), "demo is paused; resume it first");
    assert_eq!(err.exit_code(), 1);

    let deadline = CommandError::GitRsrs(GitRsrsError::Deadline(Duration::from_secs(520)));
    assert_eq!(deadline.exit_code(), 124);
    assert_eq!(deadline.to_string(), "sync did not finish within 520 s");
}

#[test]
fn refresh_reports_every_repository() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let alpha = add_repo(&fx, &fx.ctx(&launchd), "alpha");
    let beta = add_repo(&fx, &fx.ctx(&launchd), "beta");
    // alpha's repository directory becomes a file: its config cannot be written.
    let alpha_dir = fx.paths.repo_dir(&alpha);
    fs::remove_dir_all(&alpha_dir).expect("remove alpha dir");
    fs::write(&alpha_dir, "not a directory").expect("block alpha");
    let moved = PathBuf::from("/usr/local/bin/rsrsbar");
    let ctx = Ctx {
        rsrsbar_exe: moved.clone(),
        ..fx.ctx(&launchd)
    };

    let report = refresh(&ctx, None).expect("refresh reports per repository");
    assert_ne!(report.exit_code, 0, "{}", report.text);
    let lines: Vec<_> = report.text.lines().collect();
    assert_eq!(lines.len(), 2, "{}", report.text);
    assert!(lines[0].starts_with("alpha: failed"), "{}", report.text);
    assert_eq!(lines[1], "beta: restarted");
    assert!(config(&fx, &beta).contains("/usr/local/bin/rsrsbar"));
}

#[test]
fn refresh_retries_a_failed_launchd_action() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let alpha = add_repo(&fx, &fx.ctx(&launchd), "alpha");
    let label = watch_label(&alpha);
    let plist = fx.paths.launch_agent(&alpha);
    let old_plist = fs::read(&plist).expect("plist");
    let config_before = config(&fx, &alpha);
    let moved_git_rsrs = fx.root().join("cellar/git-rsrs");
    let ctx = Ctx {
        git_rsrs: moved_git_rsrs.clone(),
        ..fx.ctx(&launchd)
    };
    launchd.fail_next_bootstrap();
    let calls_before = launchd.calls().len();

    let failed = refresh(&ctx, None).expect("refresh");
    assert_ne!(failed.exit_code, 0);
    assert!(failed.text.starts_with("alpha: failed"), "{}", failed.text);
    assert_eq!(
        launchd.calls()[calls_before..],
        [
            format!("bootout {label}"),
            format!("bootstrap {}", plist.display())
        ],
        "a changed plist is reloaded, not kickstarted"
    );
    assert_eq!(
        fs::read(&plist).expect("plist"),
        old_plist,
        "previous plist restored"
    );
    assert_eq!(config(&fx, &alpha), config_before);

    let calls_before = launchd.calls().len();
    let retried = refresh(&ctx, None).expect("refresh again");
    assert_eq!(retried.exit_code, 0, "{}", retried.text);
    assert_eq!(
        launchd.calls()[calls_before..],
        [format!("bootstrap {}", plist.display())]
    );
    let new_plist = fs::read_to_string(&plist).expect("plist");
    assert!(
        new_plist.contains(moved_git_rsrs.to_str().expect("utf-8")),
        "{new_plist}"
    );
}

#[test]
fn resume_failure_stays_paused() {
    let fx = Fixture::new();
    let demo = add_repo(&fx, &fx.ctx(&Recording::new()), "demo");
    let label = watch_label(&demo);
    pause(&fx.ctx(&Recording::new()), "demo").expect("pause");
    let launchd = Recording::new().with_loaded(&label, Some(99));

    let err = resume(&fx.ctx(&launchd), "demo").expect_err("loaded label refuses bootstrap");
    assert!(
        err.to_string()
            .contains(&format!("launchctl bootout gui/$(id -u)/{label}")),
        "{err}"
    );
    assert_eq!(paused_at(&fx, &demo), Some(common::now()), "still paused");
    let plist = fx.paths.launch_agent(&demo);
    assert_eq!(
        launchd.calls(),
        vec![format!("bootstrap {}", plist.display())]
    );
}

#[test]
fn refresh_retries_after_a_partial_install() {
    let fx = Fixture::new();
    let launchd = Recording::new();
    let ctx = fx.ctx(&launchd);
    let alpha = add_repo(&fx, &ctx, "alpha");
    let label = watch_label(&alpha);
    let plist = fx.paths.launch_agent(&alpha);
    let plist_bytes = fs::read(&plist).expect("plist");
    let config_before = config(&fx, &alpha);
    Registry::update(&fx.paths.registry(), |r| {
        r.with_entry(&alpha, |e| RepoEntry {
            interval: Some("5m".to_owned()),
            ..e.clone()
        })
    })
    .expect("edit interval");
    // A directory where the plist belongs: the config step succeeds, the plist step fails.
    fs::remove_file(&plist).expect("remove plist");
    fs::create_dir(&plist).expect("block plist");
    let calls_before = launchd.calls().len();

    let failed = refresh(&ctx, None).expect("refresh");
    assert_ne!(failed.exit_code, 0);
    assert!(failed.text.starts_with("alpha: failed"), "{}", failed.text);
    assert_eq!(config(&fx, &alpha), config_before, "config rolled back");
    assert_eq!(launchd.calls().len(), calls_before, "no launchd action");

    fs::remove_dir(&plist).expect("unblock");
    fs::write(&plist, &plist_bytes).expect("repair plist");
    let retried = refresh(&ctx, None).expect("refresh again");
    assert_eq!(retried.text, "alpha: restarted");
    assert_eq!(
        launchd.calls()[calls_before..],
        [format!("kickstart {label}")]
    );
    assert!(config(&fx, &alpha).contains("interval = \"5m\""));
}
