//! Criterion micro-benchmarks for glassy's hot, GPU-free paths.
//!
//! Reached via the `glassy` **library** crate (`src/lib.rs`) — none of these
//! functions were reachable from an external target before that existed (see
//! docs/benchmarks.md). Four groups, matching the four candidates identified
//! as isolated, GPU-free, and worth tracking over time:
//!
//!   1. `collect_display_row` — per-frame display-row extraction from the
//!      alacritty_terminal grid (src/app/helpers.rs).
//!   2. `parse_config_file` — the `glassy.conf` parser (src/config/parse.rs),
//!      via a `#[doc(hidden)]` shim that keeps its `RawConfig` accumulator
//!      private (see `glassy::config::parse_config_file_bench`).
//!   3. `theme_by_name` / `theme_entries` / `theme_names` — the 60-built-in-theme
//!      registry lookup (src/color/registry.rs).
//!   4. `pane_damaged` — the thin wrapper around alacritty_terminal's own damage
//!      tracker (src/app/multipane.rs), benched against a real (but otherwise
//!      idle) `Pty`.
//!
//! Run: `cargo bench --bench hot_paths` (or, for a quick compile+run smoke
//! test, `cargo bench --bench hot_paths -- --measurement-time 1`).

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Indexed};
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Handler;
use criterion::{Criterion, criterion_group, criterion_main};

use glassy::app::collect_display_row;
use glassy::color::{theme_by_name, theme_entries, theme_names};
use glassy::config::parse_config_file_bench;

// ---------------------------------------------------------------------------
// Group 1: collect_display_row
// ---------------------------------------------------------------------------

/// Minimal [`Dimensions`] for a PTY-less [`Term`] fixture — mirrors the
/// `#[cfg(test)]` helper in `src/app/helpers.rs` (which isn't reachable here:
/// test-only items don't compile outside `cargo test`, and this bench needs
/// the identical fixture shape rather than importing it).
struct FixtureSize {
    cols: usize,
    lines: usize,
}

impl Dimensions for FixtureSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn make_term(cols: usize, lines: usize) -> Term<VoidListener> {
    Term::new(
        TermConfig::default(),
        &FixtureSize { cols, lines },
        VoidListener,
    )
}

/// Type a literal string through the VTE handler, breaking on `\n` — enough to
/// populate a grid with mixed ASCII/wide/combining content without spinning up
/// a real PTY.
fn type_str(t: &mut Term<VoidListener>, s: &str) {
    for ch in s.chars() {
        if ch == '\n' {
            t.linefeed();
            t.carriage_return();
        } else {
            t.input(ch);
        }
    }
}

