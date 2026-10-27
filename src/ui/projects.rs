//! The left pane: projects, collapsible, and their worktrees.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, ListItem};

use super::tree::Fold;
use crate::gh::{Checks, Pr, State};
use crate::model::{ProjectView, Status, WorktreeView};
use crate::theme::Theme;

/// A row of the pane: a project's path, or a worktree's with its project's.
pub type Row = super::tree::Row<PathBuf>;

#[derive(Default)]
pub struct Tree {
    view: Vec<ProjectView>,
    fold: Fold<PathBuf>,
}

impl Tree {
    pub fn set_view(&mut self, view: Vec<ProjectView>) {
        self.view = view;
        self.fold.reselect(&self.groups());
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<&Row> {
        self.fold.selected()
    }

    /// The selected worktree; `None` on a project row.
    pub fn selected_worktree(&self) -> Option<&WorktreeView> {
        let Some(Row::Child {
            parent: project,
            child: path,
        }) = self.fold.selected()
        else {
            return None;
        };
        let project = self.view.iter().find(|p| &p.path == project)?;
        project.worktrees.iter().find(|w| &w.path == path)
    }

    /// The selected row's project.
    pub fn selected_project(&self) -> Option<&ProjectView> {
        let path = match self.fold.selected()? {
            Row::Parent(path) | Row::Child { parent: path, .. } => path,
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
        let row = Row::Child {
            parent: project,
            child: path.to_path_buf(),
        };
        self.fold.select(row, &self.groups());
        true
    }

    /// Folds or unfolds the selected project.
    pub fn toggle(&mut self) {
        self.fold.toggle();
    }

    /// Selects the row drawn at (`x`, `y`); whether that was on a project's fold
    /// arrow, or `None` when no row is there.
    pub fn click(&mut self, x: u16, y: u16) -> Option<bool> {
        self.fold.click(&self.groups(), &self.heights(), x, y)
    }

    pub fn up(&mut self) {
        self.fold.up(&self.groups());
    }

    pub fn down(&mut self) {
        self.fold.down(&self.groups());
    }

    /// On an expanded project, collapses it; on a worktree, moves to its project.
    pub fn collapse(&mut self) {
        self.fold.collapse(&self.groups());
    }

    /// On a collapsed project, expands it; on an expanded one, moves to its first worktree.
    pub fn expand(&mut self) {
        self.fold.expand(&self.groups());
    }

    /// Each project's path and its worktrees', as drawn.
    fn groups(&self) -> Vec<(PathBuf, Vec<PathBuf>)> {
        (self.view.iter())
            .map(|p| {
                (
                    p.path.clone(),
                    p.worktrees.iter().map(|w| w.path.clone()).collect(),
                )
            })
            .collect()
    }

    /// How many lines each row takes, as `render` draws them: a project's error too.
    fn heights(&self) -> Vec<u16> {
        let mut heights = Vec::new();
        for project in &self.view {
            let error = project
                .error
                .as_ref()
                .map_or(0, |error| error.lines().count());
            heights.push(1 + error as u16);
            if !self.fold.is_collapsed(&project.path) {
                heights.extend(project.worktrees.iter().map(|_| 1));
            }
        }
        heights
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, block: Block, theme: &Theme) {
        let width = block.inner(area).width as usize;
        let mut items = Vec::new();
        for project in &self.view {
            let collapsed = self.fold.is_collapsed(&project.path);
            items.push(project_item(project, collapsed, theme));
            if !collapsed {
                let worktrees = project.worktrees.iter();
                items.extend(worktrees.map(|worktree| worktree_item(worktree, theme, width)));
            }
        }
        self.fold.render(frame, area, block, items, theme);
    }
}

/// The project's name, then its error, if any, a line each.
fn project_item(project: &ProjectView, collapsed: bool, theme: &Theme) -> ListItem<'static> {
    let arrow = if collapsed { "▸" } else { "▾" };
    let mut lines = vec![Line::from(vec![
        Span::raw(format!("{arrow} ")),
        Span::styled(
            project.name.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ])];
    if let Some(error) = &project.error {
        let style = Style::new().fg(theme.error);
        lines.extend(
            error
                .lines()
                .map(|line| Line::styled(format!("    {line}"), style)),
        );
    }
    ListItem::new(lines)
}

/// The status icon, the worktree's label, then its pull request's badge against
/// the right edge of `width` columns. A label too long for both is cut with `…`;
/// with no room for even that, the badge goes.
fn worktree_item(worktree: &WorktreeView, theme: &Theme, width: usize) -> ListItem<'static> {
    let (icon, style) = status_icon(worktree.status, theme);
    let row = |label: &str| {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(icon, style),
            Span::raw(format!(" {label}")),
        ];
        if worktree.main {
            spans.push(Span::styled("  repo", Style::new().fg(theme.tag)));
        }
        spans
    };
    let label = worktree.label();
    let Some(badge) = worktree.pr.as_ref().map(|pr| badge(pr, theme)) else {
        return ListItem::new(Line::from(row(label)));
    };
    let badge_width = Line::from(badge.clone()).width();
    // Two spaces at least between the row and the badge, and one after it
    let room = width.saturating_sub(badge_width + 3);
    let mut shown = label.to_string();
    if Line::from(row(&shown)).width() > room {
        loop {
            if shown.pop().is_none() {
                return ListItem::new(Line::from(row(label)));
            }
            if Line::from(row(&format!("{shown}…"))).width() <= room {
                break;
            }
        }
        shown.push('…');
    }
    let mut spans = row(&shown);
    let used = Line::from(spans.clone()).width();
    spans.push(Span::raw(" ".repeat(width - used - badge_width - 1)));
    spans.extend(badge);
    spans.push(Span::raw(" "));
    ListItem::new(Line::from(spans))
}

