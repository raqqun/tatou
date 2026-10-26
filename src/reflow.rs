//! Reflows a terminal's screen to a new size, as iTerm2 or kitty do: the lines
//! its program printed, history included, wrap again at the new width, so that
//! narrowing loses nothing and widening joins them back. `vt100` alone cuts every
//! row to the new width, for good.

use tui_term::vt100::{self, Color};

/// A cell as drawn: its text, empty when nothing was ever written there, its
/// width, and the escape codes of its colours and attributes.
#[derive(Clone, PartialEq)]
struct Cell {
    text: String,
    wide: bool,
    style: Vec<u8>,
}

/// A row of the main screen, and whether its text goes on on the next one.
struct Row {
    cells: Vec<Cell>,
    wrapped: bool,
}

/// `screen` at `rows` × `cols`, keeping at least `scrollback` lines of history:
/// its main screen's lines, history included, wrapped again at the new width, the
/// cursor at the same place of its line, and its input modes and attributes. The
/// history grows to hold what narrowing makes of it, up to ten times as much. With
/// a full-screen program running, its screen is put back over the main one, at
/// the old size: the program draws it again for the new one.
pub fn reflow(
    screen: &mut vt100::Parser,
    rows: u16,
    cols: u16,
    scrollback: usize,
) -> vt100::Parser {
    let alternate = screen.screen().alternate_screen();
    if alternate {
        // Mode 47 switches without clearing either screen
        screen.process(b"\x1b[?47l");
    }
    let (old, cursor) = main_rows(screen);
    if alternate {
        screen.process(b"\x1b[?47h");
    }
    let (lines, cursor) = lines(&old, cursor);
    let needed: usize = lines
        .iter()
        .map(|cells| columns(cells).div_ceil(usize::from(cols)).max(1))
        .sum();
    let history = needed
        .saturating_sub(usize::from(rows))
        .clamp(scrollback, scrollback * 10);
    let mut new = vt100::Parser::new(rows, cols, history);
    write_lines(&mut new, &lines, cursor);
    let old_screen = screen.screen();
    if alternate {
        new.process(b"\x1b[?1049h");
        new.process(&old_screen.contents_formatted());
    }
    new.process(&old_screen.attributes_formatted());
    new.process(&old_screen.input_mode_formatted());
    if old_screen.hide_cursor() {
        new.process(b"\x1b[?25l");
    }
    new
}

/// Every row of `screen`'s current grid, history first, and the cursor's place
/// among them: (row, column).
fn main_rows(screen: &mut vt100::Parser) -> (Vec<Row>, (usize, u16)) {
    let (height, width) = screen.screen().size();
    let screen = screen.screen_mut();
    let mut rows = Vec::new();
    // The history a screen at a time, from its start; offsets are clamped
    screen.set_scrollback(usize::MAX);
    let history = screen.scrollback();
    let mut left = history;
    while left > 0 {
        screen.set_scrollback(left);
        let take = left.min(usize::from(height));
        rows.extend((0..take as u16).map(|row| read_row(screen, row, width)));
        left -= take;
    }
    screen.set_scrollback(0);
    rows.extend((0..height).map(|row| read_row(screen, row, width)));
    let (row, col) = screen.cursor_position();
    (rows, (history + usize::from(row), col))
}

fn read_row(screen: &vt100::Screen, row: u16, width: u16) -> Row {
    let cells = (0..width)
        .filter_map(|col| screen.cell(row, col))
        // Its second half is the wide character before it
        .filter(|cell| !cell.is_wide_continuation())
        .map(|cell| Cell {
            text: cell.contents().to_string(),
            wide: cell.is_wide(),
            style: style(cell),
        })
        .collect();
    Row {
        cells,
        wrapped: screen.row_wrapped(row),
    }
}

