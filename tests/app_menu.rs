mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use common::{Fixture, add_args, id, now};
use rsrsbar::app::actions::{Action, perform};
use rsrsbar::app::menu::{IconState, Item, MenuModel, build};
use rsrsbar::app::poll::{PollOutcome, RepoView};
use rsrsbar::commands::add;
use rsrsbar::{
    AppSettings, Authorization, Color, Health, JobState, Notice, Notify, Outcome, Phase,
    RecentAttempt, Recording, RepoEntry, RepoId, RepoSnapshot, Row, StatusFile, StatusRead,
};

const MIN: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);

fn entry(name: &str) -> RepoEntry {
    RepoEntry {
        id: id(name),
        path: PathBuf::from(format!("/x/{name}")),
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

fn status(outcome: Outcome, succeeded_ago: Duration) -> StatusFile {
    StatusFile {
        version: 1,
        last_attempt_at: now(),
        last_outcome: outcome,
        last_success_at: Some(now() - succeeded_ago),
        message: None,
        pid: 42,
        git_rsrs_version: "0.1.2".to_owned(),
        hook_failures: Vec::new(),
        counters_since: "2026-11-01T00:00:00Z".to_owned(),
        attempt_seq: 3,
        consecutive_failures: 0,
        consecutive_successes: 3,
        recent: vec![RecentAttempt {
            seq: 3,
            healthy: true,
        }],
    }
}

fn health(row: Row, color: Color, label: Option<&str>) -> Health {
    Health {
        row,
        color,
        notify: Notify::Never,
        label: label.map(str::to_owned),
        phase: None,
    }
}

fn view(entry: RepoEntry, health: Health, status: StatusFile) -> RepoView {
    let settings = entry.settings().expect("settings");
    RepoView {
        snapshot: Some(RepoSnapshot {
            settings,
            paused_at: None,
            hooks_trusted: true,
            job: JobState::Running { up_for: HOUR },
            status: StatusRead::Present(status),
            current_branch: None,
        }),
        entry,
        health,
        error: None,
        hooks: Vec::new(),
    }
}

fn green(name: &str) -> RepoView {
    view(
        entry(name),
        health(Row::Healthy, Color::Green, None),
        status(Outcome::Synced, MIN),
    )
}

fn stalled(name: &str) -> RepoView {
    view(
        entry(name),
        health(Row::Stalled, Color::Red, Some("sync stalled")),
        status(Outcome::Deferred, 31 * MIN),
    )
}

fn outcome(repos: Vec<RepoView>) -> PollOutcome {
    PollOutcome {
        repos,
        ..PollOutcome::default()
    }
}

fn menu(outcome: &PollOutcome) -> MenuModel {
    build(
        outcome,
        &AppSettings::default(),
        Authorization::Authorized,
        &BTreeSet::new(),
        None,
        now(),
    )
}

fn submenu<'a>(model: &'a MenuModel, id: &str) -> &'a [Item] {
    model
        .items
        .iter()
        .find_map(|item| match item {
            Item::Submenu { title, items, .. } if title.starts_with(&format!("{id} — ")) => {
                Some(items.as_slice())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no submenu for {id}: {:?}", model.items))
}

fn label(text: &str) -> Item {
    Item::Label(text.to_owned())
}

fn action(title: &str, action: Action) -> Item {
    Item::Action {
        title: title.to_owned(),
        action,
        enabled: true,
    }
}

fn titles(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .flat_map(|item| match item {
            Item::Label(title) | Item::Action { title, .. } => vec![title.clone()],
            Item::Submenu { title, items, .. } => std::iter::once(title.clone())
                .chain(titles(items))
                .collect(),
            Item::Separator => Vec::new(),
        })
        .collect()
}

#[test]
fn icon_counts_red_and_warns() {
    let polled = outcome(vec![stalled("alpha"), green("beta"), stalled("gamma")]);
    let model = menu(&polled);
    assert_eq!(
        model.icon,
        IconState {
            red: 2,
            warning: false
        }
    );
    assert!(
        !titles(&model.items)
            .iter()
            .any(|t| t.starts_with("Notifications are off"))
    );

    for off in [
        Authorization::Denied,
        Authorization::NotDetermined,
        Authorization::Unknown,
    ] {
        let model = build(
            &polled,
            &AppSettings::default(),
            off,
            &BTreeSet::new(),
            None,
            now(),
        );
        assert_eq!(
            model.icon,
            IconState {
                red: 2,
                warning: true
            }
        );
        assert_eq!(
            model.items.first(),
            Some(&label("Notifications are off — alerts cannot reach you"))
        );
    }

    let calm = menu(&outcome(vec![green("beta")]));
    assert_eq!(
        calm.icon,
        IconState {
            red: 0,
            warning: false
        }
    );
}

#[test]
fn no_quit_item() {
    let polled = outcome(vec![stalled("alpha"), green("beta")]);
    for notifications in [Authorization::Authorized, Authorization::Denied] {
        let model = build(
            &polled,
            &AppSettings::default(),
            notifications,
            &BTreeSet::new(),
            None,
            now(),
        );
        let all = titles(&model.items);
        assert!(!all.is_empty());
        assert!(
            !all.iter().any(|t| t.to_lowercase().contains("quit")),
            "{all:?}"
        );
    }
    let empty = menu(&outcome(Vec::new()));
    assert!(
        !titles(&empty.items)
            .iter()
            .any(|t| t.to_lowercase().contains("quit"))
    );
}

#[test]
fn submenu_detail_and_actions() {
    let beta_status = StatusFile {
        last_outcome: Outcome::Failed,
        consecutive_failures: 2,
        message: Some(format!("fetch failed: {}\nsecond line", "x".repeat(100))),
        ..status(Outcome::Failed, 2 * HOUR)
    };
    let paused = RepoView {
        hooks: vec![Phase::PreCommit, Phase::OnResult],
        ..view(
            RepoEntry {
                paused_at: Some("2026-11-01T10:00:00Z".to_owned()),
                ..entry("beta")
            },
            health(Row::Paused, Color::Grey, None),
            beta_status,
        )
    };
    let model = menu(&outcome(vec![stalled("alpha"), paused]));

    let alpha = id("alpha");
    assert!(matches!(
        &model.items[0],
        Item::Submenu { title, color: Color::Red, .. } if title == "alpha — sync stalled"
    ));
    assert_eq!(
        submenu(&model, "alpha"),
        [
            label("last success 31 min ago"),
            label("latest deferred"),
            label("/x/alpha"),
            label("main → origin"),
            label("hooks: none"),
            Item::Separator,
            action("Sync now", Action::SyncNow(alpha.clone())),
            action("Open log", Action::OpenLog(alpha.clone())),
            action("Open in Terminal", Action::OpenTerminal(alpha.clone())),
            action("Restart watcher", Action::Restart(alpha.clone())),
            action("Pause", Action::Pause(alpha)),
        ]
    );

    let beta = id("beta");
    assert!(matches!(
        &model.items[1],
        Item::Submenu { title, color: Color::Grey, .. } if title == "beta — paused"
    ));
    // The message's first line, cut to 80 characters including the ellipsis.
    let message = format!("message: fetch failed: {}…", "x".repeat(65));
    assert_eq!(message.chars().count(), "message: ".len() + 80);
    assert_eq!(
        submenu(&model, "beta"),
        [
            label("last success 2 h ago"),
            label("latest failed"),
            label("2 failures in a row"),
            label(&message),
            label("/x/beta"),
            label("main → origin"),
            label("hooks: pre-commit, on-result"),
            Item::Separator,
            action("Sync now", Action::SyncNow(beta.clone())),
            action("Open log", Action::OpenLog(beta.clone())),
            action("Open in Terminal", Action::OpenTerminal(beta.clone())),
            action("Restart watcher", Action::Restart(beta.clone())),
            action("Resume", Action::Resume(beta)),
        ]
    );

    assert_eq!(model.items[2], Item::Separator);
    assert_eq!(
        model.items[3],
        label(&format!("rsrsbar {}", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(model.items.len(), 4);
}

#[test]
fn allow_only_when_hooks_changed() {
    let changed = view(
        entry("alpha"),
        Health {
            notify: Notify::Immediately,
            ..health(
                Row::HooksChanged,
                Color::Red,
                Some("hook changed — needs allow"),
            )
        },
        status(Outcome::CheckFailed, HOUR),
    );
    let model = menu(&outcome(vec![changed, stalled("beta")]));
    let allow = action("Allow hooks…", Action::AllowHooks(id("alpha")));
    assert!(submenu(&model, "alpha").contains(&allow));
    assert!(
        !titles(submenu(&model, "beta"))
            .iter()
            .any(|t| t.starts_with("Allow hooks")),
        "{:?}",
        submenu(&model, "beta")
    );
}

#[test]
fn editor_item_only_when_set() {
    let polled = outcome(vec![green("alpha")]);
    assert!(
        !titles(submenu(&menu(&polled), "alpha"))
            .iter()
            .any(|t| t.starts_with("Open in Zed"))
    );

    let app = AppSettings {
        terminal: Some("iTerm".to_owned()),
        editor: Some("Zed".to_owned()),
    };
    let model = build(
        &polled,
        &app,
        Authorization::Authorized,
        &BTreeSet::new(),
        None,
        now(),
    );
    let items = submenu(&model, "alpha");
    assert!(items.contains(&action("Open in iTerm", Action::OpenTerminal(id("alpha")))));
    assert!(items.contains(&action("Open in Zed", Action::OpenEditor(id("alpha")))));
}

#[test]
fn syncing_disables_item() {
    let polled = outcome(vec![green("alpha"), green("beta")]);
    let busy = BTreeSet::from([id("alpha")]);
    let model = build(
        &polled,
        &AppSettings::default(),
        Authorization::Authorized,
        &busy,
        None,
        now(),
    );
    assert!(submenu(&model, "alpha").contains(&Item::Action {
        title: "Syncing…".to_owned(),
        action: Action::SyncNow(id("alpha")),
        enabled: false,
    }));
    assert!(submenu(&model, "beta").contains(&action("Sync now", Action::SyncNow(id("beta")))));
}

#[test]
fn first_repo_moved_to_top() {
    let polled = outcome(vec![green("alpha"), stalled("beta"), green("gamma")]);
    let first = id("gamma");
    let model = build(
        &polled,
        &AppSettings::default(),
        Authorization::Authorized,
        &BTreeSet::new(),
        Some(&first),
        now(),
    );
    let order: Vec<&str> = model
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Submenu { title, .. } => title.split(" — ").next(),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["gamma", "alpha", "beta"]);

    let unknown = id("ghost");
    let model = build(
        &polled,
        &AppSettings::default(),
        Authorization::Authorized,
        &BTreeSet::new(),
        Some(&unknown),
        now(),
    );
    assert!(matches!(&model.items[0], Item::Submenu { title, .. } if title.starts_with("alpha")));
}

fn sync_fixture(exit_code: i32) -> (Fixture, RepoId) {
    let fx = Fixture::with_git_rsrs(&format!(
        "if [ \"$1\" = --version ]; then echo 'git-rsrs 0.1.2'; exit 0; fi\n\
         echo 'timed_out: fetch took too long'\necho 'second line'\nexit {exit_code}"
    ));
    add(
        &fx.ctx(&Recording::new()),
        add_args(&fx.repo("alpha", true)),
    )
    .expect("add");
    (fx, id("alpha"))
}

#[test]
fn failed_sync_becomes_notice() {
    let (fx, repo) = sync_fixture(7);
    let launchd = Recording::new();
    let notice = perform(
        &fx.ctx(&launchd),
        &AppSettings::default(),
        &Action::SyncNow(repo),
    )
    .expect("a failed sync is a notice, not an error");
    match notice {
        Some(Notice::Alert { label }) => {
            assert!(label.contains("timed_out: fetch took too long"), "{label}");
            assert!(!label.contains("second line"), "{label}");
        }
        other => panic!("expected an alert, got {other:?}"),
    }

    let (fx, repo) = sync_fixture(0);
    let notice = perform(
        &fx.ctx(&launchd),
        &AppSettings::default(),
        &Action::SyncNow(repo),
    )
    .expect("sync runs");
    assert_eq!(notice, None);

    // AllowHooks needs the dialog in the shell; perform refuses it.
    let (fx, repo) = sync_fixture(0);
    assert!(
        perform(
            &fx.ctx(&launchd),
            &AppSettings::default(),
            &Action::AllowHooks(repo)
        )
        .is_err()
    );
}
