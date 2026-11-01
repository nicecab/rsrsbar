use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rsrsbar::{AppSettings, Registry, RegistryError, RepoEntry, RepoId};

fn id(s: &str) -> RepoId {
    RepoId::new(s).expect("valid id")
}

fn entry(name: &str, path: &Path) -> RepoEntry {
    RepoEntry {
        id: id(name),
        path: path.to_path_buf(),
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        lock_name: None,
        interval: None,
        quiet_period: None,
        git_timeout: None,
        lock_timeout: None,
        stall_threshold: None,
        paused_limit: None,
        offline_limit: None,
        other_branch_limit: None,
        paused_at: None,
        allowed_hooks: None,
    }
}

fn mkdir(path: PathBuf) -> PathBuf {
    fs::create_dir_all(&path).expect("mkdir");
    path
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

#[test]
fn defaults_and_threshold() {
    let base = entry("obaidoc", Path::new("/x/obaidoc"));
    let s = base.settings().expect("defaults parse");
    assert_eq!(s.interval, secs(600));
    assert_eq!(s.quiet_period, secs(30));
    assert_eq!(s.git_timeout, secs(120));
    assert_eq!(s.lock_timeout, secs(30));
    assert_eq!(s.lock_name, None);
    assert_eq!(s.stall_threshold, secs(1800));

    let five = RepoEntry {
        interval: Some("5m".to_owned()),
        ..base.clone()
    };
    assert_eq!(five.settings().expect("5m").stall_threshold, secs(900));

    let hour = RepoEntry {
        stall_threshold: Some("1h".to_owned()),
        ..base
    };
    assert_eq!(hour.settings().expect("1h").stall_threshold, secs(3600));
}

#[test]
fn bad_duration_rejected_on_load() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("registry.toml");
    let text = "version = 1\n\n[[repo]]\nid = \"obaidoc\"\npath = \"/x/obaidoc\"\n\
                branch = \"main\"\nremote = \"origin\"\ninterval = \"soon\"\n";
    fs::write(&path, text).expect("write");
    let err = Registry::load(&path).expect_err("bad duration must fail");
    assert!(err.to_string().contains("obaidoc"), "{err}");
}

#[test]
fn load_missing_and_unknown_version() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("registry.toml");
    let empty = Registry::load(&path).expect("missing file is empty");
    assert_eq!(empty.version, 1);
    assert!(empty.repos.is_empty());

    fs::write(&path, "version = 2\n").expect("write");
    assert!(matches!(
        Registry::load(&path),
        Err(RegistryError::UnsupportedVersion(2))
    ));
}

#[test]
fn round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("sub/registry.toml");
    let docs = RepoEntry {
        lock_name: Some("docs-maintenance.lock".to_owned()),
        paused_at: Some("2026-11-01T10:00:00Z".to_owned()),
        ..entry("obaidoc", &mkdir(tmp.path().join("obaidoc")))
    };
    let plain = entry("tom-coten-hub", &mkdir(tmp.path().join("My Repo/hub")));
    let registry = Registry::default()
        .with_added(docs)
        .and_then(|r| r.with_added(plain))
        .expect("added");

    registry.save(&path).expect("save");
    assert_eq!(Registry::load(&path).expect("load"), registry);

    let text = fs::read_to_string(&path).expect("read");
    assert_eq!(text.matches("[[repo]]").count(), 2, "{text}");
    for key in [
        "interval",
        "quiet_period",
        "git_timeout",
        "lock_timeout",
        "stall_threshold",
        "allowed_hooks",
    ] {
        assert!(!text.contains(key), "{key} must be omitted:\n{text}");
    }
    assert_eq!(text.matches("lock_name").count(), 1, "{text}");
    assert_eq!(text.matches("paused_at").count(), 1, "{text}");
}

