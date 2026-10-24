//! The dashboard: ratatui rendering and input handling only.

mod projects;
mod terminal;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::keys::{Action, Key, Keymap, Pane};
use crate::model::{Sources, WorktreeView};
use crate::refresh::{Refresh, Update};
use crate::tmux::{self, Server};
use projects::Tree;

/// How long to wait for input before applying updates and drawing again.
const TICK: Duration = Duration::from_millis(100);

/// The worktree actions, hidden from the hint bar on a project row.
const WORKTREE_ACTIONS: [Action; 5] = [
    Action::Jump,
    Action::StartAgent,
    Action::Shell,
    Action::OpenPr,
    Action::RemoveWorktree,
];

/// Runs the dashboard until the user quits.
pub fn run() -> Result<()> {
    let config = config::load().map_err(|err| err.to_string());
    let (updates, received) = mpsc::channel();
    let server = Server::tatou(&tmux::conf_path()?);
    // Dropped when the dashboard ends, which stops the workers
    let _refresh = match &config {
        Ok(config) => {
            let projects: Vec<PathBuf> = config.projects.iter().map(|p| p.path.clone()).collect();
            Some(Refresh::start(server.clone(), &projects, &updates))
        }
        Err(_) => None,
    };
    let (failures, failed) = mpsc::channel();

    let (mut terminal, _guard) = terminal::enter().map_err(Error::Terminal)?;
    let mut app = App::new(config);
    // Debug builds only: checks that a panic leaves the terminal usable
    let panic_after_first_frame =
        cfg!(debug_assertions) && std::env::var_os("TATOU_PANIC").is_some();
    loop {
        terminal
            .draw(|frame| app.render(frame))
            .map_err(Error::Terminal)?;
        if panic_after_first_frame {
            panic!("TATOU_PANIC is set");
        }
        if event::poll(TICK).map_err(Error::Terminal)? {
            app.input(event::read().map_err(Error::Terminal)?);
        }
        for task in app.tasks.drain(..) {
            start(task, &server, &failures);
        }
        app.apply_all(&received, &failed);
        app.update_view(unix_now());
        if app.quit {
            return Ok(());
        }
    }
}

/// A command the UI asks for; the loop runs it on a thread of its own.
#[derive(Debug, PartialEq)]
enum Task {
    /// Shows the session with this id in the dashboard's client.
    Switch(String),
}

struct App {
    /// The error is shown in place of the projects.
    config: std::result::Result<Config, String>,
    sources: Sources,
    tree: Tree,
    keymap: Keymap,
    focus: Pane,
    /// Shown in place of the hints until the next key press.
    message: Option<String>,
    /// Asked for since the loop last took them.
    tasks: Vec<Task>,
    quit: bool,
}

impl App {
    fn new(config: std::result::Result<Config, String>) -> App {
        App {
            config,
            sources: Sources::default(),
            tree: Tree::default(),
            keymap: Keymap::defaults(),
            focus: Pane::Worktrees,
            message: None,
            tasks: Vec::new(),
            quit: false,
        }
    }

    fn input(&mut self, event: Event) {
        let Event::Key(key) = event else {
            return;
        };
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.message = None;
        if let Some(action) = self.keymap.action(Key::from_event(key)) {
            self.handle(action);
        }
    }

    /// Applies every update and task failure that has arrived, without waiting for more.
    fn apply_all(&mut self, received: &Receiver<Update>, failed: &Receiver<String>) {
        while let Ok(update) = received.try_recv() {
            self.sources.apply(update);
        }
        while let Ok(failure) = failed.try_recv() {
            self.message = Some(failure);
        }
    }

    /// Rebuilds the left pane from the latest sources; `now` dates agents' activity.
    fn update_view(&mut self, now: u64) {
        if let Ok(config) = &self.config {
            self.tree.set_view(self.sources.view(config, now));
        }
    }

