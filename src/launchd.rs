//! launchd behind a small trait: `Launchctl` drives `/bin/launchctl` in the
//! user's `gui/<uid>` domain, `Recording` is a test double.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Command, Output};

const LAUNCHCTL: &str = "/bin/launchctl";
/// `launchctl bootout` of a label that is not loaded ("No such process").
const BOOTOUT_NOT_LOADED: i32 = 3;
/// `launchctl print` / `kickstart` of a label that is not loaded.
const NOT_FOUND: i32 = 113;
/// `launchctl bootstrap` of a label that is already loaded.
const BOOTSTRAP_FAILED: i32 = 5;
const NOT_LOADED_MESSAGES: [&str; 2] = ["No such process", "Could not find service"];
const FIRST_FAKE_PID: u32 = 10_000;

/// A loaded job; `pid` is `None` while it is loaded but not running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobInfo {
    pub pid: Option<u32>,
}

/// Errors from talking to launchd.
#[derive(Debug, thiserror::Error)]
pub enum LaunchdError {
    #[error("cannot run launchctl: {0}")]
    Io(#[from] io::Error),
    #[error("launchctl {} failed (exit {}): {}", .args.join(" "), exit_text(*.status), .stderr.trim())]
    Command {
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
}

fn exit_text(status: Option<i32>) -> String {
    status.map_or_else(|| "by signal".to_owned(), |code| code.to_string())
}

/// The launchd operations rsrsbar needs, addressed by job label.
pub trait Launchd {
    /// Loads the LaunchAgent at `plist`.
    fn bootstrap(&self, plist: &Path) -> Result<(), LaunchdError>;
    /// Unloads `label`; a label that is not loaded is `Ok(())`.
    fn bootout(&self, label: &str) -> Result<(), LaunchdError>;
    /// Restarts `label` (`kickstart -k`).
    fn kickstart(&self, label: &str) -> Result<(), LaunchdError>;
    /// The job's state, `Ok(None)` when it is not loaded.
    fn job(&self, label: &str) -> Result<Option<JobInfo>, LaunchdError>;
}

/// Reads the top-level `pid = N` line of `launchctl print` output; `pid`
/// lines inside nested blocks belong to something else.
pub fn parse_print(output: &str) -> JobInfo {
    let mut depth = 0usize;
    for line in output.lines().map(str::trim) {
        if line == "}" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth == 1
            && let Some(pid) = line.strip_prefix("pid = ").and_then(|v| v.parse().ok())
        {
            return JobInfo { pid: Some(pid) };
        }
        if line.ends_with('{') {
            depth += 1;
        }
    }
    JobInfo { pid: None }
}

/// `/bin/launchctl` in the `gui/<uid>` domain.
#[derive(Clone, Copy, Debug)]
pub struct Launchctl {
    uid: u32,
}

impl Launchctl {
    /// The domain of the current user.
    pub fn current() -> Self {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        Self { uid }
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self, label: &str) -> String {
        format!("gui/{}/{label}", self.uid)
    }

    fn run(&self, args: &[&OsStr]) -> Result<Output, LaunchdError> {
        Ok(Command::new(LAUNCHCTL).args(args).output()?)
    }
}

fn command_error(args: &[&OsStr], output: &Output) -> LaunchdError {
    LaunchdError::Command {
        args: args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        status: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn checked(args: &[&OsStr], output: &Output) -> Result<(), LaunchdError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(args, output))
    }
}

fn reports_not_loaded(output: &Output, code: i32) -> bool {
    let text = [&output.stdout[..], &output.stderr[..]].concat();
    let text = String::from_utf8_lossy(&text);
    output.status.code() == Some(code) || NOT_LOADED_MESSAGES.iter().any(|m| text.contains(m))
}

impl Launchd for Launchctl {
    fn bootstrap(&self, plist: &Path) -> Result<(), LaunchdError> {
        let domain = self.domain();
        let args = [
            OsStr::new("bootstrap"),
            OsStr::new(&domain),
            plist.as_os_str(),
        ];
        checked(&args, &self.run(&args)?)
    }

    fn bootout(&self, label: &str) -> Result<(), LaunchdError> {
        let target = self.target(label);
        let args = [OsStr::new("bootout"), OsStr::new(&target)];
        let output = self.run(&args)?;
        if !output.status.success() && reports_not_loaded(&output, BOOTOUT_NOT_LOADED) {
            return Ok(());
        }
        checked(&args, &output)
    }

