mod app;
mod bell;
mod color;
mod config;
mod gui;
mod image;
mod input;
mod ipc;
mod pane;
mod pty;
mod renderer;
mod session;
mod text;

use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Control subcommand: `glassy toggle|show|hide` (also accepted as
    // `--toggle`/`--show`/`--hide`) is a thin CLIENT that signals an already-running
    // instance over the single-instance Unix socket, then exits. This is glassy's
    // answer to the quake/dropdown hotkey: Wayland has no portable global-hotkey
    // API, so the user binds `glassy toggle` to a key in *their compositor*, which
    // forwards the toggle here. If no instance is running, we print a hint and exit
    // non-zero (the bind should normally launch glassy first). See docs/quake-mode.md.
    if let Some(cmd) = ipc::IpcCommand::parse(
        std::env::args()
            .nth(1)
            .unwrap_or_default()
            .trim_start_matches("--"),
    ) {
        return run_control_client(cmd);
    }

    // Resolve configuration: config file first, then CLI overrides. `--help`
    // prints usage and exits; a parse error is reported and aborts startup.
    let settings = match config::Settings::resolve(std::env::args().skip(1)) {
        Ok(Some(settings)) => settings,
        Ok(None) => return Ok(()), // --help / --version printed and exited cleanly
        Err(e) => {
            eprintln!("glassy: {e}");
            std::process::exit(2);
        }
    };

    // Install the active color theme before any rendering reads it.
    color::set_theme(settings.theme);

    // Typed event loop so the PTY thread can wake us via EventLoopProxy<UserEvent>.
    let event_loop = EventLoop::<pty::UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();

    // Single-instance IPC server: lets `glassy toggle/show/hide` (bound to a
    // compositor hotkey) drive the quake window's slide. A failure to bind is
    // non-fatal — glassy still runs, just without remote toggling.
    match ipc::start_server(proxy.clone()) {
        Ok(true) => {}
        Ok(false) => log::info!("ipc: another instance owns the socket; toggle disabled here"),
        Err(e) => log::warn!("ipc: failed to start control server: {e}"),
    }

    let mut app = app::App::new(proxy, settings.config);

    event_loop.run_app(&mut app)?;
    ipc::cleanup();
    Ok(())
}

/// Run the `toggle`/`show`/`hide` control subcommand as a client: signal the
/// running instance over the IPC socket and exit. Prints a hint (and exits
/// non-zero) when no instance is listening, since the user almost certainly meant
/// to toggle a window that isn't running yet.
fn run_control_client(cmd: ipc::IpcCommand) -> anyhow::Result<()> {
    match ipc::send_command(cmd) {
        Ok(true) => Ok(()),
        Ok(false) => {
            eprintln!(
                "glassy: no running instance to '{}'. Start glassy first, then bind\n\
                 'glassy {}' to a key in your compositor (see docs/quake-mode.md).",
                cmd.verb(),
                cmd.verb()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("glassy: control command failed: {e}");
            std::process::exit(1);
        }
    }
}