/// The escape codes that draw a cell as it is, from scratch.
fn style(cell: &vt100::Cell) -> Vec<u8> {
    let mut codes = vec!["0".to_string()];
    for (on, code) in [
        (cell.bold(), "1"),
        (cell.dim(), "2"),
        (cell.italic(), "3"),
        (cell.underline(), "4"),
        (cell.inverse(), "7"),
    ] {
        if on {
            codes.push(code.to_string());
        }
    }
    codes.extend(color(cell.fgcolor(), 30, 90, 38));
    codes.extend(color(cell.bgcolor(), 40, 100, 48));
    format!("\x1b[{}m", codes.join(";")).into_bytes()
}

/// SGR codes for `color`: from `base` for the first 8, `bright` for the next 8,
/// `extended` for the 256 and RGB ones.
fn color(color: Color, base: u8, bright: u8, extended: u8) -> Option<String> {
    match color {
        Color::Default => None,
        Color::Idx(n @ 0..8) => Some((base + n).to_string()),
        Color::Idx(n @ 8..16) => Some((bright + n - 8).to_string()),
        Color::Idx(n) => Some(format!("{extended};5;{n}")),
        Color::Rgb(r, g, b) => Some(format!("{extended};2;{r};{g};{b}")),
    }
}

/// Whether a cell shows nothing: never written, or a space without a background.
fn blank(cell: &Cell, default: &[u8]) -> bool {
    cell.text.is_empty() || (cell.text == " " && cell.style == default)
}

/// The lines of `rows`: rows joined where they wrapped, without the blanks that
/// end them, up to the cursor's line or the last with text, if further down; and
/// the cursor's line, and how many columns into it it is. `cursor` is its (row,
/// column) among `rows`.
fn lines(rows: &[Row], cursor: (usize, u16)) -> (Vec<Vec<Cell>>, (usize, usize)) {
    let default = b"\x1b[0m".to_vec();
    let mut lines: Vec<Vec<Cell>> = Vec::new();
    let mut cursor_at = (0, 0);
    let mut joining = false;
    for (index, row) in rows.iter().enumerate() {
        if !joining {
            lines.push(Vec::new());
        }
        let line = lines.len() - 1;
        if index == cursor.0 {
            cursor_at = (line, columns(&lines[line]) + usize::from(cursor.1));
        }
        let mut cells = row.cells.clone();
        if row.wrapped {
            // What was never written at its end is a wide character's gap
            while cells.last().is_some_and(|cell| cell.text.is_empty()) {
                cells.pop();
            }
        } else {
            // On the cursor's row, the blanks before it stay: a prompt's space
            let keep = if index == cursor.0 {
                usize::from(cursor.1)
            } else {
                0
            };
            while columns(&cells) > keep && cells.last().is_some_and(|cell| blank(cell, &default)) {
                cells.pop();
            }
        }
        lines[line].extend(cells);
        joining = row.wrapped;
    }
    let last = lines
        .iter()
        .rposition(|cells| !cells.is_empty())
        .unwrap_or(0)
        .max(cursor_at.0);
    lines.truncate(last + 1);
    (lines, cursor_at)
}

/// How many columns `cells` take.
fn columns(cells: &[Cell]) -> usize {
    cells.iter().map(|cell| if cell.wide { 2 } else { 1 }).sum()
}

