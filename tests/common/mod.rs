//! Shared fixtures for the command tests: a temp home, git checkouts with a
//! bare `origin`, a fake git-rsrs and a `Ctx` over a `Recording`.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use rsrsbar::commands::Ctx;
use rsrsbar::{Launchd, MIN_GIT_RSRS, Paths};

pub const RSRSBAR_EXE: &str = "/Applications/rsrsbar.app/Contents/MacOS/rsrsbar";

pub fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_520_000)
}

/// A process-wide temp directory used as `HOME` for every git the tests
/// spawn, so no user or system git config (identity, `core.hooksPath`,
/// templates) leaks into a test and nothing touches the real home.
fn git_home() -> &'static Path {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| tempfile::tempdir().expect("git home"))
        .path()
}

/// `git` isolated from the real home and from user and system config.
pub fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env("HOME", git_home())
        .env("XDG_CONFIG_HOME", git_home())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=rsrsbar test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"]);
    command
}

/// Runs isolated git in `dir`; panics on failure.
pub fn git(dir: &Path, args: &[&str]) {
    let output = git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// Writes an executable `/bin/sh` script.
pub fn script(path: &Path, body: &str) -> PathBuf {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path.to_path_buf()
}

/// A temp directory holding the home, the checkouts and the fake git-rsrs.
pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub paths: Paths,
    pub git_rsrs: PathBuf,
}

impl Fixture {
    /// A fake git-rsrs reporting [`MIN_GIT_RSRS`].
    pub fn new() -> Fixture {
        let (major, minor, patch) = MIN_GIT_RSRS;
        Fixture::with_git_rsrs(&format!("echo 'git-rsrs {major}.{minor}.{patch}'"))
    }

    /// A fake git-rsrs running `body`.
    pub fn with_git_rsrs(body: &str) -> Fixture {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("mkdir home");
        let git_rsrs = script(&tmp.path().join("bin/git-rsrs"), body);
        Fixture {
            paths: Paths::new(home),
            git_rsrs,
            tmp,
        }
    }

    pub fn root(&self) -> &Path {
        self.tmp.path()
    }

    pub fn ctx<'a>(&self, launchd: &'a dyn Launchd) -> Ctx<'a> {
        Ctx {
            paths: self.paths.clone(),
            launchd,
            git_rsrs: self.git_rsrs.clone(),
            rsrsbar_exe: PathBuf::from(RSRSBAR_EXE),
            now: now(),
        }
    }

    /// A primary checkout `<root>/<name>` on `main` with one commit and,
    /// if `with_origin`, an `origin` remote pointing at a bare repository.
    pub fn repo(&self, name: &str, with_origin: bool) -> PathBuf {
        let repo = self.root().join(name);
        fs::create_dir_all(&repo).expect("mkdir repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        if with_origin {
            let bare = self.root().join(format!("{name}-origin.git"));
            git(
                self.root(),
                &[
                    "init",
                    "-q",
                    "--bare",
                    "-b",
                    "main",
                    bare.to_str().expect("utf-8"),
                ],
            );
            git(
                &repo,
                &["remote", "add", "origin", bare.to_str().expect("utf-8")],
            );
        }
        repo
    }

    /// Every file under the home directory, relative to it, sorted.
    pub fn home_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_files(&self.paths.home, &self.paths.home, &mut files);
        files.sort();
        files
    }
}

fn collect_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(items) = fs::read_dir(dir) else {
        return;
    };
    for item in items {
        let path = item.expect("entry").path();
        if path.is_dir() {
            collect_files(root, &path, files);
        } else {
            files.push(path.strip_prefix(root).expect("under root").to_path_buf());
        }
    }
}

/// The built `rsrsbar` binary with `HOME` set to the fixture's home and the
/// same git isolation as [`git_command`]. Only commands that never reach
/// launchctl (`run-hook`, `allow`) may be run this way.
pub fn rsrsbar(fx: &Fixture) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rsrsbar"));
    command
        .env("HOME", &fx.paths.home)
        .env("XDG_CONFIG_HOME", git_home())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(std::process::Stdio::null());
    command
}

pub fn id(s: &str) -> rsrsbar::RepoId {
    rsrsbar::RepoId::new(s).expect("valid id")
}

/// `add` arguments for `path` with every option at its default.
pub fn add_args(path: &Path) -> rsrsbar::commands::AddArgs {
    rsrsbar::commands::AddArgs {
        path: path.to_path_buf(),
        id: None,
        branch: None,
        remote: None,
        lock_name: None,
        interval: None,
        allow_hooks: false,
    }
}
