use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use plist::Value;
use rsrsbar::{bundle_root, info_plist};

fn plist_dict(bytes: &[u8]) -> plist::Dictionary {
    Value::from_reader(Cursor::new(bytes))
        .expect("parse plist")
        .into_dictionary()
        .expect("dictionary")
}

fn string<'a>(dict: &'a plist::Dictionary, key: &str) -> &'a str {
    dict.get(key)
        .and_then(Value::as_string)
        .unwrap_or_else(|| panic!("{key} is a string"))
}

#[test]
fn info_plist_keys() {
    let bytes = info_plist("9.8.7");
    let dict = plist_dict(&bytes);

    assert_eq!(string(&dict, "CFBundleIdentifier"), "com.nicecab.rsrsbar");
    assert_eq!(string(&dict, "CFBundleExecutable"), "rsrsbar");
    assert_eq!(string(&dict, "CFBundleName"), "rsrsbar");
    assert_eq!(string(&dict, "CFBundlePackageType"), "APPL");
    assert_eq!(string(&dict, "CFBundleShortVersionString"), "9.8.7");
    assert_eq!(string(&dict, "CFBundleVersion"), "9.8.7");
    assert_eq!(string(&dict, "LSMinimumSystemVersion"), "14.0");
    assert_eq!(dict.get("LSUIElement"), Some(&Value::Boolean(true)));

    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("Info.plist");
    std::fs::write(&file, &bytes).expect("write plist");
    let lint = Command::new("/usr/bin/plutil")
        .arg("-lint")
        .arg(&file)
        .output()
        .expect("run plutil");
    assert!(lint.status.success(), "plutil -lint: {lint:?}");
}

#[test]
fn bundle_root_detection() {
    let detect = |exe: &str| bundle_root(Path::new(exe));

    assert_eq!(
        detect("/A/rsrsbar.app/Contents/MacOS/rsrsbar"),
        Some(PathBuf::from("/A/rsrsbar.app"))
    );
    assert_eq!(
        detect("/A/b c/Other.app/Contents/MacOS/other"),
        Some(PathBuf::from("/A/b c/Other.app"))
    );
    assert_eq!(detect("/A/target/release/rsrsbar"), None);
    assert_eq!(detect("/A/rsrsbar.app/Contents/MacOS"), None);
    assert_eq!(detect("/A/foo/Contents/MacOS/rsrsbar"), None);
    assert_eq!(detect("/A/rsrsbar.app/Contents/Resources/rsrsbar"), None);
    assert_eq!(detect("/A/rsrsbar.app/Contents/MacOS/"), None);
    assert_eq!(detect("/A/.app/Contents/MacOS/rsrsbar"), None);
    assert_eq!(
        detect("rsrsbar.app/Contents/MacOS/rsrsbar"),
        Some(PathBuf::from("rsrsbar.app"))
    );
    assert_eq!(detect("rsrsbar"), None);
}
