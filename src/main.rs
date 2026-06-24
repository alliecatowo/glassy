mod app;
mod bell;
mod color;
mod config;
mod gui;
mod image;
mod input;
mod pane;
mod pty;
mod renderer;
mod text;

use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> anyhow::Result<()> {
    env_logger::init();

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
    let mut app = app::App::new(proxy, settings.config);

    event_loop.run_app(&mut app)?;
    Ok(())
}
