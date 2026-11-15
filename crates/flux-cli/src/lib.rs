//! The `flux` command line: authoring (`create`, `validate`, `package`, `dev`) and management
//! (`install`, `list`, `remove`, `enable`, `disable`, `logs`, `reload`) of extensions. Everything
//! here works on files and on the daemon's socket; nothing depends on the UI toolkit, and
//! [`run`] takes its environment as a value so tests drive it against temporary directories.

mod commands;
pub mod dev;
mod templates;

use std::{
    io::Write,
    path::PathBuf,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use flux_core::{Config, Db, Manager};
pub use flux_platform::DaemonCommand;

/// Exit codes: success, a failed operation (invalid extension, no daemon…), a usage error.
pub const OK: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;

#[derive(Parser, Debug)]
#[command(name = "flux", version, about = "Flux launcher and extension tools", propagate_version = true)]
pub struct Cli {
    /// Reload extensions when their files change
    #[arg(long, global = true)]
    pub dev: bool,
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub enum Cmd {
    /// Create an extension project from a template
    Create {
        /// Extension id (a-z, 0-9, - and _)
        name: String,
        /// list, grid, form or detail
        #[arg(long, short, default_value = "list")]
        template: String,
        /// Parent directory (default: the current one)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Develop an extension: load it in the launcher, reload on save, show logs and errors
    Dev {
        /// The extension folder (default: the current one)
        path: Option<PathBuf>,
    },
    /// Check manifest, structure, permissions and Lua syntax
    Validate {
        path: Option<PathBuf>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Build an installable `<name>-<version>.fluxext`
    Package {
        path: Option<PathBuf>,
        /// Output file or directory
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Install an extension from a folder or a .fluxext package
    Install {
        source: PathBuf,
        /// Replace an installed version even if it is not older
        #[arg(long)]
        force: bool,
    },
    /// List installed and development extensions
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove an installed extension
    Remove {
        name: String,
        /// Also delete its stored data and preferences
        #[arg(long)]
        purge: bool,
    },
    /// Enable an extension
    Enable { name: String },
    /// Disable an extension without deleting it
    Disable { name: String },
    /// Show an extension's log
    Logs {
        name: String,
        /// Keep printing new lines
        #[arg(long, short)]
        follow: bool,
        /// How many lines to show
        #[arg(long, short = 'n', default_value_t = 50)]
        lines: usize,
    },
    /// Show what an extension may do, or revoke / restore a permission
    Permissions {
        name: String,
        /// Take a permission away, e.g. `network` or `process.execute`
        #[arg(long)]
        revoke: Vec<String>,
        /// Undo a previous --revoke
        #[arg(long)]
        restore: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show what each extension is using (memory, CPU, tasks) and how it is isolated
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Ask the running launcher to reload (one extension, or all)
    Reload { name: Option<String> },
    /// Show or hide the launcher (starts the daemon if needed)
    Toggle,
    Show,
    Hide,
    /// Run the background daemon without showing the window
    Daemon,
    /// Stop the daemon
    Quit,
    /// Create the desktop entry the global-shortcut portal needs
    InstallDesktop,
}

/// What a command line asks for: finished here, or something that needs the application.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done(i32),
    /// Needs the GUI process (daemon control, `dev`, …).
    Gui(Gui),
}

#[derive(Debug, PartialEq, Clone)]
pub enum Gui {
    Toggle,
    Show,
    Hide,
    Daemon,
    Quit,
    InstallDesktop,
    /// `flux dev <path>`: the folder is validated; the caller starts or joins the daemon.
    Dev { dir: PathBuf },
}

/// Talks to the running launcher. A trait so tests can record instead of using a socket.
pub trait Daemon {
    fn send(&self, cmd: &DaemonCommand) -> std::io::Result<()>;
    /// A read-only question to the daemon (`status`); the answer is one line.
    fn query(&self, what: &str) -> std::io::Result<String>;
}

pub struct SocketDaemon(pub PathBuf);

impl Daemon for SocketDaemon {
    fn send(&self, cmd: &DaemonCommand) -> std::io::Result<()> {
        flux_platform::ipc::send(&self.0, cmd)
    }

    fn query(&self, what: &str) -> std::io::Result<String> {
        flux_platform::ipc::query(&self.0, what)
    }
}

/// Everything a command may touch.
pub struct Env {
    pub cwd: PathBuf,
    pub installed_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub db_path: PathBuf,
    /// Where `permissions.toml` lives.
    pub config_dir: PathBuf,
    /// Directories that hold development extensions (for `flux list`).
    pub extension_dirs: Vec<PathBuf>,
    pub daemon: Box<dyn Daemon>,
    pub out: Box<dyn Write>,
    pub err: Box<dyn Write>,
}

impl Env {
    /// The real environment: XDG directories, the daemon socket, stdout/stderr.
    pub fn from_system() -> Env {
        let config = Config::from_env(false);
        Env {
            cwd: std::env::current_dir().unwrap_or_default(),
            installed_dir: config.installed_dir.clone(),
            data_dir: config.data_dir.clone(),
            log_dir: config.log_dir.clone(),
            db_path: config.db_path(),
            config_dir: Config::config_dir(),
            extension_dirs: config.extension_dirs.iter().filter(|d| **d != config.installed_dir).cloned().collect(),
            daemon: Box::new(SocketDaemon(flux_platform::ipc::socket_path())),
            out: Box::new(std::io::stdout()),
            err: Box::new(std::io::stderr()),
        }
    }

    pub(crate) fn manager(&self) -> Result<Manager, String> {
        let db = Db::open(&self.db_path).map_err(|e| format!("cannot open the database: {e}"))?;
        Ok(Manager::new(self.installed_dir.clone(), Arc::new(db)))
    }

    pub(crate) fn db(&self) -> Result<Arc<Db>, String> {
        Db::open(&self.db_path).map(Arc::new).map_err(|e| format!("cannot open the database: {e}"))
    }

    pub(crate) fn resolve(&self, p: &Option<PathBuf>) -> PathBuf {
        let p = p.clone().unwrap_or_else(|| PathBuf::from("."));
        if p.is_absolute() { p } else { self.cwd.join(p) }
    }
}

/// Parses `args` (without the program name) and runs the command.
pub fn run(args: &[String], env: &mut Env) -> Outcome {
    let argv = std::iter::once("flux".to_string()).chain(args.iter().cloned());
    let cli = match Cli::try_parse_from(argv) {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { USAGE } else { OK };
            let text = e.render().to_string();
            let _ = if code == OK { write!(env.out, "{text}") } else { write!(env.err, "{text}") };
            return Outcome::Done(code);
        }
    };
    dispatch(cli.command, env)
}

pub fn dispatch(cmd: Option<Cmd>, env: &mut Env) -> Outcome {
    let result = match cmd {
        None | Some(Cmd::Toggle) => return Outcome::Gui(Gui::Toggle),
        Some(Cmd::Show) => return Outcome::Gui(Gui::Show),
        Some(Cmd::Hide) => return Outcome::Gui(Gui::Hide),
        Some(Cmd::Daemon) => return Outcome::Gui(Gui::Daemon),
        Some(Cmd::Quit) => return Outcome::Gui(Gui::Quit),
        Some(Cmd::InstallDesktop) => return Outcome::Gui(Gui::InstallDesktop),
        Some(Cmd::Dev { path }) => match commands::dev_prepare(env, &path) {
            Ok(dir) => return Outcome::Gui(Gui::Dev { dir }),
            Err(e) => Err(e),
        },
        Some(Cmd::Create { name, template, dir }) => commands::create(env, &name, &template, dir),
        Some(Cmd::Validate { path, json }) => commands::validate(env, &path, json),
        Some(Cmd::Package { path, out }) => commands::package(env, &path, out),
        Some(Cmd::Install { source, force }) => commands::install(env, &source, force),
        Some(Cmd::List { json }) => commands::list(env, json),
        Some(Cmd::Remove { name, purge }) => commands::remove(env, &name, purge),
        Some(Cmd::Enable { name }) => commands::set_enabled(env, &name, true),
        Some(Cmd::Disable { name }) => commands::set_enabled(env, &name, false),
        Some(Cmd::Logs { name, follow, lines }) => commands::logs(env, &name, follow, lines),
        Some(Cmd::Permissions { name, revoke, restore, json }) => commands::permissions(env, &name, &revoke, &restore, json),
        Some(Cmd::Status { json }) => commands::status(env, json),
        Some(Cmd::Reload { name }) => commands::reload(env, name),
    };
    match result {
        Ok(()) => Outcome::Done(OK),
        Err(CliError::Failed(m)) => {
            let _ = writeln!(env.err, "error: {m}");
            Outcome::Done(FAILED)
        }
        // validation already printed its own report
        Err(CliError::Reported) => Outcome::Done(FAILED),
    }
}

#[derive(Debug)]
pub enum CliError {
    Failed(String),
    /// The command already explained what is wrong.
    Reported,
}

impl From<String> for CliError {
    fn from(s: String) -> Self {
        CliError::Failed(s)
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Failed(e.to_string())
    }
}

#[cfg(test)]
mod tests;
