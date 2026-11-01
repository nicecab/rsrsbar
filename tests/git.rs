mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::git;
use rsrsbar::{current_branch, probe};

/// A primary checkout on `main` with one commit and an `origin` remote.
fn primary(root: &Path) -> PathBuf {
    let bare = root.join("origin.git");
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("mkdir repo");
    git(root, &["init", "--bare", "-b", "main", "origin.git"]);
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-m", "first"]);
    git(
        &repo,
        &["remote", "add", "origin", bare.to_str().expect("utf-8")],
    );
    repo
}

#[test]
fn probe_primary_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = primary(tmp.path());
    let canonical = repo.canonicalize().expect("canonical");

    let found = probe(&repo).expect("probe");
    assert_eq!(found.toplevel, canonical);
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert!(!found.is_linked_worktree);
    assert_eq!(found.remotes, vec!["origin".to_owned()]);

    let sub = repo.join("sub");
    fs::create_dir_all(&sub).expect("mkdir sub");
    assert_eq!(probe(&sub).expect("probe sub"), found);
    assert_eq!(current_branch(&repo).as_deref(), Some("main"));
}

#[test]
fn probe_linked_worktree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = primary(tmp.path());
    git(&repo, &["worktree", "add", "../wt", "-b", "feat"]);
    let worktree = tmp.path().join("wt");

    let found = probe(&worktree).expect("probe worktree");
    assert!(found.is_linked_worktree);
    assert_eq!(found.branch.as_deref(), Some("feat"));
    assert_eq!(found.toplevel, worktree.canonicalize().expect("canonical"));
    assert!(!probe(&repo).expect("probe primary").is_linked_worktree);
}

#[test]
fn probe_detached_and_non_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = primary(tmp.path());
    git(&repo, &["checkout", "--detach"]);

    assert_eq!(probe(&repo).expect("probe detached").branch, None);
    assert_eq!(current_branch(&repo), None);

    let plain = tmp.path().join("plain");
    fs::create_dir_all(&plain).expect("mkdir plain");
    assert!(probe(&plain).is_err());
    assert_eq!(current_branch(&plain), None);
    assert!(probe(&tmp.path().join("missing")).is_err());
}

#[test]
fn test_git_ignores_user_and_system_config() {
    let outside = tempfile::tempdir().expect("tempdir");
    let output = common::git_command()
        .arg("-C")
        .arg(outside.path())
        .args(["config", "--show-origin", "--list"])
        .output()
        .expect("run git");
    let text = String::from_utf8_lossy(&output.stdout);
    let config_files: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("file:"))
        .collect();
    assert!(config_files.is_empty(), "{config_files:?}");
}
