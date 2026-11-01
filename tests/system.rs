use std::time::{Duration, UNIX_EPOCH};

use rsrsbar::{parse_etime, parse_kern_time};

#[test]
fn kern_time() {
    assert_eq!(
        parse_kern_time("{ sec = 1793400000, usec = 12 } Sat Nov  1 09:00:00 2026\n"),
        Some(UNIX_EPOCH + Duration::from_secs(1_793_400_000))
    );
    assert_eq!(parse_kern_time("{ sec = 0, usec = 0 }"), Some(UNIX_EPOCH));
    assert_eq!(parse_kern_time("garbage"), None);
    assert_eq!(parse_kern_time("{ usec = 12 }"), None);
    assert_eq!(parse_kern_time("{ sec = -1, usec = 0 }"), None);
    assert_eq!(
        parse_kern_time("{ sec = 18446744073709551615, usec = 0 }"),
        None
    );
}

#[test]
fn etime() {
    assert_eq!(parse_etime("05:07"), Some(Duration::from_secs(307)));
    assert_eq!(parse_etime("   1:02:03\n"), Some(Duration::from_secs(3723)));
    assert_eq!(
        parse_etime("2-01:00:00"),
        Some(Duration::from_secs(176_400))
    );
    assert_eq!(parse_etime("  "), None);
    assert_eq!(parse_etime("07"), None);
    assert_eq!(parse_etime("1:2:3:4"), None);
    assert_eq!(parse_etime("2-05:07"), None);
    assert_eq!(parse_etime("ab:cd"), None);
    assert_eq!(parse_etime("18446744073709551615:00"), None);
    assert_eq!(parse_etime("18446744073709551615-00:00:00"), None);
    assert_eq!(
        parse_etime("00:18446744073709551615"),
        Some(Duration::from_secs(u64::MAX))
    );
    assert_eq!(parse_etime("01:18446744073709551615"), None);
}