#[test]
fn immutable_updates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).expect("mkdir");
    let original = Registry::default()
        .with_added(entry("a", &repo))
        .expect("first add");

    let added = original
        .with_added(entry("b", &mkdir(tmp.path().join("b"))))
        .expect("second add");
    assert_eq!(original.repos.len(), 1);
    assert_eq!(added.repos.len(), 2);

    let other = mkdir(tmp.path().join("other"));
    assert!(matches!(
        original.with_added(entry("a", &other)),
        Err(RegistryError::DuplicateId(_))
    ));
    let same_dir = repo.join(".");
    assert!(original.with_added(entry("c", &same_dir)).is_err());
    let trailing_slash = PathBuf::from(format!("{}/", repo.display()));
    assert!(original.with_added(entry("c", &trailing_slash)).is_err());

    assert!(original.without(&id("nope")).is_err());
    assert!(
        added
            .without(&id("b"))
            .expect("remove")
            .get(&id("b"))
            .is_none()
    );
    assert_eq!(added.repos.len(), 2);

    let paused = added
        .with_entry(&id("b"), |e| RepoEntry {
            paused_at: Some("2026-11-01T10:00:00Z".to_owned()),
            ..e.clone()
        })
        .expect("edit");
    assert!(
        paused
            .get(&id("b"))
            .and_then(|e| e.paused_at.as_ref())
            .is_some()
    );
    assert!(
        added
            .get(&id("b"))
            .and_then(|e| e.paused_at.as_ref())
            .is_none()
    );
    assert!(added.with_entry(&id("nope"), RepoEntry::clone).is_err());
    let rename_to_existing = added.with_entry(&id("b"), |e| RepoEntry {
        id: id("a"),
        ..e.clone()
    });
    assert!(rename_to_existing.is_err());
}

#[test]
fn concurrent_updates_lose_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = Arc::new(tmp.path().join("registry.toml"));
    let repos: Vec<PathBuf> = (0..8)
        .map(|n| mkdir(tmp.path().join(format!("repo-{n}"))))
        .collect();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|n| {
            let repos = repos.clone();
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let e = entry(&format!("repo-{n}"), &repos[n]);
                barrier.wait();
                Registry::update(&path, |r| r.with_added(e)).expect("update")
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread");
    }
    assert_eq!(Registry::load(&path).expect("load").repos.len(), 8);
}

#[test]
fn huge_interval_is_rejected_without_panicking() {
    let huge = RepoEntry {
        interval: Some("18446744073709551615s".to_owned()),
        ..entry("huge", Path::new("/x/huge"))
    };
    let explicit = RepoEntry {
        stall_threshold: Some("1h".to_owned()),
        ..huge.clone()
    };
    for candidate in [huge, explicit] {
        let err = candidate.settings().expect_err("interval exceeds one year");
        assert!(
            matches!(
                err,
                RegistryError::DurationOutOfRange {
                    key: "interval",
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("huge"), "{err}");
    }
}

#[test]
fn durations_git_rsrs_rejects_are_refused() {
    let year = "365days";
    let over_a_year = "365days 1s";
    for key in ["interval", "quiet_period", "git_timeout", "lock_timeout"] {
        let with = |value: &str| {
            let value = Some(value.to_owned());
            let base = entry("bounds", Path::new("/x/bounds"));
            match key {
                "interval" => RepoEntry {
                    interval: value,
                    ..base
                },
                "quiet_period" => RepoEntry {
                    quiet_period: value,
                    ..base
                },
                "git_timeout" => RepoEntry {
                    git_timeout: value,
                    ..base
                },
                _ => RepoEntry {
                    lock_timeout: value,
                    ..base
                },
            }
        };
        for bad in ["0s", over_a_year] {
            let err = with(bad).settings().expect_err("out of range");
            assert!(
                matches!(&err, RegistryError::DurationOutOfRange { key: k, .. } if *k == key),
                "{key} = {bad}: {err}"
            );
            assert!(err.to_string().contains("bounds"), "{err}");
        }
        assert!(with(year).settings().is_ok(), "{key} = one year is allowed");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("registry.toml");
    let text = "version = 1\n\n[[repo]]\nid = \"obaidoc\"\npath = \"/x/obaidoc\"\n\
                branch = \"main\"\nremote = \"origin\"\ninterval = \"0s\"\n";
    fs::write(&path, text).expect("write");
    let err = Registry::load(&path).expect_err("zero interval must fail on load");
    assert!(err.to_string().contains("obaidoc"), "{err}");
}

#[test]
fn new_path_must_resolve() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let via_missing = tmp.path().join("missing/../repo");

    let err = Registry::default()
        .with_added(entry("a", &via_missing))
        .expect_err("unresolvable path");
    assert!(
        matches!(err, RegistryError::UnresolvablePath { .. }),
        "{err}"
    );
    assert!(Registry::default().with_added(entry("b", &repo)).is_err());

    mkdir(repo.clone());
    mkdir(tmp.path().join("missing"));
    let registry = Registry::default()
        .with_added(entry("b", &repo))
        .expect("existing path");
    assert!(matches!(
        registry.with_added(entry("a", &via_missing)),
        Err(RegistryError::DuplicatePath { .. })
    ));
}

#[test]
fn moved_checkout_does_not_block_edits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let gone = mkdir(tmp.path().join("gone"));
    let registry = Registry::default()
        .with_added(entry("gone", &gone))
        .expect("add");
    fs::remove_dir(&gone).expect("rmdir");

    let more = registry
        .with_added(entry("new", &mkdir(tmp.path().join("new"))))
        .expect("unrelated add");
    let paused = more
        .with_entry(&id("gone"), |e| RepoEntry {
            paused_at: Some("2026-11-01T10:00:00Z".to_owned()),
            ..e.clone()
        })
        .expect("edit keeps the stored path");
    assert!(
        paused
            .get(&id("gone"))
            .and_then(|e| e.paused_at.as_ref())
            .is_some()
    );
    assert!(matches!(
        more.with_entry(&id("gone"), |e| RepoEntry {
            path: tmp.path().join("elsewhere"),
            ..e.clone()
        }),
        Err(RegistryError::UnresolvablePath { .. })
    ));
}

#[test]
fn identity_is_the_canonical_path_at_registration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = mkdir(tmp.path().join("repo"));
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&repo, &alias).expect("symlink");

    let registry = Registry::default()
        .with_added(entry("a", &alias))
        .expect("add via alias");
    let canonical = fs::canonicalize(&repo).expect("canonicalize");
    assert_eq!(
        registry.get(&id("a")).map(|e| e.path.clone()),
        Some(canonical)
    );

    fs::remove_file(&alias).expect("remove symlink");
    assert!(matches!(
        registry.with_added(entry("b", &repo)),
        Err(RegistryError::DuplicatePath { .. })
    ));
}

