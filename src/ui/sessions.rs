//! The sessions' pane, below the projects: every session on tatou's server, and
//! under each, its windows.

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use super::projects::status_icon;
use crate::model::SessionRow;

/// A row of the pane, by ids, so the selection follows it across refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Session(String),
    Window { session: String, window: String },
}

#[derive(Default)]
pub struct Sessions {
    sessions: Vec<SessionRow>,
    /// By session id; every session starts expanded.
    collapsed: HashSet<String>,
    selected: Option<Row>,
    /// The selected row's index, kept for when that row goes away.
    index: usize,
    /// Keeps the scroll position from one frame to the next.
    list: ListState,
}

impl Sessions {
    pub fn set_rows(&mut self, sessions: Vec<SessionRow>) {
        self.sessions = sessions;
        self.reselect();
    }

    /// How many rows show: the sessions, and the windows of those unfolded.
    pub fn len(&self) -> usize {
        self.rows().len()
    }

    pub fn selected(&self) -> Option<&Row> {
        self.selected.as_ref()
    }

    /// The pane Enter shows: the selected window's active pane, or on a session,
    /// its current window's.
    pub fn selected_pane(&self) -> Option<String> {
        let (session, window) = match self.selected.as_ref()? {
            Row::Session(id) => (id, None),
            Row::Window { session, window } => (session, Some(window)),
        };
        let session = self.sessions.iter().find(|s| &s.id == session)?;
        let window = match window {
            Some(id) => session.windows.iter().find(|w| &w.id == id),
            None => session.windows.iter().find(|w| w.current),
        };
        window.map(|w| w.pane.clone())
    }

    pub fn up(&mut self) {
        self.select(self.index.saturating_sub(1));
    }

    pub fn down(&mut self) {
        self.select(self.index + 1);
    }

    /// On an unfolded session, folds it; on a window, moves to its session.
    pub fn collapse(&mut self) {
        match self.selected.clone() {
            Some(Row::Session(id)) => {
                self.collapsed.insert(id);
            }
            Some(Row::Window { session, .. }) => {
                self.selected = Some(Row::Session(session));
                self.reselect();
            }
            None => {}
        }
    }

    /// On a folded session, unfolds it; on an unfolded one, moves to its first window.
    pub fn expand(&mut self) {
        let Some(Row::Session(id)) = &self.selected else {
            return;
        };
        if !self.collapsed.remove(id)
            && let Some(Row::Window { .. }) = self.rows().get(self.index + 1)
        {
            self.select(self.index + 1);
        }
    }

    /// Selects the row at `index`, or the last one past the end.
    fn select(&mut self, index: usize) {
        let rows = self.rows();
        self.index = index.min(rows.len().saturating_sub(1));
        self.selected = rows.get(self.index).cloned();
    }

    /// Finds the selected row again after the rows changed; when it is gone, the
    /// row now in its place is selected.
    fn reselect(&mut self) {
        let rows = self.rows();
        match rows
            .iter()
            .position(|row| Some(row) == self.selected.as_ref())
        {
            Some(index) => self.index = index,
            None => self.select(self.index),
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for session in &self.sessions {
            rows.push(Row::Session(session.id.clone()));
            if !self.collapsed.contains(&session.id) {
                rows.extend(session.windows.iter().map(|window| Row::Window {
                    session: session.id.clone(),
                    window: window.id.clone(),
                }));
            }
        }
        rows
    }

    /// Each session's fold, status icon, name, what runs in its current window,
    /// and what it is for, dimmed; then each window's index, what runs in it, and
    /// `*` on the session's current one.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, block: Block) {
        if self.sessions.is_empty() {
            frame.render_widget(Paragraph::new("No sessions").block(block), area);
            return;
        }
        let dim = Style::new().add_modifier(Modifier::DIM);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut items = Vec::new();
        for session in &self.sessions {
            let collapsed = self.collapsed.contains(&session.id);
            let (icon, style) = status_icon(session.status);
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
                let command = window.command.as_deref().unwrap_or("");
                let current = if window.current { " *" } else { "" };
                let line = format!("    {} {command}{current}", window.index);
                items.push(ListItem::new(line));
            }
        }
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        self.list.select(self.selected.as_ref().map(|_| self.index));
        frame.render_stateful_widget(list, area, &mut self.list);
    }
}