    fn handle(&mut self, action: Action) {
        let worktrees = self.focus == Pane::Worktrees;
        match action {
            Action::Quit => self.quit = true,
            Action::Up if worktrees => self.tree.up(),
            Action::Down if worktrees => self.tree.down(),
            Action::Collapse if worktrees => self.tree.collapse(),
            Action::Expand if worktrees => self.tree.expand(),
            Action::Jump if worktrees => self.jump(),
            Action::SwitchPane => {
                self.focus = match self.focus {
                    Pane::Worktrees => Pane::Files,
                    _ => Pane::Worktrees,
                }
            }
            // Each is filled in by its own step
            _ => {}
        }
    }

    /// On a worktree, switches to its session; on a project, folds or unfolds it.
    fn jump(&mut self) {
        match self.tree.selected_worktree() {
            Some(WorktreeView {
                session: Some(session),
                ..
            }) => self.tasks.push(Task::Switch(session.id.clone())),
            // Starting an agent comes with `s` (#22)
            Some(worktree) => {
                self.message = Some(format!("No session for {} yet", worktree.label()));
            }
            None => self.tree.toggle(),
        }
    }

    /// The actions that do not apply to the selected row.
    fn hidden(&self) -> &'static [Action] {
        if self.focus != Pane::Worktrees {
            return &[];
        }
        match self.tree.selected_worktree() {
            Some(worktree) if worktree.session.is_some() => &[],
            Some(_) => &[Action::Jump],
            None => &WORKTREE_ACTIONS,
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let [main, hints] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(main);
        self.render_projects(frame, left);
        frame.render_widget(self.pane_block(" Files ", Pane::Files), right);
        frame.render_widget(self.hint_bar(), hints);
    }

    /// The projects, else why there are none; a tmux error at the bottom.
    fn render_projects(&mut self, frame: &mut Frame, area: Rect) {
        let mut block = self.pane_block(" Projects ", Pane::Worktrees);
        if let Some(err) = self.sources.sessions_error() {
            let style = Style::new().fg(Color::Red);
            block = block.title_bottom(Line::styled(format!(" {err} "), style));
        }
        let message = match &self.config {
            Ok(_) if !self.tree.is_empty() => return self.tree.render(frame, area, block),
            Ok(config) => format!(
                "No projects yet. Add a [[project]] with its path to {}",
                config.path.display()
            ),
            Err(err) => err.clone(),
        };
        let body = Paragraph::new(message).wrap(Wrap { trim: false });
        frame.render_widget(body.block(block), area);
    }

    /// The focused pane's border is bold.
    fn pane_block(&self, title: &'static str, pane: Pane) -> Block<'static> {
        let block = Block::bordered().title(title);
        if self.focus == pane {
            block.border_style(Style::new().add_modifier(Modifier::BOLD))
        } else {
            block
        }
    }

    /// The message, else the keys that apply to the selection.
    fn hint_bar(&self) -> Paragraph<'static> {
        if let Some(message) = &self.message {
            return Paragraph::new(message.clone());
        }
        let mut spans = Vec::new();
        for (key, label) in self.keymap.hints(self.focus, self.hidden()) {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(key, Style::new().add_modifier(Modifier::BOLD)));
            spans.push(Span::raw(format!(" {label}")));
        }
        Paragraph::new(Line::from(spans))
    }
}

/// Runs `task` on a thread of its own; its error, if any, goes to `failures`.
fn start(task: Task, server: &Server, failures: &Sender<String>) {
    let server = server.clone();
    let failed = failures.clone();
    let started = thread::Builder::new()
        .name("task".to_string())
        .spawn(move || {
            let result = match &task {
                Task::Switch(session) => server.switch_client(session),
            };
            if let Err(err) = result {
                tracing::warn!(?task, %err, "task failed");
                // The dashboard has ended: nobody to tell
                let _ = failed.send(err.to_string());
            }
        });
    if let Err(err) = started {
        tracing::error!(%err, "cannot start a task thread");
        let _ = failures.send(format!("cannot start a thread: {err}"));
    }
}