/// Writes `lines` to `new`, one after the other, wrapping where its width says,
/// then puts the cursor back where it was in its line: `cursor` is that line and
/// how many columns into it.
fn write_lines(new: &mut vt100::Parser, lines: &[Vec<Cell>], cursor: (usize, usize)) {
    let default = b"\x1b[0m".to_vec();
    // Where the cursor goes, and how long the history was then
    let mut mark = None;
    for (index, cells) in lines.iter().enumerate() {
        if index > 0 {
            new.process(b"\x1b[0m\r\n");
        }
        let mut style: &[u8] = &default;
        let mut column = 0;
        for cell in cells {
            if index == cursor.0 && mark.is_none() && column >= cursor.1 {
                mark = Some((new.screen().cursor_position(), history_len(new)));
            }
            if cell.style != style {
                new.process(&cell.style);
                style = &cell.style;
            }
            new.process(
                if cell.text.is_empty() {
                    " "
                } else {
                    &cell.text
                }
                .as_bytes(),
            );
            column += if cell.wide { 2 } else { 1 };
        }
        // Right after its text, the last line's cursor is already there, a full
        // row's wrap pending; past it, as many columns further
        let last = index + 1 == lines.len();
        if index == cursor.0 && mark.is_none() && !(last && column == cursor.1) {
            let (row, col) = new.screen().cursor_position();
            let cols = usize::from(new.screen().size().1);
            let further = usize::from(col) + cursor.1 - column.min(cursor.1);
            let row = usize::from(row) + further / cols;
            mark = Some(((row as u16, (further % cols) as u16), history_len(new)));
        }
    }
    new.process(b"\x1b[0m");
    let Some(((row, col), history)) = mark else {
        return;
    };
    // The cursor's line may have scrolled up since
    let rows = new.screen().size().0;
    let scrolled = history_len(new) - history;
    let row = usize::from(row)
        .saturating_sub(scrolled)
        .min(usize::from(rows) - 1);
    new.process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
}

