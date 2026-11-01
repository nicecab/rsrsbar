use std::io::ErrorKind;
use std::process::Command;
use std::time::{Duration, Instant};

use rsrsbar::output_within;

#[test]
fn hanging_child_is_killed_at_deadline() {
    let started = Instant::now();
    let err = output_within(
        Command::new("/bin/sleep").arg("30"),
        Duration::from_millis(300),
    )
    .expect_err("deadline must stop the child");
    assert_eq!(err.kind(), ErrorKind::TimedOut, "{err}");
    assert!(err.to_string().contains("/bin/sleep"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn finished_child_output_is_collected() {
    let output = output_within(
        Command::new("/bin/sh").args(["-c", "read line; echo out:$line; echo err >&2; exit 3"]),
        Duration::from_secs(10),
    )
    .expect("runs");
    assert_eq!(output.status.code(), Some(3));
    // stdin is closed, so `read` gets nothing instead of waiting.
    assert_eq!(String::from_utf8_lossy(&output.stdout), "out:\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "err\n");
}
