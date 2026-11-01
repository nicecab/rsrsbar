//! Child processes with a deadline: nothing rsrsbar starts may wait
//! forever, and nothing it starts reads stdin.

use std::fs::File;
use std::io::{self, Read, Seek};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);
/// The deadline for every short-lived helper rsrsbar runs (`launchctl`,
/// `sysctl`, `ps`, `git`, `diff`, `osascript`, `git-rsrs --version`, `open`).
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// Runs `command` with stdin closed and collects its output, as
/// [`Command::output`] does, but kills and reaps it once `deadline` passes
/// (an [`io::ErrorKind::TimedOut`] error). Output goes to temporary files,
/// not pipes: nothing to drain while waiting, and a grandchild that keeps
/// a descriptor open cannot block the read.
pub fn output_within(command: &mut Command, deadline: Duration) -> io::Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let Some(status) = wait_until(&mut child, deadline)? else {
        child.kill()?;
        child.wait()?;
        let program = command.get_program().to_string_lossy().into_owned();
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{program} did not finish within {} s",
                deadline.as_secs_f64()
            ),
        ));
    };
    Ok(Output {
        status,
        stdout: read_all(&mut stdout)?,
        stderr: read_all(&mut stderr)?,
    })
}

/// Polls `child` until it exits or `deadline` passes (`Ok(None)`).
pub(crate) fn wait_until(child: &mut Child, deadline: Duration) -> io::Result<Option<ExitStatus>> {
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

fn read_all(file: &mut File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.rewind()?;
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}
