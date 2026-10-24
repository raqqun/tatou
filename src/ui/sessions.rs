//! The sessions' pane, below the projects: every session on tatou's server.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use super::projects::status_icon;
use crate::model::SessionRow;

#[derive(Default)]
pub struct Sessions {
    rows: Vec<SessionRow>,
    /// By session id, so it follows its session across refreshes.
    selected: Option<String>,
    /// The selected row's index, kept for when that session goes away.
    index: usize,
    /// Keeps the scroll position from one frame to the next.
    list: ListState,
}

impl Sessions {
    pub fn set_rows(&mut self, rows: Vec<SessionRow>) {
        self.rows = rows;
        match self
            .rows
            .iter()
            .position(|row| Some(&row.id) == self.selected.as_ref())
        {
            Some(index) => self.index = index,
            None => self.select(self.index),
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn selected(&self) -> Option<&SessionRow> {
        self.rows
            .get(self.index)
            .filter(|_| self.selected.is_some())
    }

    pub fn up(&mut self) {
        self.select(self.index.saturating_sub(1));
    }

    pub fn down(&mut self) {
        self.select(self.index + 1);
    }

    /// Selects the row at `index`, or the last one past the end.
    fn select(&mut self, index: usize) {
        self.index = index.min(self.rows.len().saturating_sub(1));
        self.selected = self.rows.get(self.index).map(|row| row.id.clone());
    }

    /// Each session's status icon, name, and what it is for, dimmed.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, block: Block) {
        if self.rows.is_empty() {
            frame.render_widget(Paragraph::new("No sessions").block(block), area);
            return;
        }
        let dim = Style::new().add_modifier(Modifier::DIM);
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| {
                let (icon, style) = status_icon(row.status);
                ListItem::new(Line::from(vec![
                    Span::styled(icon, style),
                    Span::raw(format!(" {}  ", row.name)),
                    Span::styled(row.place.clone(), dim),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        self.list.select(self.selected.as_ref().map(|_| self.index));
        frame.render_stateful_widget(list, area, &mut self.list);
    }
}
