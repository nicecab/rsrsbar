//! The `rsrsbar` command line: a thin `clap` front end over
//! `rsrsbar::commands`. The only place that reads `$HOME`.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use rsrsbar::commands::{self, AddArgs, CommandError, Ctx, Report};
use rsrsbar::{GIT_RSRS, Launchctl, Paths};

/// Exit code for a report whose code does not fit a process exit status.
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
        /// Allow the repository's .autosync/ hooks at once, without a prompt.
        #[arg(long)]
        allow_hooks: bool,
    },
    /// Stop a watcher and unregister the repository.
    Remove { id: String },
    /// Stop a watcher until resume.
    Pause { id: String },
    /// Rewrite a paused repository's service files and start its watcher.
    Resume { id: String },
    /// Rewrite service files (all repositories, or one) and restart changed watchers.
    Refresh { id: Option<String> },
    /// Restart a watcher.
    Restart { id: String },
    /// Run one sync now and pass git-rsrs's exit code through (124 on deadline).
    Sync { id: String },
    /// Show the .autosync/ changes since the last allow and trust them.
    Allow {
        id: String,
        /// Allow without asking (required when not on a terminal).
        #[arg(long)]
        yes: bool,
    },
    /// Run one allowed hook phase; called by the generated git-rsrs config.
    #[command(hide = true)]
    RunHook { id: String, phase: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::RunHook { id, phase } = &cli.command {
        return run_hook(id, phase);
    }
    match run(cli.command) {
        Ok(report) => {
            if !report.text.is_empty() {
                println!("{}", report.text);
            }
            ExitCode::from(u8::try_from(report.exit_code).unwrap_or(FAILURE))
        }
        Err(err) => {
            eprintln!("rsrsbar: {}", one_line(&err.to_string()));
            ExitCode::from(err.exit_code())
        }
    }
}

/// `run-hook` prints its refusal bare (git-rsrs shows it as the hook's
/// failure message) on exactly one line, and exits 1 — never 3.
fn run_hook(id: &str, phase: &str) -> ExitCode {
    let result = home().map_err(|err| err.to_string()).and_then(|home| {
        commands::run_hook(&Paths::new(home), id, phase).map_err(|err| err.to_string())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{}", one_line(&message));
            ExitCode::from(FAILURE)
        }
    }
}

/// Error text folded onto one line (parse errors can span several).
fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
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
            allow_hooks,
        } => commands::add(
            &ctx,
            AddArgs {
                path,
                id,
                branch,
                remote,
                lock_name,
                interval,
                allow_hooks,
            },
        ),
        Command::Remove { id } => commands::remove(&ctx, &id),
        Command::Pause { id } => commands::pause(&ctx, &id),
        Command::Resume { id } => commands::resume(&ctx, &id),
        Command::Refresh { id } => commands::refresh(&ctx, id.as_deref()),
        Command::Restart { id } => commands::restart(&ctx, &id),
        Command::Sync { id } => commands::sync(&ctx, &id),
        Command::Allow { id, yes } => allow(&ctx, &id, yes),
        Command::RunHook { .. } => Err(CommandError::Refused(
            "run-hook is handled before the command context".to_owned(),
        )),
    }
}

/// Prints the preview, then asks on a terminal unless `--yes`; without a
/// terminal and without `--yes` nothing is allowed. Either way only the
/// previewed hooks can be allowed.
fn allow(ctx: &Ctx, id: &str, yes: bool) -> Result<Report, CommandError> {
    let preview = commands::allow_preview(ctx, id)?;
    println!("{}", preview.text);
    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CommandError::Refused(format!(
                "not on a terminal; rerun rsrsbar allow {id} --yes to allow these hooks"
            )));
        }
        if !confirmed(id)? {
            return Ok(Report {
                text: format!("nothing allowed for {id}"),
                exit_code: i32::from(FAILURE),
            });
        }
    }
    // Bound to what was shown: `allow` refuses if the hooks changed since.
    commands::allow(ctx, id, &preview.digest)
}

fn confirmed(id: &str) -> Result<bool, CommandError> {
    let stdin_error = |source| CommandError::Io {
        path: PathBuf::from("standard input"),
        source,
    };
    print!("Allow these hooks for {id}? [y/N] ");
    io::stdout().flush().map_err(stdin_error)?;
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(stdin_error)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn home() -> Result<PathBuf, CommandError> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| CommandError::Refused("HOME is not set".to_owned()))
}

fn context(launchd: &Launchctl) -> Result<Ctx<'_>, CommandError> {
    let home = home()?;
    let exe = std::env::current_exe().and_then(|exe| exe.canonicalize());
    let rsrsbar_exe = exe.map_err(|source| CommandError::Io {
        path: PathBuf::from("current executable"),
        source,
    })?;
    Ok(Ctx {
        paths: Paths::new(home),
        launchd,
        git_rsrs: PathBuf::from(GIT_RSRS),
        rsrsbar_exe,
        now: SystemTime::now(),
    })
}
