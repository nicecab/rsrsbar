//! The `rsrsbar.app` bundle: its `Info.plist` and finding the bundle the
//! running executable lives in.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Bundle identifier; also the ad-hoc signing identifier and the app's
/// launchd label.
pub const BUNDLE_ID: &str = "com.nicecab.rsrsbar";

/// The bundle's `Info.plist` for `version` (used as both the short and the
/// build version). `LSUIElement` keeps the app out of the Dock.
pub fn info_plist(version: &str) -> Vec<u8> {
    let version = xml_escape(version);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>rsrsbar</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>rsrsbar</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSMinimumSystemVersion</key>
	<string>14.0</string>
	<key>LSUIElement</key>
	<true/>
</dict>
</plist>
"#
    )
    .into_bytes()
}

/// The `.app` directory when `exe` is exactly `<x>.app/Contents/MacOS/<name>`
/// (`<x>` non-empty, `.app` lower case), else `None`. Purely lexical: the
/// caller passes a canonicalized path when it needs a real one.
pub fn bundle_root(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent().filter(|_| exe.file_name().is_some())?;
    let contents = macos.parent().filter(|_| named(macos, "MacOS"))?;
    let app = contents.parent().filter(|_| named(contents, "Contents"))?;
    let stem = app.file_name()?.to_str()?.strip_suffix(".app")?;
    (!stem.is_empty()).then(|| app.to_path_buf())
}

fn named(path: &Path, name: &str) -> bool {
    path.file_name() == Some(OsStr::new(name))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
