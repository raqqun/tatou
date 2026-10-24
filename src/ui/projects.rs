//! The left pane: projects, collapsible, and their worktrees.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState};

use crate::model::{ProjectView, Status, WorktreeView};

/// A row of the pane. The selection is kept as one, so it follows its project or
/// worktree across refreshes; a project and its main worktree share a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Project(PathBuf),
    Worktree { project: PathBuf, path: PathBuf },
}

#[derive(Default)]
pub struct Tree {
    view: Vec<ProjectView>,
    /// By project path; every project starts expanded.
    collapsed: HashSet<PathBuf>,
    selected: Option<Row>,
    /// The selected row's index, kept for when that row goes away.
    index: usize,
    /// Keeps the scroll position from one frame to the next.
    list: ListState,
}

impl Tree {
    pub fn set_view(&mut self, view: Vec<ProjectView>) {
        self.view = view;
        self.reselect();
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<&Row> {
        self.selected.as_ref()
    }

    /// The selected worktree; `None` on a project row.
    pub fn selected_worktree(&self) -> Option<&WorktreeView> {
        let Some(Row::Worktree { project, path }) = &self.selected else {
            return None;
        };
        let project = self.view.iter().find(|p| &p.path == project)?;
        project.worktrees.iter().find(|w| &w.path == path)
    }

    /// The selected row's project.
    pub fn selected_project(&self) -> Option<&ProjectView> {
        let path = match self.selected.as_ref()? {
            Row::Project(path) | Row::Worktree { project: path, .. } => path,
        };
        self.view.iter().find(|p| &p.path == path)
    }

    /// Selects the worktree at `path`, unfolding its project; false when it is not
    /// listed (yet).
    pub fn select_worktree(&mut self, path: &std::path::Path) -> bool {
        let project = self
            .view
            .iter()
            .find(|p| p.worktrees.iter().any(|w| w.path == path));
        let Some(project) = project.map(|p| p.path.clone()) else {
            return false;
        };
        self.collapsed.remove(&project);
        self.selected = Some(Row::Worktree {
            project,
            path: path.to_path_buf(),
        });
        self.reselect();
        true
    }

    /// Folds or unfolds the selected project.
    pub fn toggle(&mut self) {
        if let Some(Row::Project(path)) = &self.selected
            && !self.collapsed.remove(path)
        {
            self.collapsed.insert(path.clone());
        }
    }

    pub fn up(&mut self) {
        self.select(self.index.saturating_sub(1));
    }

    pub fn down(&mut self) {
        self.select(self.index + 1);
    }

    /// On an expanded project, collapses it; on a worktree, moves to its project.
    pub fn collapse(&mut self) {
        match self.selected.clone() {
            Some(Row::Project(path)) => {
                self.collapsed.insert(path);
            }
            Some(Row::Worktree { project, .. }) => {
                self.selected = Some(Row::Project(project));
                self.reselect();
            }
            None => {}
        }
    }

    /// On a collapsed project, expands it; on an expanded one, moves to its first worktree.
    pub fn expand(&mut self) {
        let Some(Row::Project(path)) = &self.selected else {
            return;
        };
        if !self.collapsed.remove(path)
            && let Some(Row::Worktree { .. }) = self.rows().get(self.index + 1)
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
    /// row now at its place is selected.
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
        for project in &self.view {
            rows.push(Row::Project(project.path.clone()));
            if !self.collapsed.contains(&project.path) {
                rows.extend(project.worktrees.iter().map(|worktree| Row::Worktree {
                    project: project.path.clone(),
                    path: worktree.path.clone(),
                }));
            }
        }
        rows
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, block: Block) {
        let mut items = Vec::new();
        for project in &self.view {
            let collapsed = self.collapsed.contains(&project.path);
            items.push(project_item(project, collapsed));
            if !collapsed {
                items.extend(project.worktrees.iter().map(worktree_item));
            }
        }
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        self.list.select(self.selected.as_ref().map(|_| self.index));
        frame.render_stateful_widget(list, area, &mut self.list);
    }
}

/// The project's name, then its error, if any, a line each.
fn project_item(project: &ProjectView, collapsed: bool) -> ListItem<'static> {
    let arrow = if collapsed { "▸" } else { "▾" };
    let mut lines = vec![Line::from(vec![
        Span::raw(format!("{arrow} ")),
        Span::styled(
            project.name.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ])];
    if let Some(error) = &project.error {
        let style = Style::new().fg(Color::Red);
        lines.extend(
            error
                .lines()
                .map(|line| Line::styled(format!("    {line}"), style)),
        );
    }
    ListItem::new(lines)
}

/// The status icon, then the worktree's label.
fn worktree_item(worktree: &WorktreeView) -> ListItem<'static> {
    let (icon, style) = status_icon(worktree.status);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(icon, style),
        Span::raw(format!(" {}", worktree.label())),
    ];
    if worktree.main {
        spans.push(Span::styled("  repo", Style::new().fg(Color::Cyan)));
    }
    ListItem::new(Line::from(spans))
}

