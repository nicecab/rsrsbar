//! The AppKit shell: owns the main thread, renders [`super::menu::MenuModel`]s into the
//! `NSStatusItem` and its `NSMenu`, and dispatches menu actions. Every
//! decision lives in [`super::poll`], [`super::menu`] and
//! [`super::actions`]; this module only renders and forwards.
//!
//! Threads: the main thread runs AppKit. A poller thread polls every
//! [`crate::heartbeat::POLL_INTERVAL`] (or when woken after an action); a
//! notifier thread posts every notice, so a slow notification center never
//! delays a poll; actions other than "Allow hooks…" run on their own
//! threads. Results reach the main thread as events on the main dispatch
//! queue.

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::SystemTime;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::NSObject;

use super::Authorization;
use super::actions::Action;
use super::menu::build;
use super::notify::{self, NotificationDelegate};
use super::poll::{PollOutcome, acquire_instance};
use super::shell_allow::allow_hooks;
use super::shell_render::{GENERATIONS, ns_menu, render_icon, untag};
use super::shell_workers::{run_action, spawn_notifier, spawn_poller};
use crate::commands::Ctx;
use crate::launchd::Launchctl;
use crate::paths::{Paths, RepoId};
use crate::registry::AppSettings;

/// What the background threads tell the main thread.
pub(super) enum Event {
    Polled {
        outcome: PollOutcome,
        notifications: Authorization,
        app: AppSettings,
    },
    /// A Sync now finished; its notice was already posted.
    Synced(RepoId),
    /// A notification for this repository was clicked.
    Clicked(String),
}

/// Everything a command needs except launchd and the time.
#[derive(Clone, Debug)]
pub(super) struct Config {
    pub(super) paths: Paths,
    pub(super) git_rsrs: PathBuf,
    pub(super) rsrsbar_exe: PathBuf,
}

impl Config {
    pub(super) fn ctx<'a>(&self, launchd: &'a Launchctl) -> Ctx<'a> {
        Ctx {
            paths: self.paths.clone(),
            launchd,
            git_rsrs: self.git_rsrs.clone(),
            rsrsbar_exe: self.rsrsbar_exe.clone(),
            now: SystemTime::now(),
        }
    }
}

#[derive(Default)]
struct View {
    outcome: PollOutcome,
    notifications: Option<Authorization>,
    app: AppSettings,
    busy: BTreeSet<RepoId>,
    first: Option<RepoId>,
    /// The actions behind the current menu's item tags.
    actions: Vec<Action>,
    /// Bumped on every render; stale menus' clicks are ignored.
    generation: isize,
    menu_open: bool,
    /// A notification was clicked while the menu was open: reopen it, the
    /// clicked repository first, once it has closed.
    reopen: bool,
}

struct Ivars {
    config: Config,
    wake: Sender<()>,
    status_item: OnceCell<Retained<NSStatusItem>>,
    notification_delegate: OnceCell<Retained<NotificationDelegate>>,
    view: RefCell<View>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; `Shell` does not
    // implement `Drop`; it is only used on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    #[name = "RsrsbarShell"]
    struct Shell;

    impl Shell {
        /// The target of every action item.
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let (generation, index) = untag(sender.tag());
            let action = {
                let view = self.ivars().view.borrow();
                let current = view.generation == generation;
                view.actions.get(index).filter(|_| current).cloned()
            };
            match action {
                Some(action) => self.dispatch(action),
                None => eprintln!("rsrsbar: ignoring a click on an outdated menu"),
            }
        }
    }

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for Shell {}

    // SAFETY: the method signatures match the protocol's.
    unsafe impl NSMenuDelegate for Shell {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.ivars().view.borrow_mut().menu_open = true;
        }

        /// The clicked repository stays on top only until the menu closes,
        /// unless a click is waiting to reopen it. Re-rendered on the next
        /// turn of the main queue, after the click that closed the menu has
        /// been handled.
        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            let reopen = {
                let mut view = self.ivars().view.borrow_mut();
                view.menu_open = false;
                let reopen = std::mem::take(&mut view.reopen);
                if !reopen {
                    view.first = None;
                }
                reopen
            };
            DispatchQueue::main().exec_async(move || {
                with_shell(|shell| {
                    shell.render();
                    if reopen {
                        shell.open_menu();
                    }
                });
            });
        }
    }
);

thread_local! {
    /// The one shell, reachable from blocks run on the main queue.
    static SHELL: OnceCell<Retained<Shell>> = const { OnceCell::new() };
}

fn with_shell(f: impl FnOnce(&Shell)) {
    SHELL.with(|cell| {
        if let Some(shell) = cell.get() {
            f(shell);
        }
    });
}

/// Hands `event` to the shell on the main thread.
pub(super) fn send(event: Event) {
    DispatchQueue::main().exec_async(move || with_shell(|shell| shell.handle(event)));
}

