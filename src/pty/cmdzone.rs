//! Command-zone capture for OSC 133 shell integration.
//!
//! When a shell that emits OSC 133 marks runs a command, the sequence is:
//!   `133;A` (prompt start) → prompt is drawn → `133;B` (command start, cursor at
//!   the first typed column) → user types/edits the command → `133;C` (command
//!   executed, cursor still on the command line) → command runs → `133;D` (done).
//!
//! Between `B` and `C` the *final* command text is sitting on the grid (the shell
//! has finished its line editing — history recall, completion, cursor moves — so
//! the grid is authoritative, unlike replaying the raw byte stream). We therefore
//! capture the command by reading the grid cells from the `B` point to the cursor
//! position at `C`, joining rows that the terminal soft-wrapped (`WRAPLINE`).
//!
//! This module is pure grid-reading logic so it can be unit-tested without a live
//! PTY; the loop wires it up by recording the `B` point and calling
//! [`read_command_zone`] when the `C` mark arrives.

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};

/// The grid point (viewport-relative line + column) recorded when a `133;B`
/// command-start mark is seen, so the matching `133;C` can read the command text
/// that was typed starting there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CmdStart {
    pub line: i32,
    pub col: usize,
}

/// Longest command we will capture (defensive cap against a pathological prompt
/// that never emits `C`). Far longer than any realistic interactive command.
const MAX_CMD_LEN: usize = 8 * 1024;

