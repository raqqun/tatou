//! A two-level list folded by parent, as both left panes are: projects and their
//! worktrees, sessions and their windows. It keeps the folding, the selection,
//! moving and clicks; each pane keeps its data and draws its rows.

use std::collections::HashSet;
use std::hash::Hash;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::widgets::{Block, List, ListItem, ListState};

use crate::theme::Theme;

/// A row, by keys, so the selection follows it across refreshes. A child may
/// share its parent's key: a project and its main worktree share a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row<K> {
    Parent(K),
    Child { parent: K, child: K },
}

/// Each parent's key and its children's, in the order they are drawn.
pub type Groups<K> = [(K, Vec<K>)];

pub struct Fold<K> {
    /// Every parent starts unfolded.
    collapsed: HashSet<K>,
    selected: Option<Row<K>>,
    /// The selected row's index, kept for when that row goes away.
    index: usize,
    /// Keeps the scroll position from one frame to the next.
    list: ListState,
    /// Where the rows were last drawn, to find the one clicked.
    inner: Rect,
}

impl<K> Default for Fold<K> {
    fn default() -> Fold<K> {
        Fold {
            collapsed: HashSet::new(),
            selected: None,
            index: 0,
            list: ListState::default(),
            inner: Rect::default(),
        }
    }
}

impl<K: Clone + Eq + Hash> Fold<K> {
    pub fn selected(&self) -> Option<&Row<K>> {
        self.selected.as_ref()
    }

    pub fn is_collapsed(&self, parent: &K) -> bool {
        self.collapsed.contains(parent)
    }

    /// The rows shown: every parent, and the children of those unfolded.
    pub fn rows(&self, groups: &Groups<K>) -> Vec<Row<K>> {
        let mut rows = Vec::new();
        for (parent, children) in groups {
            rows.push(Row::Parent(parent.clone()));
            if !self.collapsed.contains(parent) {
                rows.extend(children.iter().map(|child| Row::Child {
                    parent: parent.clone(),
                    child: child.clone(),
                }));
            }
        }
        rows
    }

    pub fn up(&mut self, groups: &Groups<K>) {
        self.select_index(self.index.saturating_sub(1), groups);
    }

    pub fn down(&mut self, groups: &Groups<K>) {
        self.select_index(self.index + 1, groups);
    }

    /// Folds or unfolds the selected parent.
    pub fn toggle(&mut self) {
        if let Some(Row::Parent(parent)) = &self.selected
            && !self.collapsed.remove(parent)
        {
            self.collapsed.insert(parent.clone());
        }
    }

    /// On an unfolded parent, folds it; on a child, moves to its parent.
    pub fn collapse(&mut self, groups: &Groups<K>) {
        match self.selected.clone() {
            Some(Row::Parent(parent)) => {
                self.collapsed.insert(parent);
            }
            Some(Row::Child { parent, .. }) => {
                self.selected = Some(Row::Parent(parent));
                self.reselect(groups);
            }
            None => {}
        }
    }

    /// On a folded parent, unfolds it; on an unfolded one, moves to its first child.
    pub fn expand(&mut self, groups: &Groups<K>) {
        let Some(Row::Parent(parent)) = &self.selected else {
            return;
        };
        if !self.collapsed.remove(parent)
            && let Some(Row::Child { .. }) = self.rows(groups).get(self.index + 1)
        {
            self.select_index(self.index + 1, groups);
        }
    }

    /// Selects `row`, unfolding its parent when it is a child.
    pub fn select(&mut self, row: Row<K>, groups: &Groups<K>) {
        if let Row::Child { parent, .. } = &row {
            self.collapsed.remove(parent);
        }
        self.selected = Some(row);
        self.reselect(groups);
    }

    /// Finds the selected row again after the rows changed; when it is gone, the
    /// row now at its place is selected.
    pub fn reselect(&mut self, groups: &Groups<K>) {
        let rows = self.rows(groups);
        match rows
            .iter()
            .position(|row| Some(row) == self.selected.as_ref())
        {
            Some(index) => self.index = index,
            None => self.select_index(self.index, groups),
        }
    }

    /// Selects the row at `index`, or the last one past the end.
    fn select_index(&mut self, index: usize, groups: &Groups<K>) {
        let rows = self.rows(groups);
        self.index = index.min(rows.len().saturating_sub(1));
        self.selected = rows.get(self.index).cloned();
    }

    /// Selects the row drawn at (`x`, `y`), each row taking its line count in
    /// `heights`; whether that was on a parent's fold arrow, or `None` when no
    /// row is there.
    pub fn click(&mut self, groups: &Groups<K>, heights: &[u16], x: u16, y: u16) -> Option<bool> {
        if !self.inner.contains(Position::new(x, y)) {
            return None;
        }
        let index = super::row_at(heights, self.list.offset(), self.inner, y)?;
        self.select_index(index, groups);
        let parent = matches!(self.selected, Some(Row::Parent(_)));
        Some(parent && x < self.inner.x + 2)
    }

