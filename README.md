# rsrsbar

rsrsbar is a macOS menu bar manager for git-rsrs autosync watchers: it registers
repositories, generates each one's git-rsrs configuration and LaunchAgent,
gates repository-provided `.autosync/` hooks behind explicit trust, and turns
git-rsrs status files into a per-repository health signal with notifications.
This crate currently holds the pure, unit-tested core library; the CLI and the
menu bar app build on it.
