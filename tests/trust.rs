use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rsrsbar::{Phase, hooks_digest, present_hooks};

const HOOK: &[u8] = b"#!/bin/sh\nexit 0\n";

/// A named edit applied to a `.autosync/` directory.
type Edit = (&'static str, fn(&Path));

fn write(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write");
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

fn digest(repo: &Path) -> String {
    hooks_digest(repo).expect("digest")
}

/// A fresh repository with `.autosync/pre-commit` (0o755).
fn base_repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    write(&repo.join(".autosync/pre-commit"), HOOK, 0o755);
    repo
}

#[test]
fn missing_dir_has_stable_empty_digest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&a).expect("mkdir");
    fs::create_dir_all(b.join(".autosync")).expect("mkdir");
    let empty = digest(&a);
    assert_eq!(empty, digest(&tmp.path().join("never-created")));
    assert_eq!(empty, digest(&b));
    let hex = empty.strip_prefix("sha256:").expect("prefix");
    assert_eq!(hex.len(), 64);
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    );
}

#[test]
fn digest_changes_on_each_kind_of_edit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = digest(&base_repo(tmp.path(), "base"));
    let edits: [Edit; 6] = [
        ("content edit", |d| {
            write(&d.join("pre-commit"), b"#!/bin/sh\nexit 1\n", 0o755)
        }),
        ("chmod 644", |d| {
            fs::set_permissions(d.join("pre-commit"), fs::Permissions::from_mode(0o644))
                .expect("chmod")
        }),
        ("rename", |d| {
            fs::rename(d.join("pre-commit"), d.join("pre-commit.bak")).expect("rename")
        }),
        ("add on-result", |d| {
            write(&d.join("on-result"), HOOK, 0o755)
        }),
        ("delete", |d| {
            fs::remove_file(d.join("pre-commit")).expect("rm")
        }),
        ("add nested", |d| {
            write(&d.join("lib/helper.sh"), b"echo\n", 0o644)
        }),
    ];
    for (i, (name, edit)) in edits.iter().enumerate() {
        let repo = base_repo(tmp.path(), &format!("edit-{i}"));
        assert_eq!(digest(&repo), base, "{name}: fresh copy equals base");
        edit(&repo.join(".autosync"));
        assert_ne!(digest(&repo), base, "{name} must change the digest");
    }
}

#[test]
fn digest_ignores_creation_order_and_mtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let files: [(&str, &[u8], u32); 3] = [
        ("pre-commit", HOOK, 0o755),
        ("on-result", b"#!/bin/sh\n", 0o755),
        ("lib/helper.sh", b"echo\n", 0o644),
    ];
    let first = tmp.path().join("first");
    for (name, bytes, mode) in files {
        write(&first.join(".autosync").join(name), bytes, mode);
    }
    let second = tmp.path().join("second");
    for (name, bytes, mode) in files.iter().rev() {
        write(&second.join(".autosync").join(name), bytes, *mode);
    }
    assert_eq!(digest(&first), digest(&second));

    let before = digest(&first);
    let old = SystemTime::now() - Duration::from_secs(86_400);
    fs::File::options()
        .write(true)
        .open(first.join(".autosync/pre-commit"))
        .and_then(|f| f.set_modified(old))
        .expect("touch");
    assert_eq!(digest(&first), before);
}

#[test]
fn symlink_is_not_followed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    write(&repo.join("outside.sh"), HOOK, 0o755);
    write(&repo.join("other.sh"), HOOK, 0o755);
    fs::create_dir_all(repo.join(".autosync")).expect("mkdir");
    let link = repo.join(".autosync/post-integrate");
    symlink("../outside.sh", &link).expect("symlink");

    let before = digest(&repo);
    write(&repo.join("outside.sh"), b"#!/bin/sh\nexit 9\n", 0o755);
    assert_eq!(digest(&repo), before, "target content is not hashed");

    fs::remove_file(&link).expect("rm link");
    symlink("../other.sh", &link).expect("retarget");
    assert_ne!(digest(&repo), before, "retargeting changes the digest");
}

#[test]
fn symlinked_directories_are_not_followed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    write(&repo.join("elsewhere/pre-commit"), HOOK, 0o755);
    fs::create_dir_all(repo.join(".autosync")).expect("mkdir");
    symlink("../elsewhere", repo.join(".autosync/lib")).expect("symlink");
    let before = digest(&repo);
    write(&repo.join("elsewhere/pre-commit"), b"changed\n", 0o755);
    assert_eq!(digest(&repo), before);

    let linked = tmp.path().join("linked");
    fs::create_dir_all(&linked).expect("mkdir");
    symlink("../repo/elsewhere", linked.join(".autosync")).expect("symlink");
    let linked_before = digest(&linked);
    assert_ne!(linked_before, digest(&tmp.path().join("missing")));
    write(
        &repo.join("elsewhere/pre-commit"),
        b"changed again\n",
        0o755,
    );
    assert_eq!(digest(&linked), linked_before);
    assert!(present_hooks(&linked).is_empty());
}

#[test]
fn fields_are_framed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    // Unframed, both trees would hash the same byte stream:
    // "a" 1 0 "b" | "c" 1 0 "d"  vs  "a" 1 0 "bc\x01\x00d".
    write(&a.join(".autosync/a"), b"b", 0o644);
    write(&a.join(".autosync/c"), b"d", 0o644);
    write(&b.join(".autosync/a"), b"bc\x01\x00d", 0o644);
    assert_ne!(digest(&a), digest(&b));
}

#[test]
fn present_hooks_lists_executables() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    write(&repo.join(".autosync/pre-commit"), HOOK, 0o755);
    write(&repo.join(".autosync/on-result"), HOOK, 0o644);
    assert_eq!(present_hooks(&repo), vec![Phase::PreCommit]);
    write(&repo.join("tool.sh"), HOOK, 0o755);
    symlink("../tool.sh", repo.join(".autosync/post-integrate")).expect("symlink");
    assert_eq!(
        present_hooks(&repo),
        vec![Phase::PreCommit, Phase::PostIntegrate],
        "a link to an executable is what run-hook would execute"
    );
    assert!(present_hooks(&tmp.path().join("none")).is_empty());

    assert_eq!(Phase::ALL.len(), 3);
    let names: Vec<_> = Phase::ALL
        .iter()
        .map(|p| (p.file_name(), p.config_key()))
        .collect();
    assert_eq!(
        names,
        [
            ("pre-commit", "pre_commit"),
            ("post-integrate", "post_integrate"),
            ("on-result", "on_result"),
        ]
    );
}
