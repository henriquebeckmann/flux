//! Single-instance daemon control over a unix socket.
//!
//! The first `flux` process binds the socket and becomes the daemon. Later invocations
//! connect, send one command (`show`, `hide`, `toggle`, `quit`) and exit. This is also the
//! compositor-agnostic way to get a global shortcut: bind a key to `flux toggle`.

use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use std::{io::Read as _, sync::Arc};

use async_channel::Sender;

/// Produces the reply of the `status` query (JSON on one line).
pub type StatusFn = Arc<dyn Fn() -> String + Send + Sync>;

/// Longest request line a client may send; anything longer is not a command.
const MAX_REQUEST: u64 = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonCommand {
    Show,
    Hide,
    Toggle,
    Quit,
    /// Reload every extension.
    Reload,
    /// Reload one extension (name or folder name).
    ReloadExtension(String),
    Enable(String),
    Disable(String),
    /// Load an extension folder from outside the usual directories (`flux dev`).
    Attach(PathBuf),
    Detach(PathBuf),
}

/// Names and paths travel on one line: refuse anything that could break the framing.
fn clean_arg(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= 4096 && !s.contains(['\n', '\r', '\0'])).then_some(s)
}

impl DaemonCommand {
    /// One protocol line, without the newline.
    pub fn to_line(&self) -> String {
        match self {
            Self::Show => "show".into(),
            Self::Hide => "hide".into(),
            Self::Toggle => "toggle".into(),
            Self::Quit => "quit".into(),
            Self::Reload => "reload".into(),
            Self::ReloadExtension(n) => format!("reload {n}"),
            Self::Enable(n) => format!("enable {n}"),
            Self::Disable(n) => format!("disable {n}"),
            Self::Attach(p) => format!("attach {}", p.display()),
            Self::Detach(p) => format!("detach {}", p.display()),
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        let (verb, arg) = match line.split_once(' ') {
            Some((v, a)) => (v, Some(clean_arg(a)?)),
            None => (line, None),
        };
        let name = |a: Option<&str>| a.filter(|n| !n.contains('/') && !n.starts_with('.')).map(String::from);
        let path = |a: Option<&str>| a.map(PathBuf::from).filter(|p| p.is_absolute());
        Some(match (verb, arg) {
            ("show", None) => Self::Show,
            ("hide", None) => Self::Hide,
            ("toggle", None) => Self::Toggle,
            ("quit", None) => Self::Quit,
            ("reload", None) => Self::Reload,
            ("reload", a) => Self::ReloadExtension(name(a)?),
            ("enable", a) => Self::Enable(name(a)?),
            ("disable", a) => Self::Disable(name(a)?),
            ("attach", a) => Self::Attach(path(a)?),
            ("detach", a) => Self::Detach(path(a)?),
            _ => return None,
        })
    }
}

/// `$XDG_RUNTIME_DIR/flux.sock` (the directory is private to the user), falling back to `/tmp`.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("flux.sock"),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            PathBuf::from(format!("/tmp/flux-{user}.sock"))
        }
    }
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

pub enum Bind {
    /// We are the daemon.
    Server(Server),
    /// Another live daemon owns the socket.
    AlreadyRunning,
}

/// Claims the socket. A socket file left behind by a crashed daemon is detected (nobody
/// answers) and replaced.
pub fn bind(path: &Path) -> io::Result<Bind> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(Bind::Server(finish(listener, path)?)),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Ok(Bind::AlreadyRunning);
            }
            std::fs::remove_file(path)?;
            Ok(Bind::Server(finish(UnixListener::bind(path)?, path)?))
        }
        Err(e) => Err(e),
    }
}

fn finish(listener: UnixListener, path: &Path) -> io::Result<Server> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Server { listener, path: path.to_path_buf() })
}

impl Server {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serves commands on a background thread, forwarding them to `tx`.
    pub fn serve(self, tx: Sender<DaemonCommand>) {
        self.serve_with(tx, None)
    }

