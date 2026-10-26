//! Modal dialogs: a form of text and choice fields, a choice list, a yes/no
//! confirmation and a scrollable message. An open dialog takes every key; the
//! dashboard keeps refreshing behind it. Their keys are fixed, not in the keymap.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::theme::Theme;

/// The widest a dialog gets, borders included.
const MAX_WIDTH: u16 = 72;

/// What a key did to a dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still open.
    Open,
    /// Closed without an answer: Esc, `n`, or closing a message.
    Cancel,
    /// Answered: the caller reads the values from the dialog.
    Submit,
}

pub enum Dialog {
    Form(Form),
    Choice(Choice),
    Confirm(Confirm),
    Message(Message),
}

impl Dialog {
    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        match self {
            Dialog::Form(form) => form.key(key),
            Dialog::Choice(choice) => choice.key(key),
            Dialog::Confirm(confirm) => confirm.key(key),
            Dialog::Message(message) => message.key(key),
        }
    }

    /// Its keys, for the hint bar.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Dialog::Form(_) => &[
                ("⏎", "confirm"),
                ("Tab", "next field"),
                ("←→", "choose"),
                ("Esc", "cancel"),
            ],
            Dialog::Choice(_) => &[("⏎", "choose"), ("↑↓", "move"), ("Esc", "cancel")],
            Dialog::Confirm(_) => &[("y", "yes"), ("n", "no"), ("Esc", "cancel")],
            Dialog::Message(_) => &[("↑↓", "scroll"), ("⏎", "close")],
        }
    }

    /// A click at `position`, a double one with `double`: in a choice list, selects
    /// the option there, and a double click chooses it; in a form, focuses the
    /// field. A confirmation takes no clicks, so that none answers it by mistake.
    pub fn click(&mut self, position: Position, double: bool) -> Outcome {
        match self {
            Dialog::Form(form) => form.click(position),
            Dialog::Choice(choice) => choice.click(position, double),
            Dialog::Confirm(_) | Dialog::Message(_) => Outcome::Open,
        }
    }

    /// Draws it over the middle of `area`, the whole screen.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        match self {
            Dialog::Form(form) => form.render(frame, area, theme),
            Dialog::Choice(choice) => choice.render(frame, area, theme),
            Dialog::Confirm(confirm) => confirm.render(frame, area, theme),
            Dialog::Message(message) => message.render(frame, area, theme),
        }
    }
}

/// A one-line text editor with a cursor, and the editing keys of a shell.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    text: String,
    /// In characters, from 0 to the text's length.
    cursor: usize,
}