    /// Draws `items`, a row each, in `block`, the selected one highlighted.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        block: Block,
        items: Vec<ListItem<'static>>,
        theme: &Theme,
    ) {
        self.inner = block.inner(area);
        let list = List::new(items)
            .block(block)
            .highlight_style(theme.selected());
        self.list.select(self.selected.as_ref().map(|_| self.index));
        frame.render_stateful_widget(list, area, &mut self.list);
    }

    /// Remembers where the rows go without drawing them, for a pane drawn as
    /// something else (an empty list's message).
    pub fn place(&mut self, inner: Rect) {
        self.inner = inner;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `a` with children `a1`, `a2`; `b` with none; `c` with `c1`.
    fn groups() -> Vec<(&'static str, Vec<&'static str>)> {
        vec![("a", vec!["a1", "a2"]), ("b", vec![]), ("c", vec!["c1"])]
    }

    fn child(parent: &'static str, child: &'static str) -> Row<&'static str> {
        Row::Child { parent, child }
    }

    #[test]
    fn moves_over_parents_and_unfolded_children_and_stops_at_the_ends() {
        let groups = groups();
        let mut fold = Fold::default();
        fold.reselect(&groups);
        assert_eq!(fold.selected(), Some(&Row::Parent("a")));
        fold.up(&groups);
        assert_eq!(fold.selected(), Some(&Row::Parent("a")), "the top");
        let mut seen = Vec::new();
        for _ in 0..6 {
            fold.down(&groups);
            seen.push(fold.selected().cloned().unwrap());
        }
        let expected = [
            child("a", "a1"),
            child("a", "a2"),
            Row::Parent("b"),
            Row::Parent("c"),
            child("c", "c1"),
            child("c", "c1"),
        ];
        assert_eq!(seen, expected, "the last row stays selected");
    }

    #[test]
    fn folds_unfolds_and_steps_between_a_parent_and_its_children() {
        let groups = groups();
        let mut fold = Fold::default();
        fold.reselect(&groups);
        fold.collapse(&groups);
        assert!(fold.is_collapsed(&"a"));
        assert_eq!(fold.rows(&groups).len(), 4, "a's children hidden");
        fold.expand(&groups);
        assert!(!fold.is_collapsed(&"a"));
        assert_eq!(fold.selected(), Some(&Row::Parent("a")), "unfolding stays");
        fold.expand(&groups);
        assert_eq!(
            fold.selected(),
            Some(&child("a", "a1")),
            "into the first child"
        );
        fold.collapse(&groups);
        assert_eq!(
            fold.selected(),
            Some(&Row::Parent("a")),
            "back to its parent"
        );
        fold.toggle();
        assert!(fold.is_collapsed(&"a"));
        fold.toggle();
        assert!(!fold.is_collapsed(&"a"));

        fold.down(&groups);
        fold.down(&groups);
        fold.down(&groups);
        assert_eq!(fold.selected(), Some(&Row::Parent("b")));
        fold.expand(&groups);
        assert_eq!(
            fold.selected(),
            Some(&Row::Parent("b")),
            "no child to go to"
        );
    }

    #[test]
    fn selecting_a_child_unfolds_its_parent_and_a_gone_row_gives_its_place() {
        let groups = groups();
        let mut fold = Fold::default();
        fold.reselect(&groups);
        fold.toggle();
        fold.select(child("a", "a2"), &groups);
        assert!(!fold.is_collapsed(&"a"));
        assert_eq!(fold.rows(&groups)[2], child("a", "a2"));

        let without_a2 = vec![("a", vec!["a1"]), ("b", vec![]), ("c", vec!["c1"])];
        fold.reselect(&without_a2);
        assert_eq!(
            fold.selected(),
            Some(&Row::Parent("b")),
            "the row now at its place"
        );
    }

    #[test]
    fn a_click_selects_the_row_drawn_there_and_tells_a_parents_arrow() {
        let groups = groups();
        let mut fold = Fold::default();
        fold.place(Rect::new(1, 1, 20, 10));
        // `a` takes two lines, as a project with an error does
        let heights = [2, 1, 1, 1, 1, 1];
        assert_eq!(fold.click(&groups, &heights, 1, 2), Some(true), "a's arrow");
        assert_eq!(fold.selected(), Some(&Row::Parent("a")));
        assert_eq!(fold.click(&groups, &heights, 5, 3), Some(false));
        assert_eq!(fold.selected(), Some(&child("a", "a1")));
        assert_eq!(fold.click(&groups, &heights, 1, 4), Some(false), "a child");
        assert_eq!(fold.click(&groups, &heights, 5, 9), None, "below the rows");
        assert_eq!(fold.click(&groups, &heights, 30, 3), None, "outside");
    }
}