/// `#42 ✓` for an open pull request; `#42 draft ✓`, `#42 merged` and `#42 closed`
/// dim. Checks are coloured, and left out once it is merged or closed.
fn badge(pr: &Pr, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(format!("#{}", pr.number))];
    let word = match pr.state {
        State::Open => None,
        State::Draft => Some(" draft"),
        State::Merged => Some(" merged"),
        State::Closed => Some(" closed"),
    };
    spans.extend(word.map(Span::raw));
    if let (State::Open | State::Draft, Some(checks)) = (pr.state, pr.checks) {
        let (mark, color) = match checks {
            Checks::Passed => ("✓", theme.passed),
            Checks::Pending => ("●", theme.pending),
            Checks::Failed => ("✗", theme.failed),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(mark, Style::new().fg(color)));
    }
    if pr.state != State::Open {
        for span in &mut spans {
            span.style = span.style.add_modifier(Modifier::DIM);
        }
    }
    spans
}

/// Guessed statuses, from activity without hooks, are dim.
pub fn status_icon(status: Status, theme: &Theme) -> (&'static str, Style) {
    let dim = Style::new().add_modifier(Modifier::DIM);
    match status {
        Status::Busy => ("●", Style::new().fg(theme.busy)),
        Status::Input => ("⚠", Style::new().fg(theme.input)),
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
            pr: None,
        }
    }

    #[test]
    fn badges_colour_the_checks_and_dim_what_is_not_open() {
        let theme = crate::theme::terminal();
        let pr = |state, checks| Pr {
            number: 7,
            state,
            checks,
        };
        let open = badge(&pr(State::Open, Some(Checks::Passed)), &theme);
        assert_eq!(
            open,
            [
                Span::raw("#7"),
                Span::raw(" "),
                Span::styled("✓", Style::new().fg(theme.passed)),
            ]
        );
        let failed = badge(&pr(State::Open, Some(Checks::Failed)), &theme);
        assert_eq!(failed[2].style.fg, Some(theme.failed));
        let pending = badge(&pr(State::Open, Some(Checks::Pending)), &theme);
        assert_eq!(pending[2].style.fg, Some(theme.pending));
        for state in [State::Draft, State::Merged, State::Closed] {
            let spans = badge(&pr(state, Some(Checks::Passed)), &theme);
            let dim = |span: &Span| span.style.add_modifier.contains(Modifier::DIM);
            assert!(spans.iter().all(dim), "{state:?}: {spans:?}");
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
        Some(Row::Child {
            parent: project.into(),
            child: path.into(),
        })
    }

    fn on_project(path: &str) -> Option<Row> {
        Some(Row::Parent(path.into()))
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
        assert_eq!(tree.fold.rows(&tree.groups()).len(), 6);
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
        // Its new place: the row above it is the one above it now
        tree.up();
        let fix = on("/code/app", "/code/app.worktrees/fix");
        assert_eq!(tree.selected().cloned(), fix);
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
