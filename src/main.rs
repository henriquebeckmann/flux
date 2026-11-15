mod ui;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use flux_api::{SecretStore, UnavailableSecretStore};
use flux_cli::{Env, Gui, Outcome};
use flux_core::{Config, Core};
use flux_platform::{DaemonCommand, KeyringStore, hotkey, ipc};
use gpui::{App, AppContext, QuitMode};
use gpui_platform::application;

fn main() -> anyhow::Result<()> {
    // `flux host`: an Extension Host, started by the launcher for one extension. It must not
    // touch the UI toolkit, the database or the daemon socket.
    if std::env::args().nth(1).as_deref() == Some("host") {
        std::process::exit(flux_runtime::host::main());
    }
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,gpui::text_system=error,flux=info,flux_core=info,flux_runtime=info,flux_platform=info"),
    )
    .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let dev_flag = args.iter().any(|a| a == "--dev") || std::env::var_os("FLUX_DEV").is_some();
    let mut env = Env::from_system();
    match flux_cli::run(&args, &mut env) {
        Outcome::Done(code) => std::process::exit(code),
        Outcome::Gui(gui) => run_gui(gui, dev_flag, env),
    }
}

fn run_gui(gui: Gui, dev: bool, env: Env) -> anyhow::Result<()> {
    let socket = ipc::socket_path();

    // What this invocation asks for, and whether it may start a daemon to do it.
    let (command, may_start, show_on_start, is_daemon) = match &gui {
        Gui::InstallDesktop => {
            let path = flux_platform::desktop::entry_path();
            flux_platform::desktop::install(&path, &std::env::current_exe()?)?;
            println!("wrote {}\nRestart the daemon (`flux quit && flux daemon`) to register the global shortcut.", path.display());
            return Ok(());
        }
        Gui::Toggle => (DaemonCommand::Toggle, true, true, false),
        Gui::Show => (DaemonCommand::Show, true, true, false),
        Gui::Daemon => (DaemonCommand::Show, true, false, true),
        Gui::Hide => (DaemonCommand::Hide, false, false, false),
        Gui::Quit => (DaemonCommand::Quit, false, false, false),
        Gui::Dev { dir } => (DaemonCommand::Attach(dir.clone()), true, true, false),
    };

    if !may_start {
        return match ipc::send(&socket, &command) {
            Ok(()) => Ok(()),
            Err(_) => {
                eprintln!("flux is not running");
                Ok(())
            }
        };
    }

    match ipc::bind(&socket)? {
        ipc::Bind::AlreadyRunning => {
            if is_daemon {
                eprintln!("flux is already running");
                return Ok(());
            }
            ipc::send(&socket, &command)?; // a second instance only forwards the request
            if let Gui::Dev { dir } = gui {
                dev_attached(dir, &socket, &env);
            }
            Ok(())
        }
        ipc::Bind::Server(server) => {
            let attach = match &gui {
                Gui::Dev { dir } => Some(dir.clone()),
                _ => None,
            };
            if let Some(dir) = &attach {
                spawn_dev_helpers(dir.clone(), &env);
            }
            run_daemon(server, dev || attach.is_some(), show_on_start, attach)
        }
    }
}

/// Re-validates on every save (syntax errors with file:line in the terminal) and prints the
/// extension's log as it grows. Both run on background threads.
fn spawn_dev_helpers(dir: PathBuf, env: &Env) {
    let stop = flux_cli::dev::interrupted_flag();
    let out: Arc<Mutex<Box<dyn std::io::Write + Send>>> = Arc::new(Mutex::new(Box::new(std::io::stdout())));
    {
        let (dir, out) = (dir.clone(), out.clone());
        std::thread::spawn(move || flux_cli::dev::watch_validate(dir, out, stop));
    }
    if let Some(name) = flux_cli::dev::extension_name(&dir) {
        let path = flux_core::log_path(&env.log_dir, &name);
        std::thread::spawn(move || flux_cli::dev::follow_file(&path, &mut std::io::stdout(), stop));
    }
}

