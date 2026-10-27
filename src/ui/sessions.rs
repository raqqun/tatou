//! The sessions' pane, below the projects: every session the daemon has, and
//! under each, its terminals, which it calls windows.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, ListItem, Paragraph};

use super::projects::status_icon;
use super::tree::Fold;
use crate::actions::End;
use crate::model::SessionRow;
use crate::theme::Theme;

/// A row of the pane: a session's id, or a window's with its session's.
pub type Row = super::tree::Row<String>;

#[derive(Default)]
pub struct Sessions {
    sessions: Vec<SessionRow>,
    fold: Fold<String>,
}

impl Sessions {
    pub fn set_rows(&mut self, sessions: Vec<SessionRow>) {
        self.sessions = sessions;
        self.fold.reselect(&self.groups());
    }

    /// How many rows show: the sessions, and the windows of those unfolded.
    pub fn len(&self) -> usize {
        self.fold.rows(&self.groups()).len()
    }

    pub fn selected(&self) -> Option<&Row> {
        self.fold.selected()
    }

    /// What `d` would end, and how to ask about it: the selected window, or the
    /// selected session with all its windows.
    pub fn selected_end(&self) -> Option<(End, String)> {
        let (session, window) = match self.fold.selected()? {
            Row::Parent(id) => (id, None),
            Row::Child {
                parent: session,
                child: window,
            } => (session, Some(window)),
        };
        let session = self.sessions.iter().find(|s| &s.id == session)?;
        match window {
            Some(id) => {
                let window = session.windows.iter().find(|w| &w.id == id)?;
                let what = format!("{} in {}:{}", window.command, session.name, window.index);
                Some((End::Window(window.id.clone()), what))
            }
            None => {
                let programs: Vec<&str> =
                    session.windows.iter().map(|w| w.command.as_str()).collect();
                let what = match programs.len() {
                    1 => format!("{} and its window ({})", session.name, programs[0]),
                    n => format!(
                        "{} and its {n} windows ({})",
                        session.name,
                        programs.join(", ")
                    ),
                };
                Some((End::Session(session.id.clone()), what))
            }
        }
    }

    /// The terminal Enter shows: the selected window, or on a session, its
    /// current one.
    pub fn selected_terminal(&self) -> Option<String> {
        let (session, window) = match self.fold.selected()? {
            Row::Parent(id) => (id, None),
            Row::Child {
                parent: session,
                child: window,
            } => (session, Some(window)),
        };
        let session = self.sessions.iter().find(|s| &s.id == session)?;
        let window = match window {
            Some(id) => session.windows.iter().find(|w| &w.id == id),
            None => session.windows.iter().find(|w| w.current),
        };
        window.map(|w| w.id.clone())
    }

    /// The current terminal of the session `step` away from `terminal`'s, wrapping
    /// around: from none, the first session forward, the last backward. That
    /// session becomes the selected row.
    pub fn step(&mut self, terminal: Option<&str>, step: isize) -> Option<String> {
        let count = self.sessions.len() as isize;
        if count == 0 {
            return None;
        }
        let from = terminal.and_then(|terminal| {
            (self.sessions.iter()).position(|s| s.windows.iter().any(|w| w.id == terminal))
        });
        let index = match from {
            Some(from) => (from as isize + step).rem_euclid(count),
            None if step > 0 => 0,
            None => count - 1,
        } as usize;
        let session = &self.sessions[index];
        let window = session
            .windows
            .iter()
            .find(|w| w.current)
            .or(session.windows.first())?;
        let shown = window.id.clone();
        let row = Row::Parent(session.id.clone());
        self.fold.select(row, &self.groups());
        Some(shown)
    }

    /// The window `step` away from `terminal` in its session, wrapping around:
    /// from none, the first session's first window forward, its last backward.
    /// That window becomes the selected row.
    pub fn step_window(&mut self, terminal: Option<&str>, step: isize) -> Option<String> {
        let (session, from) = match terminal.and_then(|t| self.locate(t)) {
            Some((session, window)) => (session, Some(window)),
            None => (0, None),
        };
        let session = self.sessions.get(session)?;
        let count = session.windows.len() as isize;
        if count == 0 {
            return None;
        }
        let index = match from {
            Some(from) => (from as isize + step).rem_euclid(count),
            None if step > 0 => 0,
            None => count - 1,
        } as usize;
        let window = session.windows[index].id.clone();
        let row = Row::Child {
            parent: session.id.clone(),
            child: window.clone(),
        };
        self.fold.select(row, &self.groups());
        Some(window)
    }