fn bench_collect_display_row(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_display_row");

    for &(cols, lines) in &[(120usize, 40usize), (300, 100)] {
        let mut term = make_term(cols, lines);
        // Mixed ASCII + full-width CJK (WIDE_CHAR + spacer) + a combining mark,
        // repeated down every row so every row has non-trivial content.
        let row_text = "The quick brown fox ｗ jumps e\u{0301}x\n";
        for _ in 0..lines {
            type_str(&mut term, row_text);
        }

        let grid = term.grid();
        let display_offset = grid.display_offset() as i32;
        let screen_lines = grid.screen_lines();
        let mut out: Vec<Indexed<&alacritty_terminal::term::cell::Cell>> = Vec::new();

        group.bench_function(format!("{cols}x{lines}"), |b| {
            b.iter(|| {
                // Reused buffer across rows, matching the production per-frame
                // pattern (one call per visible row, buffer cleared each time).
                for row_u in 0..screen_lines {
                    collect_display_row(grid, row_u, display_offset, &mut out);
                    std::hint::black_box(&out);
                }
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 2: parse_config_file
// ---------------------------------------------------------------------------

const MINIMAL_CONFIG: &str = "font_size = 14\ntheme = tokyo-night\n";

const FULL_CONFIG: &str = r#"
font_family = FiraCode Nerd Font Mono
font_size   = 14
theme       = tokyo-night
opacity     = 0.92
window_effect = frosted
padding     = 6
padding_top = 8
padding_bottom = 6
padding_left = 4
padding_right = 4
shell       = /usr/bin/zsh -l
scrollback  = 10000
command_history = 200
bell_visual = true
bell_audible = false
follow_system = true
theme_light = one-light
theme_dark  = tokyo-night
status_bar  = true
pane_headers = true
word_separator = ,./\()\"'-:,;<>~!@#$%^&*|+=[]{}~?
ligatures   = true
cwd         =
restore_session = true
copy_on_select = false
hints_chars = asdfqwerzxcv
command_badges = true
color.fg = #c0caf5
color.bg = #1a1b26
color.cursor = #7dcfff
color.selection_bg = #283457
cursor_style = block
cursor_blink = true
minimap     = false
quake       = false
dim_unfocused = true
copy_html   = true
notify_command_finish = true
notify_command_threshold_ms = 5000
command_fold = true
power_mode  = false

[keybindings]
ctrl+shift+t = new-tab
ctrl+shift+w = close-tab

[profile.work]
shell = /usr/bin/fish
cwd = /home/user/work
theme = catppuccin-mocha
"#;

/// A config with `n` `[profile.X]` sections, to catch profile-scan blowup.
fn profiles_config(n: usize) -> String {
    let mut s = String::from(MINIMAL_CONFIG);
    for i in 0..n {
        s.push_str(&format!(
            "\n[profile.p{i}]\nshell = /bin/bash\ncwd = /tmp/p{i}\n"
        ));
    }
    s
}

fn bench_parse_config_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_config_file");

    group.bench_function("minimal", |b| {
        b.iter(|| parse_config_file_bench(std::hint::black_box(MINIMAL_CONFIG)).unwrap());
    });

    group.bench_function("full", |b| {
        b.iter(|| parse_config_file_bench(std::hint::black_box(FULL_CONFIG)).unwrap());
    });

    for n in [1usize, 5, 20] {
        let text = profiles_config(n);
        group.bench_function(format!("profiles_{n}"), |b| {
            b.iter(|| parse_config_file_bench(std::hint::black_box(&text)).unwrap());
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 3: theme_by_name / theme_entries / theme_names
// ---------------------------------------------------------------------------

fn bench_theme_by_name(c: &mut Criterion) {
    let mut group = c.benchmark_group("theme_by_name");

    group.bench_function("hit", |b| {
        b.iter(|| theme_by_name(std::hint::black_box("tokyo-night")));
    });

    group.bench_function("miss", |b| {
        b.iter(|| theme_by_name(std::hint::black_box("definitely-not-a-theme")));
    });

    group.bench_function("theme_entries", |b| {
        b.iter(theme_entries);
    });

    group.bench_function("theme_names", |b| {
        b.iter(theme_names);
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 4: pane_damaged
// ---------------------------------------------------------------------------

fn bench_pane_damaged(c: &mut Criterion) {
    use glassy::app::App;
    use glassy::pty::{Pty, UserEvent};
    use winit::event_loop::EventLoop;

    // `Pty::spawn` needs a real `EventLoopProxy<UserEvent>`, which only comes
    // from a real winit `EventLoop` — there's no lighter-weight constructor.
    // `pane_damaged` itself never touches the event loop (it only locks the
    // shared `Term`), so the loop is never run; it exists solely to mint the
    // proxy `Pty::spawn` requires. A trivial `true` "shell" exits immediately,
    // leaving an idle `Term` behind for the benchmark to hammer.
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!(
                "pane_damaged: skipping — no windowing system available to build an \
                 EventLoop ({e}); this bench needs a live X11/Wayland session"
            );
            return;
        }
    };
    let proxy = event_loop.create_proxy();

    let pty = Pty::spawn(
        proxy,
        0,
        80,
        24,
        8,
        16,
        Some(alacritty_terminal::tty::Shell::new(
            "true".to_string(),
            Vec::new(),
        )),
        None,
        1000,
        "",
        alacritty_terminal::vte::ansi::CursorShape::Block,
        false,
    )
    .expect("spawn a trivial 'true' child for the pane_damaged fixture");

    // Give the (near-instant) child a moment to exit and its PTY loop thread to
    // drain the first (Full) damage, so the steady state below is the
    // no-new-output case `render_split` hits on every unchanged pane.
    std::thread::sleep(std::time::Duration::from_millis(50));
    App::pane_damaged(&pty);

    let mut group = c.benchmark_group("pane_damaged");
    group.bench_function("idle", |b| {
        b.iter(|| std::hint::black_box(App::pane_damaged(&pty)));
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_collect_display_row,
    bench_parse_config_file,
    bench_theme_by_name,
    bench_pane_damaged
);
criterion_main!(benches);
