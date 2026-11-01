use std::fs;
use std::path::{Path, PathBuf};

use rsrsbar::{Paths, RepoId, watch_label, write_atomic};

fn id(s: &str) -> RepoId {
    RepoId::new(s).expect("valid id")
}

#[test]
fn repo_id_rules() {
    assert_eq!(id("obaidoc").as_str(), "obaidoc");
    assert_eq!(id("tom-coten-hub").as_str(), "tom-coten-hub");
    assert!(RepoId::new(&"a".repeat(63)).is_ok());
    for bad in ["", "-x", "A", "a/b", "a_b", "a b", "é"] {
        assert!(RepoId::new(bad).is_err(), "{bad:?} must be rejected");
    }
    assert!(RepoId::new(&"a".repeat(64)).is_err());
}

#[test]
fn repo_id_deserialization_validates() {
    let ok: RepoId = serde_json::from_str("\"obaidoc\"").expect("valid id");
    assert_eq!(ok, id("obaidoc"));
    assert_eq!(
        serde_json::to_string(&ok).expect("serialize"),
        "\"obaidoc\""
    );
    assert!(serde_json::from_str::<RepoId>("\"-x\"").is_err());
    assert!(serde_json::from_str::<RepoId>("\"A\"").is_err());
}

#[test]
fn suggest_from_path() {
    let suggest = |p: &str| RepoId::suggest(Path::new(p));
    assert_eq!(
        suggest("/x/tomoya55/my-llm-config").expect("ok"),
        id("my-llm-config")
    );
    assert_eq!(suggest("/x/My Repo_v2").expect("ok"), id("my-repo-v2"));
    assert_eq!(suggest("/x/a--__--b").expect("ok"), id("a-b"));
    assert!(suggest("/x/___").is_err());
    assert!(suggest("/").is_err());
    assert!(suggest(&format!("/x/{}", "a".repeat(64))).is_err());
}

#[test]
fn paths_layout() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let obaidoc = id("obaidoc");
    let support = "/Users/t/Library/Application Support/rsrsbar";
    assert_eq!(paths.support_dir(), PathBuf::from(support));
    assert_eq!(
        paths.registry(),
        PathBuf::from(format!("{support}/registry.toml"))
    );
    assert_eq!(
        paths.state(),
        PathBuf::from(format!("{support}/state.json"))
    );
    assert_eq!(
        paths.repo_dir(&obaidoc),
        PathBuf::from(format!("{support}/repos/obaidoc"))
    );
    assert_eq!(
        paths.git_rsrs_config(&obaidoc),
        PathBuf::from(format!("{support}/repos/obaidoc/git-rsrs.toml"))
    );
    assert_eq!(
        paths.status_file(&obaidoc),
        PathBuf::from(format!("{support}/repos/obaidoc/status.json"))
    );
    assert_eq!(watch_label(&obaidoc), "com.nicecab.rsrsbar.watch.obaidoc");
    assert_eq!(
        paths.launch_agent(&obaidoc),
        PathBuf::from("/Users/t/Library/LaunchAgents/com.nicecab.rsrsbar.watch.obaidoc.plist")
    );
    assert_eq!(
        paths.stdout_log(&obaidoc),
        PathBuf::from("/Users/t/Library/Logs/com.nicecab.rsrsbar.watch.obaidoc.log")
    );
    assert_eq!(
        paths.stderr_log(&obaidoc),
        PathBuf::from("/Users/t/Library/Logs/com.nicecab.rsrsbar.watch.obaidoc.error.log")
    );
    assert_eq!(
        paths.mise_shims(),
        PathBuf::from("/Users/t/.local/share/mise/shims")
    );
}

#[test]
fn write_atomic_creates_parent_and_replaces() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("a/b");
    let file = dir.join("f");

    write_atomic(&file, b"first").expect("first write");
    write_atomic(&file, b"second").expect("second write");

    assert_eq!(fs::read(&file).expect("read"), b"second");
    let entries: Vec<_> = fs::read_dir(&dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("f")]);
}
