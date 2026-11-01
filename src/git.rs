//! What `add` needs to know about a checkout, read through the `git` CLI.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::child::{DEADLINE, output_within};

const GIT: &str = "git";

/// A checkout as `add` sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoProbe {
    /// The work tree's top level, canonicalized.
    pub toplevel: PathBuf,
    /// The checked-out branch; `None` on a detached HEAD.
    pub branch: Option<String>,
    /// True for a `git worktree add` checkout (its git dir is not the
    /// common dir).
    pub is_linked_worktree: bool,
    pub remotes: Vec<String>,
}

/// Errors from probing a checkout.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("cannot run git: {0}")]
    Io(#[from] io::Error),
    #[error("{} is not a git checkout: {}", .path.display(), .stderr.trim())]
    NotACheckout { path: PathBuf, stderr: String },
    #[error("git {} failed in {}: {}", .args, .path.display(), .stderr.trim())]
    Command {
        path: PathBuf,
        args: String,
        stderr: String,
    },
    #[error("unexpected output from git rev-parse in {}", .0.display())]
    Unexpected(PathBuf),
}

/// Runs `git -C <dir> <args>` and returns its stdout.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let output = output_within(Command::new(GIT).arg("-C").arg(dir).args(args), DEADLINE)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(GitError::Command {
        path: dir.to_path_buf(),
        args: args.join(" "),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn lines(stdout: &[u8]) -> impl Iterator<Item = &[u8]> {
    stdout
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
}

/// git prints `--git-dir` / `--git-common-dir` relative to `-C <dir>` when
/// they lie below it, absolute otherwise.
fn resolved(dir: &Path, line: &[u8]) -> io::Result<PathBuf> {
    dir.join(OsStr::from_bytes(line)).canonicalize()
}

/// Probes the checkout containing `path`.
pub fn probe(path: &Path) -> Result<RepoProbe, GitError> {
    let args = [
        "rev-parse",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ];
    let stdout = git(path, &args).map_err(|err| match err {
        GitError::Command { path, stderr, .. } => GitError::NotACheckout { path, stderr },
        other => other,
    })?;
    let [toplevel, git_dir, common_dir] = lines(&stdout).collect::<Vec<_>>()[..] else {
        return Err(GitError::Unexpected(path.to_path_buf()));
    };
    let toplevel = PathBuf::from(OsString::from(OsStr::from_bytes(toplevel))).canonicalize()?;
    let is_linked_worktree = resolved(path, git_dir)? != resolved(path, common_dir)?;
    let remotes = lines(&git(&toplevel, &["remote"])?)
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect();
    Ok(RepoProbe {
        branch: current_branch(&toplevel),
        toplevel,
        is_linked_worktree,
        remotes,
    })
}

/// The branch checked out in `repo`; `None` when HEAD is detached or `repo`
/// is not a checkout.
pub fn current_branch(repo: &Path) -> Option<String> {
    let stdout = git(repo, &["branch", "--show-current"]).ok()?;
    let branch = String::from_utf8(stdout).ok()?;
    let branch = branch.trim();
    (!branch.is_empty()).then(|| branch.to_owned())
}
