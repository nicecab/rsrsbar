//! The `rsrsbar` command line: a thin `clap` front end over
//! `rsrsbar::commands`. The only place that reads `$HOME`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use rsrsbar::commands::{self, AddArgs, CommandError, Ctx, Report};
use rsrsbar::{GIT_RSRS, Launchctl, Paths};

/// Exit code for failures other than usage errors (clap exits 2 for those).
const FAILURE: u8 = 1;

#[derive(Parser)]
#[command(version, about = "Manage git-rsrs autosync watchers on macOS")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register a checkout and start its watcher.
    Add {
        path: PathBuf,
        /// Repository id (default: derived from the directory name).
        #[arg(long)]
        id: Option<String>,
        /// Branch to sync (default: the branch checked out).
        #[arg(long)]
        branch: Option<String>,
        /// Remote to sync with (default: origin).
        #[arg(long)]
        remote: Option<String>,
        /// git-rsrs lock name (default: git-rsrs's own).
        #[arg(long)]
        lock_name: Option<String>,
        /// Periodic sync interval, e.g. 10m.
        #[arg(long)]
        interval: Option<String>,
    },
    /// Stop a watcher and unregister the repository.
    Remove { id: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(report) => {
            if !report.text.is_empty() {
                println!("{}", report.text);
            }
            ExitCode::from(u8::try_from(report.exit_code).unwrap_or(FAILURE))
        }
        Err(err) => {
            eprintln!("rsrsbar: {err}");
            ExitCode::from(FAILURE)
        }
    }
}

fn run(command: Command) -> Result<Report, CommandError> {
    let launchd = Launchctl::current();
    let ctx = context(&launchd)?;
    match command {
        Command::Add {
            path,
            id,
            branch,
            remote,
            lock_name,
            interval,
        } => commands::add(
            &ctx,
            AddArgs {
                path,
                id,
                branch,
                remote,
                lock_name,
                interval,
            },
        ),
        Command::Remove { id } => commands::remove(&ctx, &id),
    }
}

fn context(launchd: &Launchctl) -> Result<Ctx<'_>, CommandError> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| CommandError::Refused("HOME is not set".to_owned()))?;
    let exe = std::env::current_exe().and_then(|exe| exe.canonicalize());
    let rsrsbar_exe = exe.map_err(|source| CommandError::Io {
        path: PathBuf::from("current executable"),
        source,
    })?;
    Ok(Ctx {
        paths: Paths::new(PathBuf::from(home)),
        launchd,
        git_rsrs: PathBuf::from(GIT_RSRS),
        rsrsbar_exe,
        now: SystemTime::now(),
    })
}