    fn kickstart(&self, label: &str) -> Result<(), LaunchdError> {
        let target = self.target(label);
        let args = [
            OsStr::new("kickstart"),
            OsStr::new("-k"),
            OsStr::new(&target),
        ];
        checked(&args, &self.run(&args)?)
    }

    fn job(&self, label: &str) -> Result<Option<JobInfo>, LaunchdError> {
        let target = self.target(label);
        let args = [OsStr::new("print"), OsStr::new(&target)];
        let output = self.run(&args)?;
        if output.status.success() {
            return Ok(Some(parse_print(&String::from_utf8_lossy(&output.stdout))));
        }
        if reports_not_loaded(&output, NOT_FOUND) {
            return Ok(None);
        }
        Err(command_error(&args, &output))
    }
}

/// A test double for [`Launchd`]: records every `bootstrap`, `bootout` and
/// `kickstart` call, successful or refused, as a string (`"bootstrap <path>"`,
/// `"bootout <label>"`, `"kickstart <label>"`; `job` lookups are not
/// recorded) and
/// keeps the loaded labels, refusing what real launchd refuses (bootstrap of
/// a loaded label, kickstart of an unloaded one). A bootstrapped label is
/// the plist's file stem and gets a fresh fake PID, as with `RunAtLoad`.
#[derive(Debug, Default)]
pub struct Recording {
    state: RefCell<RecordingState>,
}

#[derive(Debug, Default)]
struct RecordingState {
    calls: Vec<String>,
    loaded: BTreeMap<String, Option<u32>>,
    fail_next_bootstrap: bool,
    spawned: u32,
}

impl RecordingState {
    fn next_pid(&mut self) -> u32 {
        self.spawned += 1;
        FIRST_FAKE_PID + self.spawned
    }
}

fn refused(op: &str, target: String, status: i32, stderr: &str) -> LaunchdError {
    LaunchdError::Command {
        args: vec![op.to_owned(), target],
        status: Some(status),
        stderr: stderr.to_owned(),
    }
}

impl Recording {
    /// A double with nothing loaded.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same double with `label` also loaded, running as `pid` if given.
    pub fn with_loaded(self, label: &str, pid: Option<u32>) -> Self {
        self.state.borrow_mut().loaded.insert(label.to_owned(), pid);
        self
    }

    /// Makes the next `bootstrap` fail without loading anything.
    pub fn fail_next_bootstrap(&self) {
        self.state.borrow_mut().fail_next_bootstrap = true;
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<String> {
        self.state.borrow().calls.clone()
    }
}

impl Launchd for Recording {
    fn bootstrap(&self, plist: &Path) -> Result<(), LaunchdError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(format!("bootstrap {}", plist.display()));
        let label = plist.file_stem().map(|s| s.to_string_lossy().into_owned());
        let shown = plist.display().to_string();
        match label {
            Some(label) if !state.fail_next_bootstrap && !state.loaded.contains_key(&label) => {
                let pid = state.next_pid();
                state.loaded.insert(label, Some(pid));
                Ok(())
            }
            _ => {
                state.fail_next_bootstrap = false;
                Err(refused(
                    "bootstrap",
                    shown,
                    BOOTSTRAP_FAILED,
                    "Bootstrap failed: 5: Input/output error",
                ))
            }
        }
    }

    fn bootout(&self, label: &str) -> Result<(), LaunchdError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(format!("bootout {label}"));
        state.loaded.remove(label);
        Ok(())
    }

    fn kickstart(&self, label: &str) -> Result<(), LaunchdError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(format!("kickstart {label}"));
        if !state.loaded.contains_key(label) {
            return Err(refused(
                "kickstart",
                label.to_owned(),
                NOT_FOUND,
                "Could not find service",
            ));
        }
        let pid = state.next_pid();
        state.loaded.insert(label.to_owned(), Some(pid));
        Ok(())
    }

    fn job(&self, label: &str) -> Result<Option<JobInfo>, LaunchdError> {
        let state = self.state.borrow();
        Ok(state.loaded.get(label).map(|&pid| JobInfo { pid }))
    }
}
