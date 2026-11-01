//! The shell's background threads: the poller, the notifier and one
//! thread per menu action. None of them touches AppKit; results reach the
//! main thread through [`send`].

use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::UNIX_EPOCH;

use super::actions::{Action, perform};
use super::notify;
use super::poll::poll;
use super::shell::{Config, Event, send};
use crate::episode::Notice;
use crate::heartbeat::POLL_INTERVAL;
use crate::launchd::Launchctl;
use crate::paths::RepoId;
use crate::registry::{AppSettings, Registry};
use crate::system::awake_since;

/// Performs `action` off the main thread, posts what it produced, then
/// wakes the poller so the menu shows the result.
pub(super) fn run_action(config: &Config, app: &AppSettings, action: &Action, wake: &Sender<()>) {
    let launchd = Launchctl::current();
    let id = action_repo(action);
    match perform(&config.ctx(&launchd), app, action) {
        Ok(Some(notice)) => post_notice(&id, &notice),
        Ok(None) => {}
        Err(err) => post_notice(
            &id,
            &Notice::Alert {
                label: err.to_string(),
            },
        ),
    }
    if matches!(action, Action::SyncNow(_)) {
        send(Event::Synced(id));
    }
    let _ = wake.send(());
}

fn action_repo(action: &Action) -> RepoId {
    match action {
        Action::SyncNow(id)
        | Action::OpenLog(id)
        | Action::OpenTerminal(id)
        | Action::OpenEditor(id)
        | Action::Restart(id)
        | Action::Pause(id)
        | Action::Resume(id)
        | Action::AllowHooks(id) => id.clone(),
    }
}

/// The notifier thread's queue; set once by [`spawn_notifier`].
static NOTIFIER: OnceLock<Sender<(RepoId, Notice)>> = OnceLock::new();

/// Queues `notice` for `id` without waiting: the notifier thread posts it
/// (title "rsrsbar · <id>", the label or "recovered" as the body), so a
/// slow notification center never delays a poll or its heartbeat.
pub(super) fn post_notice(id: &RepoId, notice: &Notice) {
    let queued = NOTIFIER
        .get()
        .is_some_and(|queue| queue.send((id.clone(), notice.clone())).is_ok());
    if !queued {
        eprintln!("rsrsbar: notifier not running; dropped a notification for {id}");
    }
}

/// Posts queued notices one by one, in order, for the life of the process.
pub(super) fn spawn_notifier() {
    let (queue, notices) = mpsc::channel::<(RepoId, Notice)>();
    if NOTIFIER.set(queue).is_err() {
        return;
    }
    thread::spawn(move || {
        for (id, notice) in notices {
            let body = match &notice {
                Notice::Alert { label } => label.as_str(),
                Notice::Recovered => "recovered",
            };
            if let Err(err) = notify::post(id.as_str(), &format!("rsrsbar · {id}"), body) {
                eprintln!("rsrsbar: cannot post a notification for {id}: {err}");
            }
        }
    });
}

/// Polls forever: authorization, one poll, its notices queued, the outcome
/// sent to the main thread; then waits [`POLL_INTERVAL`] or until woken.
pub(super) fn spawn_poller(config: Config, wake: Receiver<()>) {
    thread::spawn(move || {
        notify::request_authorization();
        loop {
            let notifications = notify::authorization();
            let launchd = Launchctl::current();
            let awake = awake_since().unwrap_or_else(|err| {
                eprintln!("rsrsbar: wake time unknown ({err}); no wake grace");
                UNIX_EPOCH
            });
            let outcome = poll(&config.ctx(&launchd), awake, notifications);
            outcome
                .warnings
                .iter()
                .for_each(|w| eprintln!("rsrsbar: {w}"));
            outcome
                .notices
                .iter()
                .for_each(|(id, n)| post_notice(id, n));
            let app = Registry::load(&config.paths.registry())
                .map_or_else(|_| AppSettings::default(), |r| r.app);
            send(Event::Polled {
                outcome,
                notifications,
                app,
            });
            if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(POLL_INTERVAL) {
                return;
            }
            while wake.try_recv().is_ok() {}
        }
    });
}