impl Shell {
    fn new(mtm: MainThreadMarker, config: Config, wake: Sender<()>) -> Retained<Shell> {
        let this = Shell::alloc(mtm).set_ivars(Ivars {
            config,
            wake,
            status_item: OnceCell::new(),
            notification_delegate: OnceCell::new(),
            view: RefCell::new(View::default()),
        });
        // SAFETY: `NSObject`'s `init` signature is correct.
        unsafe { msg_send![super(this), init] }
    }

    fn install(&self) {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let _ = self.ivars().status_item.set(item);
        match notify::install_delegate(|repo| send(Event::Clicked(repo.to_owned()))) {
            Some(delegate) => drop(self.ivars().notification_delegate.set(delegate)),
            None => eprintln!("rsrsbar: not running from an app bundle; no notifications"),
        }
        self.render();
    }

    fn handle(&self, event: Event) {
        match event {
            Event::Clicked(repo) => return self.clicked(&repo),
            Event::Polled {
                outcome,
                notifications,
                app,
            } => {
                let mut view = self.ivars().view.borrow_mut();
                view.outcome = outcome;
                view.notifications = Some(notifications);
                view.app = app;
            }
            Event::Synced(id) => drop(self.ivars().view.borrow_mut().busy.remove(&id)),
        }
        self.render();
    }

    /// A notification click shows the menu with the clicked repository
    /// first (AppKit cannot open one submenu programmatically). An open
    /// menu cannot be rebuilt, so it is closed and reopened from
    /// `menuDidClose:`.
    fn clicked(&self, repo: &str) {
        let menu_open = {
            let mut view = self.ivars().view.borrow_mut();
            view.first = RepoId::new(repo).ok();
            view.reopen = view.menu_open;
            view.menu_open
        };
        if menu_open {
            let menu = self
                .ivars()
                .status_item
                .get()
                .and_then(|item| item.menu(self.mtm()));
            if let Some(menu) = menu {
                menu.cancelTracking();
            }
            return;
        }
        self.render();
        self.open_menu();
    }

    fn open_menu(&self) {
        if let Some(button) = self.button() {
            // SAFETY: `performClick:` accepts a nil sender.
            unsafe { button.performClick(None) };
        }
    }

    fn button(&self) -> Option<Retained<objc2_app_kit::NSStatusBarButton>> {
        self.ivars().status_item.get()?.button(self.mtm())
    }

    /// Rebuilds the icon and the menu, unless the menu is open (it is
    /// rebuilt when it closes).
    fn render(&self) {
        let mtm = self.mtm();
        let Some(status_item) = self.ivars().status_item.get() else {
            return;
        };
        let mut view = self.ivars().view.borrow_mut();
        if view.menu_open {
            return;
        }
        // Until the first poll, notifications count as authorized: no
        // warning flashes up before the answer is known.
        let notifications = view.notifications.unwrap_or(Authorization::Authorized);
        let model = build(
            &view.outcome,
            &view.app,
            notifications,
            &view.busy,
            view.first.as_ref(),
            SystemTime::now(),
        );
        view.generation = (view.generation + 1) % GENERATIONS;
        let generation = view.generation;
        let mut actions = Vec::new();
        let menu = ns_menu(&model.items, self.as_ref(), generation, &mut actions, mtm);
        view.actions = actions;
        drop(view);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        status_item.setMenu(Some(&menu));
        if let Some(button) = self.button() {
            render_icon(&button, model.icon);
        }
    }

    fn dispatch(&self, action: Action) {
        if let Action::AllowHooks(id) = &action {
            allow_hooks(&self.ivars().config, id, self.mtm());
            let _ = self.ivars().wake.send(());
            return;
        }
        if let Action::SyncNow(id) = &action {
            let inserted = self.ivars().view.borrow_mut().busy.insert(id.clone());
            if !inserted {
                // A queued second click on the same Sync now: one is running.
                return;
            }
            self.render();
        }
        let config = self.ivars().config.clone();
        let app = self.ivars().view.borrow().app.clone();
        let wake = self.ivars().wake.clone();
        thread::spawn(move || run_action(&config, &app, &action, &wake));
    }
}

/// Runs the menu bar app until the process ends. A second instance prints
/// "rsrsbar is already running" and exits 0 at once, before any icon or
/// notification.
pub fn run_app(paths: Paths, git_rsrs: PathBuf, rsrsbar_exe: PathBuf) -> ! {
    let lock = match acquire_instance(&paths) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            println!("rsrsbar is already running");
            std::process::exit(0);
        }
        Err(err) => fail(&format!("cannot take the instance lock: {err}")),
    };
    let Some(mtm) = MainThreadMarker::new() else {
        fail("the app must run on the main thread");
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let config = Config {
        paths,
        git_rsrs,
        rsrsbar_exe,
    };
    let (wake, woken) = mpsc::channel();
    let shell = Shell::new(mtm, config.clone(), wake);
    shell.install();
    SHELL.with(|cell| drop(cell.set(shell)));
    spawn_notifier();
    spawn_poller(config, woken);
    app.run();
    drop(lock);
    std::process::exit(0)
}

fn fail(message: &str) -> ! {
    eprintln!("rsrsbar: {message}");
    std::process::exit(1)
}