/// Seconds since the Unix epoch, like tmux's activity times.
fn unix_now() -> u64 {
    let since_epoch = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    since_epoch.map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::projects::tests::{project, worktree};
    use super::*;
    use crate::model::{ProjectView, SessionView, Status};

    fn app() -> App {
        App::new(Err("not loaded".into()))
    }

    fn press(app: &mut App, code: KeyCode) {
        app.input(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    /// The text on screen, one string per line, styles left out.
    fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let cells = (0..width).map(|x| buffer[(x, y)].symbol().to_string());
                cells.collect::<String>().trim_end().to_string()
            })
            .collect()
    }

    /// The left pane's text inside its borders, on a 100-column screen.
    fn projects_pane(app: &mut App, height: u16) -> Vec<String> {
        let lines = screen(app, 100, height);
        let inner = &lines[1..height as usize - 2];
        inner
            .iter()
            .map(|line| {
                line.chars()
                    .skip(1)
                    .take(38)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The dashboard showing `view`.
    fn showing(view: Vec<ProjectView>) -> App {
        let mut app = App::new(Ok(test_config()));
        app.tree.set_view(view);
        app
    }

    #[test]
    fn lists_two_projects_and_their_worktrees() {
        let mut app = showing(vec![
            project("/code/app", &["fix-login"]),
            project("/code/web", &[]),
        ]);
        let expected = [
            "┌ Projects ──────────────────┐┌ Files ─────────────────────────────────────┐",
            "│▾ app                       ││                                            │",
            "│  · main  repo              ││                                            │",
            "│  · fix-login               ││                                            │",
            "│▾ web                       ││                                            │",
            "│  · main  repo              ││                                            │",
            "└────────────────────────────┘└────────────────────────────────────────────┘",
            "n new  ? help  q quit",
        ];
        assert_eq!(screen(&mut app, 76, 8), expected);
    }

    #[test]
    fn a_collapsed_project_hides_its_worktrees() {
        let mut app = showing(vec![
            project("/code/app", &["fix-login"]),
            project("/code/web", &[]),
        ]);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(
            projects_pane(&mut app, 7),
            ["▸ app", "▾ web", "  · main  repo", ""]
        );
    }

    #[test]
    fn each_status_has_its_icon() {
        let statuses = [
            ("busy", Status::Busy),
            ("input", Status::Input),
            ("idle", Status::Idle),
            ("active", Status::Active),
            ("quiet", Status::Quiet),
            ("none", Status::NoSession),
        ];
        let mut app_project = project("/code/app", &[]);
        app_project.worktrees = statuses
            .iter()
            .map(|(name, status)| worktree(&format!("/code/{name}"), Some(name), *status))
            .collect();
        let mut app = showing(vec![app_project]);
        assert_eq!(
            projects_pane(&mut app, 10)[1..],
            [
                "  ● busy",
                "  ⚠ input",
                "  ○ idle",
                "  ● active",
                "  ○ quiet",
                "  · none"
            ]
        );
    }

    #[test]
    fn a_detached_worktree_shows_its_name() {
        let detached = worktree("/code/app.worktrees/spike", None, Status::Idle);
        let mut app_project = project("/code/app", &[]);
        app_project.worktrees.push(detached);
        let lines = projects_pane(&mut showing(vec![app_project]), 6);
        assert_eq!(lines[2], "  ○ spike");
    }

    #[test]
    fn project_errors_show_under_the_project() {
        let mut broken = project("/code/broken", &[]);
        broken.worktrees.clear();
        broken.error = Some("fatal: not a git repository\nhint: check the path".into());
        let mut app = showing(vec![broken, project("/code/web", &[])]);
        assert_eq!(
            projects_pane(&mut app, 7)[..4],
            [
                "▾ broken",
                "    fatal: not a git repository",
                "    hint: check the path",
                "▾ web"
            ]
        );
    }

    #[test]
    fn a_tmux_error_shows_at_the_bottom_of_the_pane() {
        let mut app = showing(vec![project("/code/app", &[])]);
        app.sources
            .apply(Update::Sessions(Err("tmux: no server".into())));
        let lines = screen(&mut app, 60, 5);
        assert!(lines[3].starts_with("└ tmux: no server ─"), "{lines:?}");
    }

    #[test]
    fn without_projects_shows_the_config_path() {
        let mut config = test_config();
        config.path = "/home/me/.config/tatou/config.toml".into();
        let mut app = App::new(Ok(config));
        assert_eq!(
            projects_pane(&mut app, 7),
            [
                "No projects yet. Add a [[project]]",
                "with its path to",
                "/home/me/.config/tatou/config.toml",
                ""
            ]
        );
    }

    #[test]
    fn moving_applies_to_the_focused_pane_only() {
        let mut app = showing(vec![project("/code/app", &["fix"])]);
        press(&mut app, KeyCode::Char('j'));
        let selected = app.tree.selected().cloned();
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.tree.selected().cloned(), selected);
    }

    #[test]
    fn a_config_error_shows_in_place_of_the_projects() {
        let mut app = App::new(Err("config.toml: unknown field `popop`".into()));
        let lines = screen(&mut app, 100, 6);
        assert!(lines[1].contains("config.toml: unknown field"), "{lines:?}");
    }

    #[test]
    fn tab_switches_the_focus_and_the_hints() {
        let mut app = app();
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Pane::Files);
        let lines = screen(&mut app, 80, 4);
        assert!(lines[3].starts_with("e edit  v view  D diff"), "{lines:?}");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Pane::Worktrees);
    }

    #[test]
    fn q_quits_and_other_keys_do_not() {
        let mut app = app();
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Enter);
        assert!(!app.quit);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn updates_are_applied_without_waiting() {
        let mut app = app();
        let (updates, received) = mpsc::channel();
        let (failures, failed) = mpsc::channel();
        updates
            .send(Update::Sessions(Err("tmux: boom".into())))
            .unwrap();
        failures.send("can't find session: $9".to_string()).unwrap();
        app.apply_all(&received, &failed);
        assert_eq!(app.sources.sessions_error(), Some("tmux: boom"));
        assert_eq!(app.message.as_deref(), Some("can't find session: $9"));
        // Nothing more to apply: returns at once
        app.apply_all(&received, &failed);
    }

    /// `app` with a session on its `fix` worktree and none on its main one.
    fn with_a_session() -> App {
        let mut app_project = project("/code/app", &["fix"]);
        app_project.worktrees[1].session = Some(SessionView {
            id: "$3".into(),
            name: "app-fix".into(),
            agent_pane: None,
        });
        showing(vec![app_project])
    }

    fn hint_line(app: &mut App) -> String {
        screen(app, 100, 6)[5].clone()
    }

    #[test]
    fn enter_switches_to_the_worktrees_session() {
        let mut app = with_a_session();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tasks, [Task::Switch("$3".into())]);
        assert_eq!(app.message, None);
    }

    #[test]
    fn enter_without_a_session_says_so_until_the_next_key() {
        let mut app = with_a_session();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tasks, []);
        assert_eq!(hint_line(&mut app), "No session for main yet");
        press(&mut app, KeyCode::Char('x'));
        assert!(
            hint_line(&mut app).starts_with("n new"),
            "back to the hints"
        );
    }

    #[test]
    fn enter_on_a_project_folds_and_unfolds_it() {
        let mut app = with_a_session();
        press(&mut app, KeyCode::Enter);
        assert_eq!(projects_pane(&mut app, 6), ["▸ app", "", ""]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            projects_pane(&mut app, 6)[0..2],
            ["▾ app", "  · main  repo"]
        );
        assert_eq!(app.tasks, []);
    }

    #[test]
    fn the_hints_follow_the_selected_row() {
        let mut app = with_a_session();
        assert_eq!(hint_line(&mut app), "n new  ? help  q quit", "project");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            hint_line(&mut app),
            "n new  s start agent  t shell  p PR  d remove  ? help  q quit",
            "no session"
        );
        press(&mut app, KeyCode::Char('j'));
        assert!(hint_line(&mut app).starts_with("⏎ jump  n new"), "session");
        press(&mut app, KeyCode::Tab);
        assert!(hint_line(&mut app).starts_with("e edit"), "files");
    }

    fn test_config() -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        config::load_from(&path, None).unwrap()
    }
}
