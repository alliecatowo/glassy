mod app;
mod color;
mod input;
mod pty;
mod renderer;
mod text;

use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Typed event loop so the PTY thread can wake us via EventLoopProxy<UserEvent>.
    let event_loop = EventLoop::<pty::UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();

    // `shell: None` lets alacritty_terminal resolve the user's real login shell
    // from the passwd database (falling back to $SHELL), which is more correct
    // than reading $SHELL ourselves.
    let config = app::Config {
        font_size: 14.0,
        shell: None,
    };
    let mut app = app::App::new(proxy, config);

    event_loop.run_app(&mut app)?;
    Ok(())
}
