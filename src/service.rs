//! Per-repository git-rsrs config and LaunchAgent plist, built from
//! `Serialize` values (never string formatting).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::paths::{APP_LABEL, HEARTBEAT_LABEL, Paths, watch_label};
use crate::registry::{RepoEntry, Settings};
use crate::trust::Phase;

/// The Homebrew symlink to git-rsrs, never resolved into the Cellar.
pub const GIT_RSRS: &str = "/opt/homebrew/bin/git-rsrs";

const CONFIG_VERSION: u32 = 1;
const BASE_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
const THROTTLE_INTERVAL_SECS: i64 = 30;
const PRE_COMMIT_TIMEOUT: Duration = Duration::from_secs(30);
const POST_INTEGRATE_TIMEOUT: Duration = Duration::from_secs(30);
const ON_RESULT_TIMEOUT: Duration = Duration::from_secs(10);
const PRE_COMMIT_REJECT: i32 = 3;
/// How often launchd runs `check-heartbeat`.
const HEARTBEAT_INTERVAL_SECS: i64 = 900;
/// The executable inside the app bundle.
const BUNDLE_EXE: &str = "Contents/MacOS/rsrsbar";

/// Errors from generating service files.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// launchd and git-rsrs take UTF-8 strings; a lossy conversion would
    /// point them at a different file, so non-UTF-8 paths are refused.
    #[error("path is not valid UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),
    #[error("cannot serialize the git-rsrs config: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("cannot serialize the LaunchAgent plist: {0}")]
    Plist(#[from] plist::Error),
}

#[derive(Serialize)]
struct GitRsrsConfig {
    version: u32,
    branch: String,
    remote: String,
    quiet_period: String,
    interval: String,
    git_timeout: String,
    lock_timeout: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_name: Option<String>,
    status_file: String,
    hooks: Hooks,
}

#[derive(Serialize)]
struct Hooks {
    pre_commit: [Hook; 1],
    post_integrate: [Hook; 1],
    on_result: [Hook; 1],
}

#[derive(Serialize)]
struct Hook {
    command: [String; 4],
    timeout: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reject_exit_codes: Vec<i32>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct LaunchAgent {
    label: String,
    program_arguments: [String; 6],
    working_directory: String,
    environment_variables: Environment,
    run_at_load: bool,
    keep_alive: bool,
    throttle_interval: i64,
    standard_out_path: String,
    standard_error_path: String,
}

/// The menu bar app: always kept alive, an interactive process.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct AppAgent {
    label: &'static str,
    program_arguments: [String; 2],
    environment_variables: Environment,
    run_at_load: bool,
    keep_alive: bool,
    process_type: &'static str,
    standard_out_path: String,
    standard_error_path: String,
}

/// `check-heartbeat`: every 15 min, not at load.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct HeartbeatAgent {
    label: &'static str,
    program_arguments: [String; 2],
    environment_variables: Environment,
    run_at_load: bool,
    start_interval: i64,
    standard_out_path: String,
    standard_error_path: String,
}

#[derive(Serialize)]
struct Environment {
    #[serde(rename = "PATH")]
    path: String,
}

/// The git-rsrs TOML for one repository: every hook phase is wired to
/// `rsrsbar_exe run-hook <id> <phase>`.
pub fn git_rsrs_config(
    entry: &RepoEntry,
    settings: &Settings,
    paths: &Paths,
    rsrsbar_exe: &Path,
) -> Result<String, ServiceError> {
    let exe = utf8(rsrsbar_exe)?;
    let hook = |phase: Phase| {
        let (timeout, reject_exit_codes) = hook_policy(phase);
        Hook {
            command: [
                exe.to_owned(),
                "run-hook".to_owned(),
                entry.id.to_string(),
                phase.file_name().to_owned(),
            ],
            timeout: duration(timeout),
            reject_exit_codes,
        }
    };
    let config = GitRsrsConfig {
        version: CONFIG_VERSION,
        branch: entry.branch.clone(),
        remote: entry.remote.clone(),
        quiet_period: duration(settings.quiet_period),
        interval: duration(settings.interval),
        git_timeout: duration(settings.git_timeout),
        lock_timeout: duration(settings.lock_timeout),
        lock_name: settings.lock_name.clone(),
        status_file: utf8(&paths.status_file(&entry.id))?.to_owned(),
        hooks: Hooks {
            pre_commit: [hook(Phase::PreCommit)],
            post_integrate: [hook(Phase::PostIntegrate)],
            on_result: [hook(Phase::OnResult)],
        },
    };
    Ok(toml::to_string(&config)?)
}

