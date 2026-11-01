//! The installed git-rsrs: its version floor and a one-shot `sync` under a
//! deadline of rsrsbar's own.

use std::fs::File;
use std::io::{self, Read, Seek};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::registry::Settings;

/// The first git-rsrs release with `status_file` (0.1.2); older releases
/// reject the generated config.
pub const MIN_GIT_RSRS: (u64, u64, u64) = (0, 1, 2);

const VERSION_PREFIX: &str = "git-rsrs ";
/// Lock wait, then fetch / push / one retry, each bounded by `git_timeout`.
const GIT_STEPS: u32 = 3;
/// `pre_commit` 30 s + `post_integrate` 30 s + `on_result` 10 s.
const HOOK_TIMEOUTS: Duration = Duration::from_secs(70);
const MARGIN: Duration = Duration::from_secs(60);
const KILL_GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);
/// Shell convention for a process ended by a signal.
const SIGNAL_EXIT_BASE: i32 = 128;

/// Errors from running git-rsrs.
#[derive(Debug, thiserror::Error)]
pub enum GitRsrsError {
    #[error("cannot run {}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("cannot read the version of {}: {}", .path.display(), .output.trim())]
    UnreadableVersion { path: PathBuf, output: String },
    #[error(
        "git-rsrs {} is older than {}; run brew upgrade nicecab/tap/git-rsrs",
        dotted(*.found),
        dotted(MIN_GIT_RSRS)
    )]
    TooOld { found: (u64, u64, u64) },
    #[error("sync did not finish within {} s", .0.as_secs())]
    Deadline(Duration),
}

fn dotted((major, minor, patch): (u64, u64, u64)) -> String {
    format!("{major}.{minor}.{patch}")
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> GitRsrsError + '_ {
    move |source| GitRsrsError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Parses `git-rsrs --version` output, `git-rsrs 0.1.1`.
pub fn parse_version(output: &str) -> Option<(u64, u64, u64)> {
    let version = output.trim().strip_prefix(VERSION_PREFIX)?;
    let parts = version
        .split('.')
        .map(|part| part.parse().ok())
        .collect::<Option<Vec<u64>>>()?;
    match parts[..] {
        [major, minor, patch] => Some((major, minor, patch)),
        _ => None,
    }
}

/// Refuses a git-rsrs older than [`MIN_GIT_RSRS`].
pub fn check_version(git_rsrs: &Path) -> Result<(), GitRsrsError> {
    let output = Command::new(git_rsrs)
        .arg("--version")
        .output()
        .map_err(io_error(git_rsrs))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let found = parse_version(&text)
        .filter(|_| output.status.success())
        .ok_or_else(|| GitRsrsError::UnreadableVersion {
            path: git_rsrs.to_path_buf(),
            output: text.clone().into_owned(),
        })?;
    if found < MIN_GIT_RSRS {
        return Err(GitRsrsError::TooOld { found });
    }
    Ok(())
}

/// How long a one-shot `sync` may take: `lock_timeout + 3 × git_timeout +
/// 70 s + 60 s` (520 s with the defaults), because git-rsrs's status-file
/// lock waits without a deadline of its own. Saturates rather than panics.
pub fn sync_deadline(settings: &Settings) -> Duration {
    settings
        .lock_timeout
        .saturating_add(settings.git_timeout.saturating_mul(GIT_STEPS))
        .saturating_add(HOOK_TIMEOUTS)
        .saturating_add(MARGIN)
}

/// A finished one-shot sync: git-rsrs's exit code (128 + the signal number
/// if a signal ended it) and its interleaved stdout and stderr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRun {
    pub exit_code: i32,
    pub output: String,
}

/// Runs `<git_rsrs> --repo <repo> --config <config> sync` in its own process
/// group. Past `deadline` the whole group gets SIGTERM, then SIGKILL after
/// 2 s, and the result is [`GitRsrsError::Deadline`].
pub fn sync_once(
    git_rsrs: &Path,
    repo: &Path,
    config: &Path,
    deadline: Duration,
) -> Result<SyncRun, GitRsrsError> {
    // A file, not a pipe: nothing to drain while waiting, and a grandchild
    // that keeps the descriptor open cannot block the read.
    let mut log = tempfile::tempfile().map_err(io_error(git_rsrs))?;
    let stderr = log.try_clone().map_err(io_error(git_rsrs))?;
    let stdout = log.try_clone().map_err(io_error(git_rsrs))?;
    let mut child = Command::new(git_rsrs)
        .arg("--repo")
        .arg(repo)
        .arg("--config")
        .arg(config)
        .arg("sync")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .map_err(io_error(git_rsrs))?;
    let Some(status) = wait_until(&mut child, deadline).map_err(io_error(git_rsrs))? else {
        terminate_group(&mut child).map_err(io_error(git_rsrs))?;
        return Err(GitRsrsError::Deadline(deadline));
    };
    Ok(SyncRun {
        exit_code: exit_code(status),
        output: read_all(&mut log).map_err(io_error(git_rsrs))?,
    })
}

/// Polls `child` until it exits or `deadline` passes (`Ok(None)`).
fn wait_until(child: &mut Child, deadline: Duration) -> io::Result<Option<ExitStatus>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started.elapsed() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

/// SIGTERM to the group, SIGKILL after the grace, then reap the leader.
/// The leader is reaped last so its PID — the group id — cannot be reused
/// while signals are still being sent to it.
fn terminate_group(child: &mut Child) -> io::Result<()> {
    let group = libc::pid_t::try_from(child.id()).map_err(io::Error::other)?;
    signal_group(group, libc::SIGTERM)?;
    thread::sleep(KILL_GRACE);
    signal_group(group, libc::SIGKILL)?;
    child.wait().map(drop)
}

fn signal_group(group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: killpg only sends a signal; `group` is the process group this
    // module created with `process_group(0)` and whose leader is not reaped.
    if unsafe { libc::killpg(group, signal) } == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // ESRCH: no member is left. EPERM: macOS answers it when the only
    // members left are zombies — here the unreaped leader, which this
    // process owns, so it cannot mean a real permission problem.
    match err.raw_os_error() {
        Some(libc::ESRCH | libc::EPERM) => Ok(()),
        _ => Err(err),
    }
}

/// git-rsrs's exit code; a process ended by a signal reports 128 + the
/// signal number, as a shell does (e.g. 143 for SIGTERM).
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| SIGNAL_EXIT_BASE + signal))
        .unwrap_or(SIGNAL_EXIT_BASE)
}

fn read_all(log: &mut File) -> io::Result<String> {
    log.rewind()?;
    let mut bytes = Vec::new();
    log.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
