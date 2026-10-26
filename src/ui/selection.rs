//! Text selected with the mouse in the shown terminal, for a program that leaves
//! the mouse to it: a span dragged over, a word or a line, in screen cells.

use tui_term::vt100::Screen;

/// What a selection grows by: a click, two or three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Cell,
    /// A run of characters other than spaces: a path, a URL or a hash, whole.
    Word,
    Line,
}

/// From where the mouse went down to where it is, as (row, column) of the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    anchor: (u16, u16),
    head: (u16, u16),
    unit: Unit,
}

impl Selection {
    pub fn new(cell: (u16, u16), unit: Unit) -> Selection {
        Selection {
            anchor: cell,
            head: cell,
            unit,
        }
    }

    /// Moves its end to `cell`, where the mouse is dragged.
    pub fn extend(&mut self, cell: (u16, u16)) {
        self.head = cell;
    }

    /// A click that was not dragged selects nothing.
    pub fn is_empty(&self) -> bool {
        self.unit == Unit::Cell && self.anchor == self.head
    }

    /// Its first and last cells, both included, in reading order and grown to its
    /// unit on `screen`.
    pub fn bounds(&self, screen: &Screen) -> ((u16, u16), (u16, u16)) {
        let (start, end) = match self.anchor <= self.head {
            true => (self.anchor, self.head),
            false => (self.head, self.anchor),
        };
        let last = screen.size().1.saturating_sub(1);
        match self.unit {
            Unit::Cell => (start, end),
            Unit::Word => {
                let (first, _) = word(screen, start);
                let (_, after) = word(screen, end);
                ((start.0, first), (end.0, after))
            }
            Unit::Line => ((start.0, 0), (end.0, last)),
        }
    }

    /// Whether `cell` is selected, of the `bounds` it has.
    pub fn contains(bounds: ((u16, u16), (u16, u16)), cell: (u16, u16)) -> bool {
        bounds.0 <= cell && cell <= bounds.1
    }

    /// The text selected on `screen`: lines joined where they wrapped, without the
    /// blanks that end them.
    pub fn text(&self, screen: &Screen) -> String {
        let ((start_row, start_col), (end_row, end_col)) = self.bounds(screen);
        screen.contents_between(start_row, start_col, end_row, end_col + 1)
    }
}

/// The first and last columns of the word at `cell`, or the cell alone on a space.
fn word(screen: &Screen, (row, col): (u16, u16)) -> (u16, u16) {
    let blank = |col: u16| {
        screen
            .cell(row, col)
            .is_none_or(|cell| cell.contents().trim().is_empty() && !cell.is_wide_continuation())
    };
    if blank(col) {
        return (col, col);
    }
    let cols = screen.size().1;
    let mut first = col;
    while first > 0 && !blank(first - 1) {
        first -= 1;
    }
    let mut last = col;
    while last + 1 < cols && !blank(last + 1) {
        last += 1;
    }
    (first, last)
}

#[cfg(test)]
mod tests {
    use tui_term::vt100::Parser;

    use super::*;

    fn screen(text: &str) -> Parser {
        let mut parser = Parser::new(4, 20, 0);
        parser.process(text.as_bytes());
        parser
    }

    #[test]
    fn a_drag_selects_what_it_goes_over_in_reading_order() {
        let parser = screen("one two\r\nthree four");
        let mut selection = Selection::new((1, 3), Unit::Cell);
        assert!(selection.is_empty(), "not dragged yet");
        selection.extend((0, 4));
        assert!(!selection.is_empty());
        assert_eq!(
            selection.bounds(parser.screen()),
            ((0, 4), (1, 3)),
            "backwards"
        );
        assert_eq!(selection.text(parser.screen()), "two\nthre");
        let bounds = selection.bounds(parser.screen());
        assert!(
            Selection::contains(bounds, (0, 19)),
            "the rest of the first line"
        );
        assert!(!Selection::contains(bounds, (1, 4)));
    }

    #[test]
    fn a_word_is_a_run_of_non_spaces() {
        let parser = screen("cd ~/code/app/fix  now");
        let selection = Selection::new((0, 8), Unit::Word);
        assert_eq!(selection.text(parser.screen()), "~/code/app/fix");
        let on_a_space = Selection::new((0, 17), Unit::Word);
        assert_eq!(on_a_space.text(parser.screen()).trim(), "");

        // Wrapped at 20 columns: "ls ~/code/app.worktr", then "ees/fix  now"
        let parser = screen("ls ~/code/app.worktrees/fix  now");
        let mut selection = Selection::new((0, 4), Unit::Word);
        assert_eq!(selection.text(parser.screen()), "~/code/app.worktr");
        selection.extend((1, 2));
        assert_eq!(selection.text(parser.screen()), "~/code/app.worktrees/fix");
    }

    #[test]
    fn a_line_is_the_whole_row_without_its_trailing_blanks() {
        let parser = screen("first\r\nsecond line\r\nthird");
        let mut selection = Selection::new((1, 3), Unit::Line);
        assert_eq!(selection.text(parser.screen()), "second line");
        selection.extend((2, 0));
        assert_eq!(selection.text(parser.screen()), "second line\nthird");
    }

    #[test]
    fn wide_characters_stay_whole() {
        let parser = screen("日本語 text");
        let selection = Selection::new((0, 1), Unit::Word);
        assert_eq!(selection.text(parser.screen()), "日本語");
    }
}
