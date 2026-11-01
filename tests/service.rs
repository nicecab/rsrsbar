use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use git_rsrs::config::Config;
use plist::Value;
use rsrsbar::{GIT_RSRS, Paths, Phase, RepoEntry, RepoId, git_rsrs_config, launch_agent_plist};

const EXE: &str = "/Applications/rsrsbar.app/Contents/MacOS/rsrsbar";

fn entry(id: &str, path: &str, branch: &str, lock_name: Option<&str>) -> RepoEntry {
    RepoEntry {
        id: RepoId::new(id).expect("valid id"),
        path: PathBuf::from(path),
        branch: branch.to_owned(),
        remote: "origin".to_owned(),
        lock_name: lock_name.map(str::to_owned),
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

fn config_text(entry: &RepoEntry, paths: &Paths) -> String {
    let settings = entry.settings().expect("settings");
    git_rsrs_config(entry, &settings, paths, Path::new(EXE)).expect("config")
}

fn plist_dict(entry: &RepoEntry, paths: &Paths) -> plist::Dictionary {
    let bytes = launch_agent_plist(entry, paths, Path::new(GIT_RSRS)).expect("plist");
    Value::from_reader(std::io::Cursor::new(bytes))
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
fn config_is_accepted_by_git_rsrs() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let obaidoc = entry(
        "obaidoc",
        "/x/obaidoc",
        "main",
        Some("docs-maintenance.lock"),
    );
    let config = Config::parse(&config_text(&obaidoc, &paths)).expect("git-rsrs accepts");

    assert_eq!(config.branch, "main");
    assert_eq!(config.remote, "origin");
    assert_eq!(config.interval, Duration::from_secs(600));
    assert_eq!(config.quiet_period, Duration::from_secs(30));
    assert_eq!(config.git_timeout, Duration::from_secs(120));
    assert_eq!(config.lock_timeout, Duration::from_secs(30));
    assert_eq!(config.lock_name, "docs-maintenance.lock");
    assert_eq!(
        config.status_file,
        Some(PathBuf::from(
            "/Users/t/Library/Application Support/rsrsbar/repos/obaidoc/status.json"
        ))
    );
    let hooks = &config.hooks;
    assert_eq!(hooks.pre_commit.len(), 1);
    assert_eq!(hooks.post_integrate.len(), 1);
    assert_eq!(hooks.on_result.len(), 1);
    assert_eq!(
        hooks.pre_commit[0].command,
        [EXE, "run-hook", "obaidoc", "pre-commit"]
    );
    assert_eq!(
        hooks.post_integrate[0].command,
        [EXE, "run-hook", "obaidoc", "post-integrate"]
    );
    assert_eq!(
        hooks.on_result[0].command,
        [EXE, "run-hook", "obaidoc", "on-result"]
    );
    assert_eq!(hooks.pre_commit[0].reject_exit_codes, [3]);
    assert!(hooks.post_integrate[0].reject_exit_codes.is_empty());
    assert!(hooks.on_result[0].reject_exit_codes.is_empty());
    assert_eq!(hooks.pre_commit[0].timeout, Duration::from_secs(30));
    assert_eq!(hooks.post_integrate[0].timeout, Duration::from_secs(30));
    assert_eq!(hooks.on_result[0].timeout, Duration::from_secs(10));
}

#[test]
fn odd_durations_survive_git_rsrs_parsing() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let odd = RepoEntry {
        interval: Some("90s".to_owned()),
        quiet_period: Some("1m 5s".to_owned()),
        git_timeout: Some("2h".to_owned()),
        lock_timeout: Some("1500ms".to_owned()),
        ..entry("odd", "/x/odd", "main", None)
    };
    let config = Config::parse(&config_text(&odd, &paths)).expect("git-rsrs accepts");
    assert_eq!(config.interval, Duration::from_secs(90));
    assert_eq!(config.quiet_period, Duration::from_secs(65));
    assert_eq!(config.git_timeout, Duration::from_secs(7200));
    assert_eq!(config.lock_timeout, Duration::from_millis(1500));
}

#[test]
fn lock_name_omitted_by_default() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let text = config_text(&entry("hub", "/x/hub", "main", None), &paths);
    assert!(!text.contains("lock_name"), "{text}");
    assert_eq!(
        Config::parse(&text).expect("git-rsrs accepts").lock_name,
        "git-rsrs.lock"
    );
}