impl Input {
    /// `text`, with the cursor at its end.
    pub fn new(text: &str) -> Input {
        Input {
            text: text.to_string(),
            cursor: text.chars().count(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Applies an editing key; false when it is not one, so the caller can use it.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.text.chars().count();
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char('u') if ctrl => {
                self.remove(0, self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => self.remove(self.cursor, len),
            KeyCode::Char('w') if ctrl => {
                let start = self.word_start();
                self.remove(start, self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace if self.cursor > 0 => {
                self.remove(self.cursor - 1, self.cursor);
                self.cursor -= 1;
            }
            KeyCode::Delete => self.remove(self.cursor, (self.cursor + 1).min(len)),
            // Nothing to delete is still an editing key
            KeyCode::Backspace => {}
            _ => return false,
        }
        true
    }

    /// The byte offset of character `index`.
    fn byte(&self, index: usize) -> usize {
        let mut offsets = self.text.char_indices().map(|(at, _)| at);
        offsets.nth(index).unwrap_or(self.text.len())
    }

    /// Removes characters `from..to`.
    fn remove(&mut self, from: usize, to: usize) {
        let (from, to) = (self.byte(from), self.byte(to));
        self.text.replace_range(from..to, "");
    }

    /// Where Ctrl-w deletes back to: past spaces, then past the word before them.
    fn word_start(&self) -> usize {
        let before: Vec<char> = self.text.chars().take(self.cursor).collect();
        let mut start = before.len();
        while start > 0 && before[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !before[start - 1].is_whitespace() {
            start -= 1;
        }
        start
    }

    /// The part that fits in `width` columns, scrolled to keep the cursor in view,
    /// and the cursor's column in it.
    fn visible(&self, width: usize) -> (String, usize) {
        let width = width.max(1);
        // The cursor may sit one past the last character
        let skip = (self.cursor + 1).saturating_sub(width);
        let shown = self.text.chars().skip(skip).take(width).collect();
        (shown, self.cursor - skip)
    }
}

pub enum Field {
    Text {
        label: String,
        input: Input,
    },
    Choice {
        label: String,
        options: Vec<String>,
        selected: usize,
    },
}

impl Field {
    pub fn text(label: &str, value: &str) -> Field {
        Field::Text {
            label: label.to_string(),
            input: Input::new(value),
        }
    }

    /// `options` with the one equal to `selected` chosen, else the first.
    pub fn choice(label: &str, options: Vec<String>, selected: &str) -> Field {
        let selected = options.iter().position(|o| o == selected).unwrap_or(0);
        Field::Choice {
            label: label.to_string(),
            options,
            selected,
        }
    }

    fn label(&self) -> &str {
        match self {
            Field::Text { label, .. } | Field::Choice { label, .. } => label,
        }
    }

    /// The text typed, or the option chosen.
    fn value(&self) -> &str {
        match self {
            Field::Text { input, .. } => input.text(),
            Field::Choice {
                options, selected, ..
            } => options.get(*selected).map_or("", String::as_str),
        }
    }
}

/// Fields one under the other, the first focused; Enter submits them all.
pub struct Form {
    title: String,
    fields: Vec<Field>,
    focus: usize,
    /// Why the last answer was refused; the form stays open meanwhile.
    error: Option<String>,
    /// What runs with the answer; only Esc works until it is done.
    busy: Option<String>,
    /// Where the fields were last drawn, a line each, to find the one clicked.
    inner: Rect,
}

impl Form {
    pub fn new(title: &str, fields: Vec<Field>) -> Form {
        Form {
            title: title.to_string(),
            fields,
            focus: 0,
            inner: Rect::default(),
            error: None,
            busy: None,
        }
    }

    /// The fields' values, in order.
    pub fn values(&self) -> Vec<&str> {
        self.fields.iter().map(Field::value).collect()
    }

    /// Shows why the answer cannot be used, e.g. not a repository.
    pub fn set_error(&mut self, error: &str) {
        self.error = Some(error.to_string());
    }

    /// Shows that the answer is being used, or with `None`, that it no longer is.
    pub fn set_busy(&mut self, busy: Option<&str>) {
        self.busy = busy.map(String::from);
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        if self.busy.is_some() {
            return match key.code {
                KeyCode::Esc => Outcome::Cancel,
                _ => Outcome::Open,
            };
        }
        let count = self.fields.len().max(1);
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter => return Outcome::Submit,
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % count,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + count - 1) % count,
            _ => match self.fields.get_mut(self.focus) {
                Some(Field::Text { input, .. }) => {
                    input.key(key);
                }
                Some(Field::Choice {
                    options, selected, ..
                }) => {
                    let n = options.len().max(1);
                    match key.code {
                        KeyCode::Right | KeyCode::Char(' ') => *selected = (*selected + 1) % n,
                        KeyCode::Left => *selected = (*selected + n - 1) % n,
                        _ => {}
                    }
                }
                None => {}
            },
        }
        // Editing is an answer to the error: it goes until the next submit
        self.error = None;
        Outcome::Open
    }

    fn click(&mut self, position: Position) -> Outcome {
        let row = position.y.wrapping_sub(self.inner.y) as usize;
        if self.busy.is_none() && self.inner.contains(position) && row < self.fields.len() {
            self.focus = row;
        }
        Outcome::Open
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let label_width = self.fields.iter().map(|f| f.label().chars().count()).max();
        let label_width = label_width.unwrap_or(0);
        let error_lines = self.error.as_ref().map_or(0, |e| e.lines().count() + 1);
        let busy_lines = if self.busy.is_some() { 2 } else { 0 };
        let height = self.fields.len() + error_lines + busy_lines;
        let inner = open(frame, area, &self.title, height as u16, theme);
        self.inner = inner;
        let value_width = (inner.width as usize).saturating_sub(label_width + 2);

        let mut lines = Vec::new();
        for (i, field) in self.fields.iter().enumerate() {
            let focused = i == self.focus;
            let style = match focused {
                true => Style::new().add_modifier(Modifier::BOLD),
                false => Style::new(),
            };
            let label = format!("{:label_width$}  ", field.label());
            let value = match field {
                Field::Text { input, .. } => {
                    let (shown, cursor) = input.visible(value_width);
                    if focused {
                        let x = inner.x + (label_width + 2 + cursor) as u16;
                        frame.set_cursor_position(Position::new(x, inner.y + i as u16));
                    }
                    shown
                }
                Field::Choice { .. } if focused => format!("◂ {} ▸", field.value()),
                Field::Choice { .. } => format!("  {}", field.value()),
            };
            lines.push(Line::from(vec![
                Span::styled(label, style),
                Span::raw(value),
            ]));
        }
        if let Some(error) = &self.error {
            lines.push(Line::raw(""));
            let red = Style::new().fg(theme.error);
            lines.extend(
                error
                    .lines()
                    .map(|line| Line::styled(line.to_string(), red)),
            );
        }
        if let Some(busy) = &self.busy {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                busy.clone(),
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// One of a list of options, chosen with the arrows.
pub struct Choice {
    title: String,
    options: Vec<String>,
    list: ListState,
    /// Where the options were last drawn, to find the one clicked.
    inner: Rect,
}

impl Choice {
    /// `options` with the one equal to `selected` chosen, else the first.
    pub fn new(title: &str, options: Vec<String>, selected: &str) -> Choice {
        let selected = options.iter().position(|o| o == selected).unwrap_or(0);
        Choice {
            title: title.to_string(),
            options,
            list: ListState::default().with_selected(Some(selected)),
            inner: Rect::default(),
        }
    }

    /// The option chosen; `None` when there are none.
    pub fn selected(&self) -> Option<&str> {
        let index = self.list.selected()?;
        self.options.get(index).map(String::as_str)
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        let last = self.options.len().saturating_sub(1);
        let selected = self.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter if !self.options.is_empty() => return Outcome::Submit,
            KeyCode::Up | KeyCode::Char('k') => self.list.select(Some(selected.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => self.list.select(Some((selected + 1).min(last))),
            _ => {}
        }
        Outcome::Open
    }

    fn click(&mut self, position: Position, double: bool) -> Outcome {
        if !self.inner.contains(position) {
            return Outcome::Open;
        }
        let heights = vec![1; self.options.len()];
        let Some(index) = super::row_at(&heights, self.list.offset(), self.inner, position.y)
        else {
            return Outcome::Open;
        };
        self.list.select(Some(index));
        match double {
            true => Outcome::Submit,
            false => Outcome::Open,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Up to half the screen; the list scrolls past that
        let height = self.options.len().min(area.height as usize / 2).max(1);
        let inner = open(frame, area, &self.title, height as u16, theme);
        self.inner = inner;
        let items: Vec<ListItem> = self
            .options
            .iter()
            .map(|o| ListItem::new(o.as_str()))
            .collect();
        let list = List::new(items).highlight_style(theme.selected());
        frame.render_stateful_widget(list, inner, &mut self.list);
    }
}

/// A question answered with `y` or `n`; Enter takes the default.
pub struct Confirm {
    title: String,
    /// Shown above the question as printed, e.g. why git refused.
    detail: Option<String>,
    question: String,
    default: bool,
}

impl Confirm {
    pub fn new(title: &str, question: &str, default: bool) -> Confirm {
        Confirm {
            title: title.to_string(),
            detail: None,
            question: question.to_string(),
            default,
        }
    }

    /// With `detail` above the question, keeping its lines.
    pub fn with_detail(mut self, detail: &str) -> Confirm {
        self.detail = Some(detail.trim_end().to_string());
        self
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        let answer = match key.code {
            KeyCode::Char('y' | 'Y') => true,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => false,
            KeyCode::Enter => self.default,
            _ => return Outcome::Open,
        };
        match answer {
            true => Outcome::Submit,
            false => Outcome::Cancel,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let choices = if self.default { "[Y/n]" } else { "[y/N]" };
        let text = format!("{} {choices}", self.question);
        let width = dialog_width(area) as usize - 2;
        let mut lines = Vec::new();
        if let Some(detail) = &self.detail {
            lines.extend(wrap(detail, width));
            lines.push(String::new());
        }
        lines.extend(wrap_words(&text, width));
        let inner = open(frame, area, &self.title, lines.len() as u16, theme);
        let lines: Vec<Line> = lines.into_iter().map(Line::raw).collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// Text to read, e.g. git's message, scrolled when longer than the screen.
pub struct Message {
    title: String,
    text: String,
    /// The first line shown.
    scroll: usize,
    /// Lines shown at the last draw, for paging.
    page: usize,
}

impl Message {
    pub fn new(title: &str, text: &str) -> Message {
        Message {
            title: title.to_string(),
            text: text.trim_end().to_string(),
            scroll: 0,
            page: 1,
        }
    }

    fn key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => return Outcome::Cancel,
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll += 1,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll += self.page,
            KeyCode::Home => self.scroll = 0,
            // Kept within the text when drawn
            KeyCode::End => self.scroll = usize::MAX,
            _ => {}
        }
        Outcome::Open
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let width = dialog_width(area) as usize - 2;
        let lines = wrap(&self.text, width);
        // Up to the screen's height less a margin, borders and a line for the position
        let room = (area.height as usize).saturating_sub(6).max(1);
        self.page = lines.len().min(room);
        self.scroll = self.scroll.min(lines.len() - self.page);
        let more = lines.len() > self.page;
        let inner = open(
            frame,
            area,
            &self.title,
            (self.page + more as usize) as u16,
            theme,
        );
        let mut shown: Vec<Line> = lines[self.scroll..self.scroll + self.page]
            .iter()
            .map(|line| Line::raw(line.clone()))
            .collect();
        if more {
            let end = self.scroll + self.page;
            let position = format!("{}–{} of {}", self.scroll + 1, end, lines.len());
            shown.push(Line::styled(
                position,
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        frame.render_widget(Paragraph::new(shown), inner);
    }
}

fn dialog_width(area: Rect) -> u16 {
    area.width
        .saturating_sub(4)
        .clamp(10, MAX_WIDTH)
        .min(area.width)
}

/// Clears a bordered box for `height` lines of content in the middle of `area`,
/// titled, in the theme's colours, and returns the space inside it.
fn open(frame: &mut Frame, area: Rect, title: &str, height: u16, theme: &Theme) -> Rect {
    let width = dialog_width(area);
    let height = (height + 2).min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    let rect = Rect::new(x, y, width, height);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .style(theme.base())
        .border_style(Style::new().add_modifier(Modifier::BOLD));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

/// Prose in lines of at most `width` characters, broken between words; a word
/// longer than a line is cut.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let needed =
                line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
            if !line.is_empty() && needed > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.extend(wrap(&line, width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// `text` cut into lines of at most `width` characters, each of its own lines
/// wrapped on its own and nothing else changed, so command output keeps its shape.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            lines.push(String::new());
        }
        lines.extend(chars.chunks(width).map(|chunk| chunk.iter().collect()));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(input: &mut Input, text: &str) {
        for c in text.chars() {
            assert!(input.key(key(KeyCode::Char(c))));
        }
    }

    /// The input's text with `|` at the cursor.
    fn shown(input: &Input) -> String {
        let mut text: Vec<char> = input.text.chars().collect();
        text.insert(input.cursor, '|');
        text.into_iter().collect()
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut input = Input::new("");
        typed(&mut input, "fix-login");
        assert_eq!(shown(&input), "fix-login|");
        for _ in 0..6 {
            input.key(key(KeyCode::Left));
        }
        typed(&mut input, "ed");
        assert_eq!(shown(&input), "fixed|-login");
        // Shift is part of the character
        input.key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
        assert_eq!(shown(&input), "fixedX|-login");
    }

    #[test]
    fn moving_stays_within_the_text() {
        let mut input = Input::new("ab");
        input.key(key(KeyCode::Right));
        assert_eq!(shown(&input), "ab|");
        input.key(key(KeyCode::Home));
        input.key(key(KeyCode::Left));
        assert_eq!(shown(&input), "|ab");
        input.key(ctrl('e'));
        assert_eq!(shown(&input), "ab|");
        input.key(ctrl('a'));
        assert_eq!(shown(&input), "|ab");
        input.key(key(KeyCode::End));
        assert_eq!(shown(&input), "ab|");
    }

    #[test]
    fn deleting_like_a_shell() {
        let mut input = Input::new("feature/new login");
        input.key(key(KeyCode::Backspace));
        assert_eq!(shown(&input), "feature/new logi|");
        input.key(ctrl('w'));
        assert_eq!(shown(&input), "feature/new |");
        input.key(ctrl('w'));
        assert_eq!(shown(&input), "|", "spaces, then the word before them");

        let mut input = Input::new("abcdef");
        for _ in 0..3 {
            input.key(key(KeyCode::Left));
        }
        input.key(key(KeyCode::Delete));
        assert_eq!(shown(&input), "abc|ef");
        input.key(ctrl('k'));
        assert_eq!(shown(&input), "abc|");
        input.key(key(KeyCode::Left));
        input.key(ctrl('u'));
        assert_eq!(shown(&input), "|c");
        // Nothing to delete: still handled, nothing changes
        assert!(input.key(key(KeyCode::Backspace)));
        assert_eq!(shown(&input), "|c");
    }

    #[test]
    fn editing_works_on_characters_not_bytes() {
        let mut input = Input::new("café");
        input.key(key(KeyCode::Left));
        typed(&mut input, "ü");
        input.key(key(KeyCode::Backspace));
        input.key(key(KeyCode::Backspace));
        assert_eq!(shown(&input), "ca|é");
        input.key(ctrl('w'));
        assert_eq!(shown(&input), "|é");
    }

    #[test]
    fn other_keys_are_left_to_the_caller() {
        let mut input = Input::new("a");
        for other in [
            key(KeyCode::Enter),
            key(KeyCode::Esc),
            key(KeyCode::Tab),
            ctrl('x'),
        ] {
            assert!(!input.key(other), "{other:?}");
        }
        let alt = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT);
        assert!(!input.key(alt));
        assert_eq!(input.text(), "a");
    }

    #[test]
    fn a_long_text_scrolls_to_keep_the_cursor_in_view() {
        let mut input = Input::new("abcdefghij");
        assert_eq!(input.visible(5), ("ghij".to_string(), 4));
        input.key(key(KeyCode::Home));
        assert_eq!(input.visible(5), ("abcde".to_string(), 0));
    }

    /// What `dialog` draws on a `width`×`height` screen, styles left out.
    fn screen(dialog: &mut Dialog, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| dialog.render(frame, frame.area(), &crate::theme::terminal()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_string()
            })
            .filter(|line| !line.is_empty())
            .collect()
    }

    fn new_worktree() -> Form {
        let agents = vec![
            "claude".to_string(),
            "codex".to_string(),
            "shell".to_string(),
        ];
        Form::new(
            "New worktree in my-app",
            vec![
                Field::text("Name", ""),
                Field::text("Base", "main"),
                Field::choice("Agent", agents, "codex"),
            ],
        )
    }

    #[test]
    fn a_form_shows_its_fields() {
        let mut form = Dialog::Form(new_worktree());
        let expected = [
            "  ┌ New worktree in my-app ──────────────┐",
            "  │Name                                  │",
            "  │Base   main                           │",
            "  │Agent    codex                        │",
            "  └──────────────────────────────────────┘",
        ];
        assert_eq!(screen(&mut form, 44, 9), expected);
    }

    #[test]
    fn a_form_moves_between_fields_and_submits_their_values() {
        let mut form = new_worktree();
        for c in "fix".chars() {
            assert_eq!(form.key(key(KeyCode::Char(c))), Outcome::Open);
        }
        form.key(key(KeyCode::Tab));
        form.key(key(KeyCode::Tab));
        form.key(key(KeyCode::Right));
        assert_eq!(form.values(), ["fix", "main", "shell"]);
        form.key(key(KeyCode::Right));
        assert_eq!(form.values()[2], "claude", "round");
        form.key(key(KeyCode::Up));
        form.key(ctrl('u'));
        assert_eq!(form.values(), ["fix", "", "claude"]);
        form.key(key(KeyCode::BackTab));
        form.key(key(KeyCode::BackTab));
        assert_eq!(form.focus, 2, "round");
        assert_eq!(form.key(key(KeyCode::Enter)), Outcome::Submit);
        assert_eq!(form.key(key(KeyCode::Esc)), Outcome::Cancel);
    }

    #[test]
    fn a_form_shows_an_error_until_the_next_edit() {
        let mut form = new_worktree();
        form.set_error("a branch named 'fix' already exists");
        let mut dialog = Dialog::Form(form);
        let lines = screen(&mut dialog, 60, 12);
        assert_eq!(
            lines[5],
            "  │a branch named 'fix' already exists                   │"
        );
        dialog.key(key(KeyCode::Char('x')));
        let lines = screen(&mut dialog, 60, 12);
        assert!(
            !lines.iter().any(|l| l.contains("already exists")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_choice_list_moves_and_chooses() {
        let options = vec!["claude".to_string(), "codex".to_string()];
        let mut choice = Choice::new("Agent", options, "claude");
        assert_eq!(choice.key(key(KeyCode::Char('j'))), Outcome::Open);
        assert_eq!(choice.key(key(KeyCode::Down)), Outcome::Open);
        assert_eq!(choice.selected(), Some("codex"), "stops at the last");
        let mut dialog = Dialog::Choice(choice);
        assert_eq!(
            screen(&mut dialog, 30, 8),
            [
                "  ┌ Agent ─────────────────┐",
                "  │claude                  │",
                "  │codex                   │",
                "  └────────────────────────┘",
            ]
        );
        assert_eq!(dialog.key(key(KeyCode::Enter)), Outcome::Submit);
        assert!(Choice::new("None", vec![], "x").selected().is_none());
        let preselected = Choice::new("Agent", vec!["a".into(), "b".into()], "b");
        assert_eq!(preselected.selected(), Some("b"));
    }

    #[test]
    fn a_confirmation_takes_y_n_and_its_default() {
        let mut remove = Confirm::new("Remove", "Remove ~/code/app.worktrees/fix?", false);
        assert_eq!(remove.key(key(KeyCode::Enter)), Outcome::Cancel);
        assert_eq!(remove.key(key(KeyCode::Char('x'))), Outcome::Open);
        assert_eq!(remove.key(key(KeyCode::Char('y'))), Outcome::Submit);
        assert_eq!(remove.key(key(KeyCode::Esc)), Outcome::Cancel);
        let mut restart = Confirm::new("Restart", "Restart the agent?", true);
        assert_eq!(restart.key(key(KeyCode::Enter)), Outcome::Submit);
        assert_eq!(restart.key(key(KeyCode::Char('n'))), Outcome::Cancel);

        let mut dialog = Dialog::Confirm(remove);
        assert_eq!(
            screen(&mut dialog, 34, 8),
            [
                "  ┌ Remove ────────────────────┐",
                "  │Remove                      │",
                "  │~/code/app.worktrees/fix?   │",
                "  │[y/N]                       │",
                "  └────────────────────────────┘",
            ]
        );
    }

    #[test]
    fn a_message_keeps_its_lines_and_scrolls() {
        let text = "error: 'fix' contains modified or untracked files, use --force to delete it\n\nhint: commit first\n";
        let mut message = Dialog::Message(Message::new("git worktree remove", text));
        assert_eq!(
            screen(&mut message, 44, 12),
            [
                "  ┌ git worktree remove ─────────────────┐",
                "  │error: 'fix' contains modified or untr│",
                "  │acked files, use --force to delete it │",
                "  │                                      │",
                "  │hint: commit first                    │",
                "  └──────────────────────────────────────┘",
            ]
        );
        // Too long for the screen: a page at a time, with where it is
        let long: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
        let mut message = Dialog::Message(Message::new("Log", &long.join("\n")));
        let lines = screen(&mut message, 30, 10);
        assert_eq!(lines[1], "  │line 1                  │");
        assert_eq!(lines[5], "  │1–4 of 20               │");
        message.key(key(KeyCode::PageDown));
        message.key(key(KeyCode::Down));
        let lines = screen(&mut message, 30, 10);
        assert_eq!(lines[1], "  │line 6                  │");
        message.key(key(KeyCode::End));
        let lines = screen(&mut message, 30, 10);
        assert_eq!(
            lines[4], "  │line 20                 │",
            "no further than the end"
        );
        assert_eq!(message.key(key(KeyCode::Char('q'))), Outcome::Cancel);
    }

    #[test]
    fn prose_wraps_between_words() {
        assert_eq!(wrap_words("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap_words("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_words("", 4), [""]);
    }
}
