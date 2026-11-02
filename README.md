# rsrsbar

rsrsbar is a macOS menu bar manager for git-rsrs autosync watchers: it registers
repositories, generates each one's git-rsrs configuration and LaunchAgent,
gates repository-provided `.autosync/` hooks behind explicit trust, and turns
git-rsrs status files into a per-repository health signal with notifications.
The `rsrsbar` command line tool and the menu bar app ship together in one app
bundle, `rsrsbar.app`.

## Requirements

- macOS 15 (Sequoia) or later, on Apple Silicon.
- [git-rsrs](https://github.com/nicecab/git-rsrs) 0.1.2 or later. The cask
  installs it from the same tap.

## Install

```sh
brew install --cask nicecab/tap/rsrsbar
rsrsbar start
```

`rsrsbar start` loads the menu bar app and its heartbeat job as LaunchAgents.
Allow notifications when macOS asks: they are how rsrsbar reports a failing
repository.

### Unsigned app

`rsrsbar.app` is ad-hoc signed, not signed with a Developer ID or notarized.
Homebrew quarantines everything a cask downloads, and Gatekeeper can refuse to
run the quarantined ad-hoc-signed binary (in our test it showed a dialog and
killed the process). The cask's postflight therefore removes the
`com.apple.quarantine` attribute from the installed app. Install it only from
the `nicecab/tap` tap.

## Everyday use

```sh
rsrsbar add <path>      # register a checkout and start its watcher
rsrsbar allow <id>      # review the .autosync/ hook changes and trust them
rsrsbar status          # every repository's health; exits 1 if any is red
rsrsbar pause <id>      # stop a watcher until resume
rsrsbar resume <id>     # start a paused watcher again
rsrsbar remove <id>     # stop a watcher and unregister the repository
rsrsbar stop            # unload the menu bar app and its heartbeat job
```

`add` derives the id from the directory name unless `--id` is given, and takes
`--branch`, `--remote`, `--lock-name`, `--interval` (for example `10m`) and
`--allow-hooks`. Hooks under `.autosync/` run only once allowed; a changed hook
turns the repository red until `rsrsbar allow` trusts it again. `rsrsbar
--help` lists every command, including `refresh`, `restart` and `sync`;
`rsrsbar <command> --help` describes one command's arguments and options.

Watchers keep syncing while the menu bar app is stopped; `stop` only removes
the supervision.

### What `rsrsbar status` assumes

No notification and no red icon means healthy only when three things hold.
`rsrsbar status` ends with one line for each:

- `heartbeat job: loaded` — the job that checks every 15 minutes that the app
  is alive, restarts it if it is stuck, and raises its own alert if that does
  not help.
- `notifications: authorized` — as the running app last reported it. `denied`
  or `not determined` means alerts cannot reach you; `unknown (…)` means the app
  is not running or has not reported recently.
- `focus: off` — a Focus mode can hold notifications back; `on` says one is
  active, `unknown` that it could not be read.

## Upgrade

```sh
brew upgrade --cask rsrsbar
```

A running app is restarted on the new version; an app stopped with
`rsrsbar stop` stays stopped. Each release carries a new ad-hoc signature. In
our test macOS kept the notification permission across the upgrade, but it may
ask again: allow it if asked. The app shows a warning while notifications are
not authorized.

## Uninstall

First run `rsrsbar remove <id>` for every registered repository. A watcher left
behind keeps running and calls its hook runner inside `rsrsbar.app`, which no
longer exists, so its hooks fail and nothing is left to report it.

```sh
rsrsbar remove <id>
brew uninstall --cask rsrsbar
```

Add `--zap` to also delete rsrsbar's settings, caches and logs. Uninstalling
while the app is running leaves a small restart marker in
`~/Library/Caches/com.nicecab.rsrsbar`, so a later reinstall starts the app
once; `--zap` removes it.

## Build from source

```sh
scripts/bundle.sh --install     # build and install ~/Applications/rsrsbar.app
scripts/bundle.sh --zip dist    # build a release-style zip and its .sha256 in dist/
```

Both build `target/rsrsbar.app` first. `start` must run from inside the
bundle it loads, so after `--install` run
`~/Applications/rsrsbar.app/Contents/MacOS/rsrsbar start`.