#[test]
fn plist_matches_todays_shape() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let dict = plist_dict(&entry("obaidoc", "/x/obaidoc", "main", None), &paths);

    assert_eq!(string(&dict, "Label"), "com.nicecab.rsrsbar.watch.obaidoc");
    assert_eq!(
        dict.get("KeepAlive").and_then(Value::as_boolean),
        Some(true)
    );
    assert_eq!(
        dict.get("RunAtLoad").and_then(Value::as_boolean),
        Some(true)
    );
    assert_eq!(
        dict.get("ThrottleInterval")
            .and_then(Value::as_signed_integer),
        Some(30)
    );
    assert_eq!(string(&dict, "WorkingDirectory"), "/x/obaidoc");
    let args: Vec<&str> = dict
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .expect("ProgramArguments")
        .iter()
        .map(|v| v.as_string().expect("string argument"))
        .collect();
    assert_eq!(
        args,
        [
            "/opt/homebrew/bin/git-rsrs",
            "--repo",
            "/x/obaidoc",
            "--config",
            "/Users/t/Library/Application Support/rsrsbar/repos/obaidoc/git-rsrs.toml",
            "watch",
        ]
    );
    let env = dict
        .get("EnvironmentVariables")
        .and_then(Value::as_dictionary)
        .expect("EnvironmentVariables");
    assert_eq!(
        string(env, "PATH"),
        "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:\
         /Users/t/.local/share/mise/shims"
    );
    assert_eq!(
        string(&dict, "StandardOutPath"),
        "/Users/t/Library/Logs/com.nicecab.rsrsbar.watch.obaidoc.log"
    );
    assert_eq!(
        string(&dict, "StandardErrorPath"),
        "/Users/t/Library/Logs/com.nicecab.rsrsbar.watch.obaidoc.error.log"
    );
}

#[test]
fn awkward_strings_round_trip() {
    let paths = Paths::new(PathBuf::from("/Users/t q"));
    let repo = "/x/日本語 repo";
    let branch = "feat/\"quoted\"";
    let awkward = entry("awkward", repo, branch, None);

    let config = Config::parse(&config_text(&awkward, &paths)).expect("git-rsrs accepts");
    assert_eq!(config.branch, branch);
    assert_eq!(
        config.status_file,
        Some(PathBuf::from(
            "/Users/t q/Library/Application Support/rsrsbar/repos/awkward/status.json"
        ))
    );

    let dict = plist_dict(&awkward, &paths);
    assert_eq!(string(&dict, "WorkingDirectory"), repo);
    let args = dict
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .expect("ProgramArguments");
    assert_eq!(args[2].as_string(), Some(repo));
    assert_eq!(
        args[4].as_string(),
        Some("/Users/t q/Library/Application Support/rsrsbar/repos/awkward/git-rsrs.toml")
    );
}

#[test]
fn non_utf8_paths_are_errors() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let bad = PathBuf::from(OsStr::from_bytes(b"/x/bad\xff"));
    let broken = RepoEntry {
        path: bad,
        ..entry("bad", "/unused", "main", None)
    };
    assert!(launch_agent_plist(&broken, &paths, Path::new(GIT_RSRS)).is_err());

    let bad_home = Paths::new(PathBuf::from(OsStr::from_bytes(b"/Users/\xff")));
    let fine = entry("fine", "/x/fine", "main", None);
    let settings = fine.settings().expect("settings");
    assert!(git_rsrs_config(&fine, &settings, &bad_home, Path::new(EXE)).is_err());
    assert!(launch_agent_plist(&fine, &bad_home, Path::new(GIT_RSRS)).is_err());
}

#[test]
fn registry_duration_bounds_match_git_rsrs() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let year = Duration::from_secs(365 * 24 * 3600);
    let at_limit = RepoEntry {
        interval: Some("365days".to_owned()),
        quiet_period: Some("365days".to_owned()),
        git_timeout: Some("365days".to_owned()),
        lock_timeout: Some("365days".to_owned()),
        ..entry("limit", "/x/limit", "main", None)
    };
    let settings = at_limit.settings().expect("one year is allowed");
    let text = git_rsrs_config(&at_limit, &settings, &paths, Path::new(EXE)).expect("config");
    let config = Config::parse(&text).expect("git-rsrs accepts one year");
    assert_eq!(config.interval, year);

    for beyond in [Duration::ZERO, year + Duration::from_secs(1)] {
        let forced = rsrsbar::Settings {
            interval: beyond,
            ..settings.clone()
        };
        let text = git_rsrs_config(&at_limit, &forced, &paths, Path::new(EXE)).expect("config");
        assert!(
            Config::parse(&text).is_err(),
            "git-rsrs rejects {beyond:?}, so the registry must too"
        );
    }
}

#[test]
fn hook_tables_use_phase_config_keys() {
    let paths = Paths::new(PathBuf::from("/Users/t"));
    let text = config_text(&entry("keys", "/x/keys", "main", None), &paths);
    for phase in Phase::ALL {
        let table = format!("[[hooks.{}]]", phase.config_key());
        assert_eq!(text.matches(&table).count(), 1, "{table} in:\n{text}");
    }
}