/// Guessed statuses, from activity without hooks, are dim.
fn status_icon(status: Status) -> (&'static str, Style) {
    let dim = Style::new().add_modifier(Modifier::DIM);
    match status {
        Status::Busy => ("●", Style::new()),
        Status::Input => ("⚠", Style::new().fg(Color::Yellow)),
        Status::Idle => ("○", Style::new()),
        Status::Active => ("●", dim),
        Status::Quiet => ("○", dim),
        Status::NoSession => ("·", Style::new()),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn worktree(path: &str, branch: Option<&str>, status: Status) -> WorktreeView {
        WorktreeView {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            branch: branch.map(String::from),
            main: false,
            session: None,
            status,
        }
    }

    /// A project at `path` whose main worktree is on `main`, then `linked` worktrees.
    pub fn project(path: &str, linked: &[&str]) -> ProjectView {
        let main = WorktreeView {
            main: true,
            ..worktree(path, Some("main"), Status::NoSession)
        };
        let linked = linked.iter().map(|name| {
            let path = format!("{path}.worktrees/{name}");
            worktree(&path, Some(name), Status::NoSession)
        });
        ProjectView {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            error: None,
            worktrees: std::iter::once(main).chain(linked).collect(),
            remote_default: None,
        }
    }

    fn two_projects() -> Vec<ProjectView> {
        vec![
            project("/code/app", &["fix", "api"]),
            project("/code/web", &[]),
        ]
    }

    fn tree(view: Vec<ProjectView>) -> Tree {
        let mut tree = Tree::default();
        tree.set_view(view);
        tree
    }

    fn on(project: &str, path: &str) -> Option<Row> {
        Some(Row::Worktree {
            project: project.into(),
            path: path.into(),
        })
    }

    fn on_project(path: &str) -> Option<Row> {
        Some(Row::Project(path.into()))
    }

    #[test]
    fn starts_on_the_first_project_and_moves_over_every_row() {
        let mut tree = tree(two_projects());
        assert_eq!(tree.selected().cloned(), on_project("/code/app"));
        tree.down();
        assert_eq!(tree.selected().cloned(), on("/code/app", "/code/app"));
        for _ in 0..10 {
            tree.down();
        }
        assert_eq!(
            tree.selected().cloned(),
            on("/code/web", "/code/web"),
            "stops at the last"
        );
        for _ in 0..10 {
            tree.up();
        }
        assert_eq!(tree.selected().cloned(), on_project("/code/app"));
    }

    #[test]
    fn nothing_to_select_without_projects() {
        let mut tree = tree(vec![]);
        tree.down();
        tree.expand();
        tree.collapse();
        assert_eq!(tree.selected(), None);
    }

    #[test]
    fn collapse_moves_to_the_project_then_folds_it() {
        let mut tree = tree(two_projects());
        tree.down();
        tree.down();
        assert_eq!(
            tree.selected().cloned(),
            on("/code/app", "/code/app.worktrees/fix")
        );
        tree.collapse();
        assert_eq!(tree.selected().cloned(), on_project("/code/app"));
        tree.collapse();
        tree.down();
        assert_eq!(
            tree.selected().cloned(),
            on_project("/code/web"),
            "skips the folded worktrees"
        );
    }

    #[test]
    fn expand_unfolds_then_moves_to_the_first_worktree() {
        let mut tree = tree(two_projects());
        tree.collapse();
        tree.expand();
        assert_eq!(tree.selected().cloned(), on_project("/code/app"));
        assert_eq!(tree.rows().len(), 6);
        tree.expand();
        assert_eq!(tree.selected().cloned(), on("/code/app", "/code/app"));
        tree.expand();
        assert_eq!(tree.selected().cloned(), on("/code/app", "/code/app"));
    }

    #[test]
    fn expand_on_a_project_without_worktrees_stays() {
        let mut empty = project("/code/new", &[]);
        empty.worktrees.clear();
        let mut tree = tree(vec![empty, project("/code/web", &[])]);
        tree.expand();
        assert_eq!(tree.selected().cloned(), on_project("/code/new"));
    }

    #[test]
    fn the_selection_follows_its_worktree_across_refreshes() {
        let mut tree = tree(two_projects());
        tree.down();
        tree.down();
        tree.down();
        let api = on("/code/app", "/code/app.worktrees/api");
        assert_eq!(tree.selected().cloned(), api);
        // A worktree added above it moves it down a row
        tree.set_view(vec![
            project("/code/app", &["new", "fix", "api"]),
            project("/code/web", &[]),
        ]);
        assert_eq!(tree.selected().cloned(), api);
        assert_eq!(tree.index, 4);
    }

    #[test]
    fn a_removed_selection_moves_to_the_row_in_its_place() {
        let mut tree = tree(two_projects());
        tree.down();
        tree.down();
        tree.set_view(vec![
            project("/code/app", &["api"]),
            project("/code/web", &[]),
        ]);
        assert_eq!(
            tree.selected().cloned(),
            on("/code/app", "/code/app.worktrees/api")
        );
        tree.set_view(vec![]);
        assert_eq!(tree.selected(), None);
        tree.set_view(two_projects());
        assert_eq!(tree.selected().cloned(), on_project("/code/app"));
    }
}