#[test]
fn editors_never_return_a_registry_load_would_reject() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("registry.toml");
    let first = mkdir(tmp.path().join("first"));
    let second = mkdir(tmp.path().join("second"));
    Registry::update(&path, |r| r.with_added(entry("first", &first))).expect("seed");

    for bad in ["0s", "soon"] {
        let added = RepoEntry {
            interval: Some(bad.to_owned()),
            ..entry("second", &second)
        };
        let err = Registry::update(&path, |r| r.with_added(added)).expect_err("add refused");
        assert!(err.to_string().contains("second"), "{bad}: {err}");

        let err = Registry::update(&path, |r| {
            r.with_entry(&id("first"), |e| RepoEntry {
                interval: Some(bad.to_owned()),
                ..e.clone()
            })
        })
        .expect_err("edit refused");
        assert!(err.to_string().contains("first"), "{bad}: {err}");

        let saved = Registry::load(&path).expect("registry still loads");
        assert_eq!(saved.repos.len(), 1);
        assert_eq!(
            saved.get(&id("first")).and_then(|e| e.interval.clone()),
            None
        );
    }
}

#[test]
fn app_settings_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("registry.toml");
    let repo = entry("obaidoc", &mkdir(tmp.path().join("obaidoc")));
    let plain = Registry::default().with_added(repo).expect("added");
    assert_eq!(plain.app, AppSettings::default());
    assert_eq!(plain.app.terminal_app(), "Terminal");

    plain.save(&path).expect("save");
    let text = fs::read_to_string(&path).expect("read");
    assert!(
        !text.contains("[app]"),
        "no [app] table by default:\n{text}"
    );
    assert_eq!(Registry::load(&path).expect("load"), plain);

    let zed = Registry {
        app: AppSettings {
            editor: Some("Zed".to_owned()),
            ..AppSettings::default()
        },
        ..plain.clone()
    };
    zed.save(&path).expect("save");
    let text = fs::read_to_string(&path).expect("read");
    assert!(
        text.contains("[app]") && text.contains("editor = \"Zed\""),
        "{text}"
    );
    assert!(
        !text.contains("terminal"),
        "unset terminal is omitted:\n{text}"
    );
    let loaded = Registry::load(&path).expect("load");
    assert_eq!(loaded, zed);
    assert_eq!(loaded.app.terminal_app(), "Terminal");

    let iterm = AppSettings {
        terminal: Some("iTerm".to_owned()),
        ..AppSettings::default()
    };
    assert_eq!(iterm.terminal_app(), "iTerm");

    // Every editor keeps the app settings.
    let edited = loaded
        .with_entry(&id("obaidoc"), |e| RepoEntry {
            paused_at: Some("2026-11-01T10:00:00Z".to_owned()),
            ..e.clone()
        })
        .expect("edited");
    assert_eq!(edited.app, zed.app);
    assert_eq!(
        edited.without(&id("obaidoc")).expect("removed").app,
        zed.app
    );
}