/// The XML LaunchAgent plist running `git_rsrs watch` for one repository.
pub fn launch_agent_plist(
    entry: &RepoEntry,
    paths: &Paths,
    git_rsrs: &Path,
) -> Result<Vec<u8>, ServiceError> {
    let repo = utf8(&entry.path)?.to_owned();
    let agent = LaunchAgent {
        label: watch_label(&entry.id),
        program_arguments: [
            utf8(git_rsrs)?.to_owned(),
            "--repo".to_owned(),
            repo.clone(),
            "--config".to_owned(),
            utf8(&paths.git_rsrs_config(&entry.id))?.to_owned(),
            "watch".to_owned(),
        ],
        working_directory: repo,
        environment_variables: environment(paths)?,
        run_at_load: true,
        keep_alive: true,
        throttle_interval: THROTTLE_INTERVAL_SECS,
        standard_out_path: utf8(&paths.stdout_log(&entry.id))?.to_owned(),
        standard_error_path: utf8(&paths.stderr_log(&entry.id))?.to_owned(),
    };
    xml(&agent)
}

/// The app's LaunchAgent: `<bundle>/Contents/MacOS/rsrsbar app`, unconditional
/// `KeepAlive`, `RunAtLoad`, `ProcessType = Interactive`. It gets the
/// watchers' `PATH`, so a Sync now runs hooks with the same tools as the
/// watcher would.
pub fn app_plist(bundle: &Path, paths: &Paths) -> Result<Vec<u8>, ServiceError> {
    let agent = AppAgent {
        label: APP_LABEL,
        program_arguments: [utf8(&bundle.join(BUNDLE_EXE))?.to_owned(), "app".to_owned()],
        environment_variables: environment(paths)?,
        run_at_load: true,
        keep_alive: true,
        process_type: "Interactive",
        standard_out_path: utf8(&paths.job_log(APP_LABEL))?.to_owned(),
        standard_error_path: utf8(&paths.job_error_log(APP_LABEL))?.to_owned(),
    };
    xml(&agent)
}

/// The heartbeat job's LaunchAgent: `<bundle>/Contents/MacOS/rsrsbar
/// check-heartbeat` every 900 s, not at load.
pub fn heartbeat_plist(bundle: &Path, paths: &Paths) -> Result<Vec<u8>, ServiceError> {
    let agent = HeartbeatAgent {
        label: HEARTBEAT_LABEL,
        program_arguments: [
            utf8(&bundle.join(BUNDLE_EXE))?.to_owned(),
            "check-heartbeat".to_owned(),
        ],
        environment_variables: environment(paths)?,
        run_at_load: false,
        start_interval: HEARTBEAT_INTERVAL_SECS,
        standard_out_path: utf8(&paths.job_log(HEARTBEAT_LABEL))?.to_owned(),
        standard_error_path: utf8(&paths.job_error_log(HEARTBEAT_LABEL))?.to_owned(),
    };
    xml(&agent)
}

fn environment(paths: &Paths) -> Result<Environment, ServiceError> {
    Ok(Environment {
        path: format!("{BASE_PATH}:{}", utf8(&paths.mise_shims())?),
    })
}

fn xml(agent: &impl Serialize) -> Result<Vec<u8>, ServiceError> {
    let mut bytes = Vec::new();
    plist::to_writer_xml(&mut bytes, agent)?;
    Ok(bytes)
}

/// Each phase's timeout and rejection exit codes (Global Constraints:
/// `reject_exit_codes = [3]` on `pre_commit` only).
fn hook_policy(phase: Phase) -> (Duration, Vec<i32>) {
    match phase {
        Phase::PreCommit => (PRE_COMMIT_TIMEOUT, vec![PRE_COMMIT_REJECT]),
        Phase::PostIntegrate => (POST_INTEGRATE_TIMEOUT, Vec::new()),
        Phase::OnResult => (ON_RESULT_TIMEOUT, Vec::new()),
    }
}

fn duration(value: Duration) -> String {
    humantime::format_duration(value).to_string()
}

fn utf8(path: &Path) -> Result<&str, ServiceError> {
    path.to_str()
        .ok_or_else(|| ServiceError::NonUtf8Path(path.to_path_buf()))
}