/// How many lines of history `parser` has.
fn history_len(parser: &mut vt100::Parser) -> usize {
    let screen = parser.screen_mut();
    let at = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let len = screen.scrollback();
    screen.set_scrollback(at);
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    const HISTORY: usize = 10_000;

    fn printed(rows: u16, cols: u16, text: &str) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, HISTORY);
        parser.process(text.as_bytes());
        parser
    }

    /// Every line, history included, as text.
    fn all_lines(parser: &mut vt100::Parser) -> Vec<String> {
        let (rows, cols) = parser.screen().size();
        let screen = parser.screen_mut();
        screen.set_scrollback(usize::MAX);
        let history = screen.scrollback();
        let mut lines = Vec::new();
        for offset in (1..=history).rev() {
            screen.set_scrollback(offset);
            lines.push(screen.contents_between(0, 0, 0, cols));
        }
        screen.set_scrollback(0);
        lines.extend((0..rows).map(|row| screen.contents_between(row, 0, row, cols)));
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    fn resized(parser: &mut vt100::Parser, rows: u16, cols: u16) -> vt100::Parser {
        reflow(parser, rows, cols, HISTORY)
    }

    #[test]
    fn narrowing_wraps_and_widening_joins_back() {
        let mut wide = printed(4, 30, "the quick brown fox jumps\r\n$ ");
        let mut narrow = resized(&mut wide, 4, 10);
        assert_eq!(
            all_lines(&mut narrow),
            ["the quick ", "brown fox ", "jumps", "$ "]
        );
        assert_eq!(narrow.screen().cursor_position(), (3, 2));
        let mut back = resized(&mut narrow, 4, 30);
        assert_eq!(all_lines(&mut back), ["the quick brown fox jumps", "$ "]);
        assert_eq!(back.screen().cursor_position(), (1, 2));
    }

    #[test]
    fn down_to_one_column_and_back_loses_nothing() {
        let text: String = (1..=25)
            .map(|n| format!("line {n}: the quick brown fox jumps over the lazy dog\r\n"))
            .collect();
        let mut screen = printed(30, 140, &format!("{text}$ "));
        let before = all_lines(&mut screen);
        for cols in [96, 40, 8, 1, 3, 60, 140] {
            screen = resized(&mut screen, 30, cols);
        }
        assert_eq!(all_lines(&mut screen), before);
        assert_eq!(screen.screen().cursor_position(), (25, 2));
    }

    #[test]
    fn shrinking_keeps_the_cursors_line_and_growing_brings_lines_back() {
        let text: String = (1..=8).map(|n| format!("line {n}\r\n")).collect();
        let mut tall = printed(10, 20, &format!("{text}$ "));
        let mut short = resized(&mut tall, 3, 20);
        let screen = short.screen();
        assert_eq!(screen.contents_between(0, 0, 2, 20), "line 7\nline 8\n$ ");
        assert_eq!(screen.cursor_position(), (2, 2));
        let mut tall_again = resized(&mut short, 10, 20);
        let screen = tall_again.screen();
        assert_eq!(screen.contents_between(0, 0, 0, 20), "line 1");
        assert_eq!(screen.cursor_position(), (8, 2));
        assert_eq!(history_len(&mut tall_again), 0);
    }

    #[test]
    fn colours_and_attributes_follow_their_text() {
        let mut wide = printed(
            4,
            20,
            "plain \x1b[1;31mbold red\x1b[0m \x1b[48;5;236mshade\x1b[0m",
        );
        let narrow = resized(&mut wide, 4, 8);
        let screen = narrow.screen();
        assert_eq!(screen.contents_between(0, 0, 2, 8), "plain bold red shade");
        let red = screen.cell(0, 6).unwrap();
        assert!(
            red.bold() && red.fgcolor() == Color::Idx(1),
            "the start of a row"
        );
        assert_eq!(screen.cell(1, 0).unwrap().contents(), "l", "wrapped");
        assert!(screen.cell(1, 0).unwrap().bold());
        assert_eq!(screen.cell(2, 0).unwrap().bgcolor(), Color::Idx(236));
        assert!(!screen.cell(0, 5).unwrap().bold(), "the space before it");
    }

    #[test]
    fn wide_characters_wrap_whole() {
        let mut wide = printed(4, 14, "日本語日本 ok");
        let mut narrow = resized(&mut wide, 4, 4);
        assert_eq!(all_lines(&mut narrow), ["日本", "語日", "本 o", "k"]);
        let mut back = resized(&mut narrow, 4, 14);
        assert_eq!(all_lines(&mut back), ["日本語日本 ok"]);
        // One column short of a wide character, vt100 ends the line there, as
        // tmux does: narrowing keeps it all, widening leaves it in two
        let mut odd = resized(&mut back, 4, 3);
        assert_eq!(all_lines(&mut odd), ["日", "本", "語", "日", "本 ", "ok"]);
    }

    #[test]
    fn input_modes_and_a_hidden_cursor_stay() {
        let mut screen = printed(
            4,
            20,
            "\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1h\x1b[?25l$ ",
        );
        let new = resized(&mut screen, 4, 10);
        let screen = new.screen();
        assert!(screen.bracketed_paste() && screen.application_cursor());
        assert_eq!(
            screen.mouse_protocol_mode(),
            vt100::MouseProtocolMode::PressRelease
        );
        assert_eq!(
            screen.mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Sgr
        );
        assert!(screen.hide_cursor());
    }

    #[test]
    fn a_full_screen_program_stays_over_the_reflowed_main_screen() {
        let mut screen = printed(
            4,
            20,
            "the quick brown fox jumps\r\n$ vim\x1b[?1049hfull screen",
        );
        let mut new = resized(&mut screen, 4, 10);
        assert!(new.screen().alternate_screen());
        assert_eq!(new.screen().contents_between(0, 0, 0, 10), "full scree");
        new.process(b"\x1b[?1049l");
        assert_eq!(
            all_lines(&mut new),
            ["the quick ", "brown fox ", "jumps", "$ vim"]
        );
    }

    #[test]
    fn down_to_two_rows_and_back_loses_nothing() {
        let text: String = (1..=25)
            .map(|n| format!("line {n}: the quick brown fox jumps over the lazy dog\r\n"))
            .collect();
        let mut screen = printed(30, 140, &format!("$ for\r\n{text}$ "));
        let before = all_lines(&mut screen);
        for (rows, cols) in [
            (12, 80),
            (4, 40),
            (2, 20),
            (2, 10),
            (2, 2),
            (2, 140),
            (30, 140),
        ] {
            screen = resized(&mut screen, rows, cols);
        }
        assert_eq!(all_lines(&mut screen), before);
    }

    #[test]
    fn a_cleared_screen_starts_at_its_top() {
        let mut screen = printed(6, 20, "old\r\n\x1b[2J\x1b[H$ ");
        let new = resized(&mut screen, 6, 10);
        assert_eq!(new.screen().cursor_position(), (0, 2));
    }
}