/// `flux dev` while the launcher already runs: the folder was attached over the socket; keep
/// validating and tailing until Ctrl+C, then detach.
fn dev_attached(dir: PathBuf, socket: &std::path::Path, env: &Env) {
    println!("Attached {} to the running launcher. Press Ctrl+C to detach.", dir.display());
    spawn_dev_helpers(dir.clone(), env);
    let stop = flux_cli::dev::interrupted_flag();
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = ipc::send(socket, &DaemonCommand::Detach(dir));
    println!("Detached.");
}

fn run_daemon(server: ipc::Server, dev: bool, show_on_start: bool, attach: Option<PathBuf>) -> anyhow::Result<()> {
    let config = Config::from_env(dev);
    log::info!("extensions in {:?} (dev: {dev})", config.extension_dirs);

    // Credentials only ever go to the Secret Service; without one, secure storage reports an error.
    let secrets: Arc<dyn SecretStore> = match KeyringStore::new() {
        Ok(store) => Arc::new(store),
        Err(e) => Arc::new(UnavailableSecretStore(format!("could not start the keyring client: {e}"))),
    };
    let (mut core, events) = Core::start(&config, secrets)?;
    if let Some(dir) = attach {
        core.attach_dir(dir);
    }

    let (daemon_tx, daemon_rx) = async_channel::unbounded::<DaemonCommand>();
    let socket = server.path().to_path_buf();
    let status_runtime = core.runtime_handle();
    server.serve_with(
        daemon_tx.clone(),
        Some(Arc::new(move || serde_json::to_string(&status_runtime.stats()).unwrap_or_else(|e| format!("error: {e}")))),
    );
    hotkey::spawn(config.hotkey.clone(), daemon_tx);

    let image_cache = config.data_dir.join("image-cache");
    let reduce_motion = config.reduce_motion;
    // Closing the last window must not end the process: only an explicit quit does.
    application().with_assets(ui::Assets).with_quit_mode(QuitMode::Explicit).run(move |cx: &mut App| {
        ui::install(cx, image_cache, reduce_motion);
        ui::bind_keys(cx);
        let core = cx.new(|_| core);
        ui::shell::install(cx, core.clone(), socket);

        // Runtime events -> core state (+ effects such as hiding the window).
        {
            let core = core.clone();
            cx.spawn(async move |cx| {
                while let Ok(event) = events.recv().await {
                    let effects = core.update(cx, |core, cx| {
                        let effects = core.on_event(event);
                        cx.notify();
                        effects
                    });
                    let _ = cx.update(|cx| ui::shell::apply(effects, cx));
                }
            })
            .detach();
        }

        // `flux show|hide|toggle|quit|reload|…` and the global shortcut.
        cx.spawn(async move |cx| {
            while let Ok(cmd) = daemon_rx.recv().await {
                let _ = cx.update(|cx| match cmd {
                    DaemonCommand::Show => ui::shell::show(cx),
                    DaemonCommand::Hide => ui::shell::hide(cx),
                    DaemonCommand::Toggle => ui::shell::toggle(cx),
                    DaemonCommand::Quit => ui::shell::quit(cx),
                    DaemonCommand::Reload => core.read(cx).reload(),
                    DaemonCommand::ReloadExtension(name) => core.read(cx).reload_extension(&name),
                    DaemonCommand::Enable(name) => core.update(cx, |c, _| c.set_extension_enabled(&name, true)),
                    DaemonCommand::Disable(name) => core.update(cx, |c, _| c.set_extension_enabled(&name, false)),
                    DaemonCommand::Attach(dir) => {
                        core.update(cx, |c, _| c.attach_dir(dir));
                        ui::shell::show(cx);
                    }
                    DaemonCommand::Detach(dir) => core.update(cx, |c, _| c.detach_dir(dir)),
                });
            }
        })
        .detach();

        if show_on_start {
            ui::shell::show(cx);
        }
    });
    Ok(())
}