    /// Like [`serve`](Self::serve), and answers the read-only `status` query with `status()`.
    pub fn serve_with(self, tx: Sender<DaemonCommand>, status: Option<StatusFn>) {
        std::thread::Builder::new()
            .name("flux-ipc".into())
            .spawn(move || {
                for stream in self.listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut line = String::new();
                    if BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line).is_err() {
                        continue;
                    }
                    if line.trim() == "status" {
                        let body = status.as_ref().map(|f| f()).unwrap_or_else(|| "error: status is not available".into());
                        let _ = (&stream).write_all(format!("{}\n", body.replace('\n', " ")).as_bytes());
                        continue;
                    }
                    let reply = match DaemonCommand::parse(&line) {
                        Some(cmd) => {
                            if tx.send_blocking(cmd).is_err() {
                                break; // the app is gone
                            }
                            "ok\n"
                        }
                        None => "error: unknown command\n",
                    };
                    let _ = (&stream).write_all(reply.as_bytes());
                }
            })
            .expect("spawn ipc thread");
    }
}

/// Sends a command to the running daemon.
pub fn send(path: &Path, cmd: &DaemonCommand) -> io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{}", cmd.to_line())?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    if reply.trim() == "ok" {
        Ok(())
    } else {
        Err(io::Error::other(reply.trim().to_string()))
    }
}

/// Asks the daemon a read-only question (`status`) and returns its one-line answer.
pub fn query(path: &Path, what: &str) -> io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "{what}")?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let reply = reply.trim().to_string();
    match reply.strip_prefix("error: ") {
        Some(e) => Err(io::Error::other(e.to_string())),
        None => Ok(reply),
    }
}

pub fn remove_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trip() {
        for c in [
            DaemonCommand::Show,
            DaemonCommand::Hide,
            DaemonCommand::Toggle,
            DaemonCommand::Quit,
            DaemonCommand::Reload,
            DaemonCommand::ReloadExtension("my-ext".into()),
            DaemonCommand::Enable("a_b".into()),
            DaemonCommand::Disable("x".into()),
            DaemonCommand::Attach("/home/u/dev/ext with space".into()),
            DaemonCommand::Detach("/tmp/x".into()),
        ] {
            assert_eq!(DaemonCommand::parse(&c.to_line()), Some(c.clone()), "{c:?}");
        }
        assert_eq!(DaemonCommand::parse("  toggle\n"), Some(DaemonCommand::Toggle));
        assert_eq!(DaemonCommand::parse("reboot"), None);
        // framing and traversal tricks are refused
        for bad in ["attach relative/path", "attach", "reload ../x", "enable a/b", "disable .hidden", "show extra", "quit now", "reload \0"] {
            assert_eq!(DaemonCommand::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_one_daemon_and_clients_reach_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flux.sock");
        let Bind::Server(server) = bind(&path).unwrap() else { panic!("first bind must win") };
        assert!(matches!(bind(&path).unwrap(), Bind::AlreadyRunning), "second instance must not start");

        let (tx, rx) = async_channel::unbounded();
        server.serve(tx);
        send(&path, &DaemonCommand::Toggle).unwrap();
        send(&path, &DaemonCommand::Quit).unwrap();
        assert_eq!(rx.recv_blocking().unwrap(), DaemonCommand::Toggle);
        assert_eq!(rx.recv_blocking().unwrap(), DaemonCommand::Quit);
    }

    #[test]
    fn stale_socket_files_are_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flux.sock");
        // A crashed daemon leaves the file behind: bind then drop the listener without removing.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        assert!(matches!(bind(&path).unwrap(), Bind::Server(_)));
    }

    #[test]
    fn sending_without_a_daemon_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(send(&dir.path().join("none.sock"), &DaemonCommand::Show).is_err());
    }

    #[test]
    fn unknown_commands_are_rejected_by_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flux.sock");
        let Bind::Server(server) = bind(&path).unwrap() else { panic!() };
        let (tx, _rx) = async_channel::unbounded();
        server.serve(tx);
        let mut s = UnixStream::connect(&path).unwrap();
        writeln!(s, "format-disk").unwrap();
        let mut reply = String::new();
        BufReader::new(&s).read_line(&mut reply).unwrap();
        assert!(reply.starts_with("error"));
    }
}
