//! Kernel and process time: how long the Mac has been awake and how long a
//! process has been running, read through `sysctl` and `ps`.

use std::io;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SYSCTL: &str = "/usr/sbin/sysctl";
const PS: &str = "/bin/ps";
const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 60 * SECS_PER_MINUTE;
const SECS_PER_DAY: u64 = 24 * SECS_PER_HOUR;

/// Parses `sysctl -n kern.boottime` / `kern.waketime` output,
/// `{ sec = 1793400000, usec = 12 } Sat Nov  1 …`, to whole seconds.
/// A Mac that never slept reports `sec = 0`, which is `UNIX_EPOCH`; a value
/// outside `SystemTime`'s range is `None`.
pub fn parse_kern_time(output: &str) -> Option<SystemTime> {
    let fields = output.trim().strip_prefix('{')?.split_once('}')?.0;
    let secs = fields.split(',').find_map(|field| {
        let (key, value) = field.split_once('=')?;
        (key.trim() == "sec").then(|| value.trim().parse::<u64>().ok())?
    })?;
    // Checked: a seconds field beyond `SystemTime`'s range is garbage.
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// The later of the last boot and the last wake.
pub fn awake_since() -> io::Result<SystemTime> {
    let boot = kern_time("kern.boottime")?;
    let wake = kern_time("kern.waketime")?;
    Ok(boot.max(wake))
}

fn kern_time(name: &str) -> io::Result<SystemTime> {
    let output = Command::new(SYSCTL).args(["-n", name]).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!("sysctl {name} failed")));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_kern_time(&text).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected sysctl {name} output: {}", text.trim()),
        )
    })
}

/// Parses `ps -o etime=` output, `[[dd-]hh:]mm:ss`.
pub fn parse_etime(output: &str) -> Option<Duration> {
    let text = output.trim();
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (Some(number(days)?), clock),
        None => (None, text),
    };
    let parts = clock.split(':').map(number).collect::<Option<Vec<u64>>>()?;
    let units: &[u64] = match (days, parts.len()) {
        (None, 2) => &[SECS_PER_MINUTE, 1],
        (_, 3) => &[SECS_PER_HOUR, SECS_PER_MINUTE, 1],
        _ => return None,
    };
    // Checked: a field too large for a u64 second count is garbage, not time.
    let clock = parts
        .iter()
        .zip(units)
        .try_fold(0u64, |total, (value, unit)| {
            total.checked_add(value.checked_mul(*unit)?)
        })?;
    let day_secs = days.unwrap_or(0).checked_mul(SECS_PER_DAY)?;
    Some(Duration::from_secs(clock.checked_add(day_secs)?))
}

fn number(text: &str) -> Option<u64> {
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| text.parse().ok())?
}

/// How long `pid` has been running; `Ok(None)` when there is no such
/// process.
pub fn process_age(pid: u32) -> io::Result<Option<Duration>> {
    let output = Command::new(PS)
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || text.trim().is_empty() {
        return Ok(None);
    }
    parse_etime(&text).map(Some).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected ps etime output: {}", text.trim()),
        )
    })
}