/// Read the command text the user entered between a `133;B` start point and the
/// `end` point (the cursor when `133;C` arrived), from `grid`.
///
/// `start` and `end` are viewport-relative grid points (the same space as
/// `Grid` indexing and `cursor.point`). Rows are concatenated; a row whose final
/// cell carries `WRAPLINE` is joined to the next without a separator (it was a
/// soft wrap, not a real newline). Trailing whitespace on each unwrapped row is
/// trimmed. Returns `None` if the range is empty or yields only whitespace.
pub fn read_command_zone(grid: &Grid<Cell>, start: CmdStart, end: Point) -> Option<String> {
    // The command must start at or before the cursor; a backwards range means the
    // viewport scrolled or the marks were malformed — bail rather than guess.
    if end.line.0 < start.line {
        return None;
    }
    let cols = grid.columns();
    if cols == 0 {
        return None;
    }
    let end_line = end.line.0;
    let end_col = end.column.0;
    let mut out = String::new();
    // Whether the row just appended ended in a soft wrap (WRAPLINE): the next row
    // is its continuation and must join directly, with no separator space.
    let mut prev_wrapped = false;
    let mut line = start.line;
    while line <= end_line {
        // First row starts at the B column; later rows start at column 0.
        let first_col = if line == start.line { start.col } else { 0 };
        // Last row stops at the cursor column (exclusive); earlier rows run to the
        // grid width.
        let last_col = if line == end_line { end_col } else { cols };
        let last_col = last_col.min(cols);

        let li = Line(line);
        let mut row_text = String::new();
        let mut col = first_col;
        while col < last_col {
            let cell = &grid[Point::new(li, Column(col))];
            // A wide char occupies two cells; the trailing spacer carries no glyph.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                col += 1;
                continue;
            }
            let c = cell.c;
            row_text.push(if c == '\0' { ' ' } else { c });
            col += 1;
        }

        // Was this row soft-wrapped into the next? Check the WRAPLINE flag on the
        // last grid cell of the row (set by the terminal when text overflowed).
        let wrapped = grid[Point::new(li, Column(cols - 1))]
            .flags
            .contains(Flags::WRAPLINE);
        if wrapped && line < end_line {
            // Soft wrap: keep the full row (no trailing trim) and join directly.
            out.push_str(&row_text);
            prev_wrapped = true;
        } else {
            // Hard row end: trim trailing spaces. Separate from a preceding HARD
            // row with a space (multi-line commands collapse to one query line),
            // but join directly when the preceding row was a soft-wrap that this
            // row continues.
            let trimmed = row_text.trim_end_matches(' ');
            if !out.is_empty() && !trimmed.is_empty() && !prev_wrapped {
                out.push(' ');
            }
            out.push_str(trimmed);
            prev_wrapped = false;
        }
        if out.len() >= MAX_CMD_LEN {
            break;
        }
        line += 1;
    }

    let cmd = out.trim();
    if cmd.is_empty() {
        None
    } else {
        let mut cmd = cmd.to_string();
        if cmd.len() > MAX_CMD_LEN {
            // Truncate on a char boundary.
            let mut end = MAX_CMD_LEN;
            while end > 0 && !cmd.is_char_boundary(end) {
                end -= 1;
            }
            cmd.truncate(end);
        }
        Some(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::grid::Grid;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::cell::{Cell, Flags};

    /// Build a small grid and write `text` into row 0 starting at column 0.
    fn grid_with_row(cols: usize, rows: usize, text: &str) -> Grid<Cell> {
        let mut grid = Grid::<Cell>::new(rows, cols, 0);
        for (i, ch) in text.chars().enumerate() {
            if i >= cols {
                break;
            }
            grid[Point::new(Line(0), Column(i))].c = ch;
        }
        grid
    }

    #[test]
    fn reads_single_line_command() {
        let grid = grid_with_row(40, 5, "cargo build --release");
        let start = CmdStart { line: 0, col: 0 };
        let end = Point::new(Line(0), Column(21)); // cursor just past the text
        assert_eq!(
            read_command_zone(&grid, start, end).as_deref(),
            Some("cargo build --release")
        );
    }

    #[test]
    fn respects_start_column_skipping_prompt() {
        // Simulate a prompt "$ " in cols 0-1; B mark at col 2.
        let grid = grid_with_row(40, 5, "$ ls -la");
        let start = CmdStart { line: 0, col: 2 };
        let end = Point::new(Line(0), Column(8));
        assert_eq!(
            read_command_zone(&grid, start, end).as_deref(),
            Some("ls -la")
        );
    }

    #[test]
    fn trims_trailing_whitespace() {
        let grid = grid_with_row(40, 5, "echo hi          ");
        let start = CmdStart { line: 0, col: 0 };
        // Cursor is at the grid edge (trailing spaces should be trimmed away).
        let end = Point::new(Line(0), Column(40));
        assert_eq!(
            read_command_zone(&grid, start, end).as_deref(),
            Some("echo hi")
        );
    }

    #[test]
    fn empty_zone_returns_none() {
        let grid = grid_with_row(40, 5, "");
        let start = CmdStart { line: 0, col: 0 };
        let end = Point::new(Line(0), Column(0));
        assert_eq!(read_command_zone(&grid, start, end), None);
    }

    #[test]
    fn whitespace_only_returns_none() {
        let grid = grid_with_row(40, 5, "        ");
        let start = CmdStart { line: 0, col: 0 };
        let end = Point::new(Line(0), Column(8));
        assert_eq!(read_command_zone(&grid, start, end), None);
    }

    #[test]
    fn backwards_range_returns_none() {
        let grid = grid_with_row(40, 5, "anything");
        let start = CmdStart { line: 5, col: 0 };
        let end = Point::new(Line(0), Column(0));
        assert_eq!(read_command_zone(&grid, start, end), None);
    }

    #[test]
    fn joins_soft_wrapped_rows() {
        // 8-col grid: row 0 holds "abcdefgh" and WRAPLINE, row 1 holds "ij".
        let mut grid = Grid::<Cell>::new(5, 8, 0);
        for (i, ch) in "abcdefgh".chars().enumerate() {
            grid[Point::new(Line(0), Column(i))].c = ch;
        }
        grid[Point::new(Line(0), Column(7))]
            .flags
            .insert(Flags::WRAPLINE);
        grid[Point::new(Line(1), Column(0))].c = 'i';
        grid[Point::new(Line(1), Column(1))].c = 'j';
        let start = CmdStart { line: 0, col: 0 };
        let end = Point::new(Line(1), Column(2));
        // Soft wrap joins directly: "abcdefgh" + "ij" = "abcdefghij".
        assert_eq!(
            read_command_zone(&grid, start, end).as_deref(),
            Some("abcdefghij")
        );
    }

    #[test]
    fn hard_wrapped_rows_join_with_space() {
        // Two rows, NO wrapline flag (a real newline within the zone). They should
        // collapse to one line separated by a single space.
        let mut grid = Grid::<Cell>::new(5, 8, 0);
        for (i, ch) in "one".chars().enumerate() {
            grid[Point::new(Line(0), Column(i))].c = ch;
        }
        for (i, ch) in "two".chars().enumerate() {
            grid[Point::new(Line(1), Column(i))].c = ch;
        }
        let start = CmdStart { line: 0, col: 0 };
        let end = Point::new(Line(1), Column(3));
        assert_eq!(
            read_command_zone(&grid, start, end).as_deref(),
            Some("one two")
        );
    }
}