    /// `terminal`'s session and window, by index.
    fn locate(&self, terminal: &str) -> Option<(usize, usize)> {
        self.sessions.iter().enumerate().find_map(|(i, session)| {
            let window = session.windows.iter().position(|w| w.id == terminal)?;
            Some((i, window))
        })
    }

    /// Forgets `terminal`, whose program ended, until the next refresh says so, and
    /// its session with its last terminal. Returns what to show instead: another
    /// terminal of its session, the one after it or else before; else the current
    /// one of the next session, or else of the previous one.
    pub fn ended(&mut self, terminal: &str) -> Option<String> {
        let index =
            (self.sessions.iter()).position(|s| s.windows.iter().any(|w| w.id == terminal))?;
        let session = &mut self.sessions[index];
        let i = session.windows.iter().position(|w| w.id == terminal)?;
        session.windows.remove(i);
        // The one after it took its place
        let sibling = session.windows.get(i).or_else(|| session.windows.last());
        if let Some(window) = sibling {
            return Some(window.id.clone());
        }
        self.sessions.remove(index);
        let current = |s: &SessionRow| {
            let window = s.windows.iter().find(|w| w.current).or(s.windows.first());
            window.map(|w| w.id.clone())
        };
        let next = self.sessions.get(index).and_then(current);
        let previous = || {
            index
                .checked_sub(1)
                .and_then(|i| self.sessions.get(i))
                .and_then(current)
        };
        let shown = next.or_else(previous);
        self.fold.reselect(&self.groups());
        shown
    }

    /// What the shown terminal's pane is titled: its session, the window's index
    /// and what runs in it, `app-fix:1 zsh`.
    pub fn title(&self, terminal: &str) -> String {
        for session in &self.sessions {
            if let Some(window) = session.windows.iter().find(|w| w.id == terminal) {
                return format!("{}:{} {}", session.name, window.index, window.command);
            }
        }
        terminal.to_string()
    }

    /// Selects the row drawn at (`x`, `y`); whether that was on a session's fold
    /// arrow, or `None` when no row is there.
    pub fn click(&mut self, x: u16, y: u16) -> Option<bool> {
        let heights = vec![1; self.len()];
        self.fold.click(&self.groups(), &heights, x, y)
    }

    /// Folds or unfolds the selected session.
    pub fn toggle(&mut self) {
        self.fold.toggle();
    }

    pub fn up(&mut self) {
        self.fold.up(&self.groups());
    }

    pub fn down(&mut self) {
        self.fold.down(&self.groups());
    }

    /// On an unfolded session, folds it; on a window, moves to its session.
    pub fn collapse(&mut self) {
        self.fold.collapse(&self.groups());
    }

    /// On a folded session, unfolds it; on an unfolded one, moves to its first window.
    pub fn expand(&mut self) {
        self.fold.expand(&self.groups());
    }

    /// Each session's id and its windows', as drawn.
    fn groups(&self) -> Vec<(String, Vec<String>)> {
        (self.sessions.iter())
            .map(|s| {
                (
                    s.id.clone(),
                    s.windows.iter().map(|w| w.id.clone()).collect(),
                )
            })
            .collect()
    }

    /// Each session's fold, status icon, name, what runs in its current window,
    /// and what it is for, dimmed; then each window's index, what runs in it, and
    /// `*` on the session's current one.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, block: Block, theme: &Theme) {
        if self.sessions.is_empty() {
            self.fold.place(block.inner(area));
            frame.render_widget(Paragraph::new("No sessions").block(block), area);
            return;
        }
        let dim = Style::new().add_modifier(Modifier::DIM);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut items = Vec::new();
        for session in &self.sessions {
            let collapsed = self.fold.is_collapsed(&session.id);
            let (icon, style) = status_icon(session.status, theme);
            let mut spans = vec![
                Span::raw(if collapsed { "▸ " } else { "▾ " }),
                Span::styled(icon, style),
                Span::raw(format!(" {}  ", session.name)),
            ];
            if let Some(command) = &session.command {
                spans.push(Span::styled(command.clone(), bold));
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(session.place.clone(), dim));
            items.push(ListItem::new(Line::from(spans)));
            if collapsed {
                continue;
            }
            for window in &session.windows {
                let command = &window.command;
                let current = if window.current { " *" } else { "" };
                let line = format!("    {} {command}{current}", window.index);
                items.push(ListItem::new(line));
            }
        }
        self.fold.render(frame, area, block, items, theme);
    }
}
