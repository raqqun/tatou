//! The dashboard: ratatui rendering and input handling only.

mod dialog;
mod projects;
mod sessions;
mod terminal;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

use crate::actions::{
    self, EDITOR, NewWorktree, Program, RemoveWorktree, RestartAgent, StartAgent, Task,
};
use crate::client::{Client, Stream};
use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::git;
use crate::keys::{Action, Key, Keymap, Pane};
use crate::model::{self, SessionView, Sources, WorktreeView};
use crate::protocol::{Reply, Request};
use crate::refresh::{ConfigWatch, Refresh, Update};
use crate::term::{self, Modes};
use dialog::{Choice, Confirm, Dialog, Field, Form, Message, Outcome};
use projects::Tree;
use sessions::Sessions;

/// The focused pane's border and title: cyan, as the `repo` tag.
const ACCENT: Color = Color::Cyan;

/// How long to wait for input before applying updates and drawing again; short
/// enough for a shown terminal's output to appear as it comes.
const TICK: Duration = Duration::from_millis(15);

/// Moves the focus from the shown terminal back to the dashboard.
const BACK: (KeyCode, KeyModifiers) = (KeyCode::Char('d'), KeyModifiers::ALT);

/// Show the next and the previous session's terminal, from anywhere.
const NEXT: (KeyCode, KeyModifiers) = (KeyCode::Char('l'), KeyModifiers::ALT);
const PREVIOUS: (KeyCode, KeyModifiers) = (KeyCode::Char('h'), KeyModifiers::ALT);

/// Show the next and the previous window of the shown session, from anywhere.
const NEXT_WINDOW: (KeyCode, KeyModifiers) = (KeyCode::Char('j'), KeyModifiers::ALT);
const PREVIOUS_WINDOW: (KeyCode, KeyModifiers) = (KeyCode::Char('k'), KeyModifiers::ALT);

/// Lines of scrollback kept for the shown terminal.
const SCROLLBACK: usize = 1_000;

/// The worktree actions, hidden from the hint bar on a project row.
const WORKTREE_ACTIONS: [Action; 5] = [
    Action::Jump,
    Action::StartAgent,
    Action::Shell,
    Action::OpenPr,
    Action::RemoveWorktree,
];

/// Runs the dashboard until the user quits; the daemon keeps every terminal.
pub fn run(client: Client) -> Result<()> {
    let path = config::path()?;
    // Before loading, so that a change made meanwhile is loaded again
    let seen = config::stamp(&path);
    let config = config::load().map_err(|err| err.to_string());
    let (updates, received) = mpsc::channel();
    let projects = match &config {
        Ok(config) => project_paths(config),
        Err(_) => Vec::new(),
    };
    let watch = ConfigWatch::new(path, seen);
    let refresh = Refresh::start(client.clone(), watch, &projects, &updates);
    let (events, happened) = mpsc::channel();
    // Open while the dashboard runs, which also keeps the daemon from ending
    let (stream, streamed) = client.stream()?;

    let (mut terminal, guard) = terminal::enter().map_err(Error::Terminal)?;
    let mut app = App::new(config);
    // Dropped with the app when the dashboard ends, which stops the workers
    app.refresh = Some(refresh);
    app.stream = Some(stream);
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
            actions::start(task, &client, &events);
        }
        while let Ok(reply) = streamed.try_recv() {
            app.streamed(reply);
        }
        app.apply_all(&received, &happened);
        app.update_view(unix_now());
        if app.quit {
            drop(guard);
            return Ok(());
        }
    }
}

/// What Alt-l/Alt-h and Alt-j/Alt-k step through.
#[derive(Clone, Copy)]
enum Step {
    Session,
    /// The windows of the shown terminal's session.
    Window,
}

/// The terminal shown on the right: the daemon's, drawn from the screen it sent
/// and the output since.
struct Shown {
    /// `%3`.
    terminal: String,
    screen: vt100::Parser,
}

/// An open dialog, and what its answer is for.
struct Open {
    dialog: Dialog,
    purpose: Purpose,
}

enum Purpose {
    NewWorktree {
        project: PathBuf,
    },
    /// The agent to start in a worktree, or to restart in its session.
    Agent(AgentFor),
    /// The restart to run once confirmed.
    Restart(RestartAgent),
    /// The removal to run once confirmed, forced or not.
    Remove(RemoveWorktree),
    /// Only to read, e.g. a command's output.
    Notice,
}

/// The worktree `s` was pressed on.
struct AgentFor {
    path: PathBuf,
    /// Its label, for the dialogs and progress.
    name: String,
    /// The session name to give a new session.
    session_name: String,
    /// Its session's id and agent pane, when it has a session.
    session: Option<(String, Option<String>)>,
}

struct App {
    /// The error is shown in place of the projects, until a valid config is loaded.
    config: std::result::Result<Config, String>,
    /// Why the config could not be loaded again; the last valid one stays meanwhile.
    reload_error: Option<String>,
    /// `None` in tests.
    refresh: Option<Refresh>,
    sources: Sources,
    tree: Tree,
    sessions: Sessions,
    /// `$HOME`, to show folders with `~/`.
    home: Option<PathBuf>,
    keymap: Keymap,
    focus: Pane,
    /// Takes the keys while open; drawn over the panes.
    dialog: Option<Open>,
    /// Shown in place of the hints until the next key press.
    message: Option<String>,
    /// What a background action is doing, shown in place of the hints until it ends.
    progress: Option<String>,
    /// A worktree to select as soon as it is listed: the one just created.
    select_when_listed: Option<PathBuf>,
    /// Asked for since the loop last took them.
    tasks: Vec<Task>,
    /// The connection the shown terminal comes over; `None` in tests.
    stream: Option<Stream>,
    shown: Option<Shown>,
    /// The shown terminal's size inside its pane, as last drawn: (rows, cols).
    slot: (u16, u16),
    quit: bool,
}

impl App {
    fn new(config: std::result::Result<Config, String>) -> App {
        App {
            config,
            reload_error: None,
            refresh: None,
            sources: Sources::default(),
            tree: Tree::default(),
            sessions: Sessions::default(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            keymap: Keymap::defaults(),
            focus: Pane::Worktrees,
            dialog: None,
            message: None,
            progress: None,
            select_when_listed: None,
            tasks: Vec::new(),
            stream: None,
            shown: None,
            slot: (24, 80),
            quit: false,
        }
    }

    fn input(&mut self, event: Event) {
        if let Event::Paste(text) = &event
            && self.focus == Pane::Terminal
        {
            let bytes = term::paste_bytes(text, self.modes());
            return self.send(Request::Input(bytes));
        }
        let Event::Key(key) = event else {
            return;
        };
        if key.kind == KeyEventKind::Release {
            return;
        }
        // From anywhere but a dialog
        let step = match (key.code, key.modifiers) {
            NEXT => Some((Step::Session, 1)),
            PREVIOUS => Some((Step::Session, -1)),
            NEXT_WINDOW => Some((Step::Window, 1)),
            PREVIOUS_WINDOW => Some((Step::Window, -1)),
            _ => None,
        };
        if let (Some((by, step)), None) = (step, &self.dialog) {
            if key.kind == KeyEventKind::Press {
                self.message = None;
                self.step(by, step);
            }
            return;
        }
        // Every key goes to the shown terminal's program, but these and the one back
        if self.focus == Pane::Terminal && self.dialog.is_none() {
            if (key.code, key.modifiers) == BACK {
                self.focus = Pane::Worktrees;
            } else {
                self.send(Request::Input(term::key_bytes(key, self.modes())));
            }
            return;
        }
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.message = None;
        if let Some(open) = &mut self.dialog {
            let outcome = open.dialog.key(key);
            if outcome != Outcome::Open {
                self.answered(outcome);
            }
            return;
        }
        if let Some(action) = self.keymap.action(Key::from_event(key)) {
            self.handle(action);
        }
    }

    /// Applies every update and action event that has arrived, without waiting for more.
    fn apply_all(&mut self, received: &Receiver<Update>, happened: &Receiver<actions::Event>) {
        while let Ok(update) = received.try_recv() {
            match update {
                Update::Config(result) => self.reload(result),
                update => self.sources.apply(update),
            }
        }
        while let Ok(event) = happened.try_recv() {
            self.happened(event);
        }
    }

    /// What an action running in the background reports.
    fn happened(&mut self, event: actions::Event) {
        use actions::Event;
        match event {
            Event::Progress(text) => self.progress = Some(text),
            Event::Failed(message) => {
                self.progress = None;
                self.message = Some(message);
            }
            // Back to the form, to fix the name; cancelled meanwhile, to the hint bar
            Event::Refused(error) => {
                self.progress = None;
                match self.new_worktree_form() {
                    Some(form) => {
                        form.set_busy(None);
                        form.set_error(&error);
                    }
                    None => self.message = Some(error),
                }
            }
            Event::Added { project, path } => {
                if self.new_worktree_form().is_some() {
                    self.dialog = None;
                }
                self.select_when_listed = Some(path);
                if let Some(refresh) = &self.refresh {
                    refresh.worktrees_now(&project);
                }
            }
            Event::OnCreateFailed(output) => match self.dialog {
                None => {
                    let dialog = Dialog::Message(Message::new("on_create failed", &output));
                    self.dialog = Some(Open {
                        dialog,
                        purpose: Purpose::Notice,
                    });
                }
                // Not over another dialog: its first line, then
                Some(_) => self.message = output.lines().next().map(String::from),
            },
            Event::Started(terminal) => {
                self.progress = None;
                self.show(&terminal);
                if let Some(refresh) = &self.refresh {
                    refresh.sessions_now();
                }
            }
            // Forcing is a second, explicit question, never an answer by default
            Event::RemoveRefused { remove, why } => {
                self.progress = None;
                let question =
                    "Force the removal? Uncommitted changes and untracked files in it are lost.";
                let confirm = Confirm::new(
                    &format!("git refused to remove {}", remove.name),
                    question,
                    false,
                );
                let force = RemoveWorktree {
                    force: true,
                    ..remove
                };
                self.open_over(
                    Dialog::Confirm(confirm.with_detail(&why)),
                    Purpose::Remove(force),
                    &why,
                );
            }
            Event::Removed { project, name } => {
                self.progress = None;
                self.message = Some(format!("{name} removed; its branch stays"));
                if let Some(refresh) = &self.refresh {
                    refresh.worktrees_now(&project);
                    refresh.sessions_now();
                }
            }
            Event::Error { title, output } => {
                self.progress = None;
                let message = Dialog::Message(Message::new(&title, &output));
                self.open_over(message, Purpose::Notice, &output);
            }
        }
    }

    /// Opens `dialog`, unless one is open already: then only `text`'s first line
    /// shows, in the hint bar.
    fn open_over(&mut self, dialog: Dialog, purpose: Purpose, text: &str) {
        match self.dialog {
            None => self.dialog = Some(Open { dialog, purpose }),
            Some(_) => self.message = text.lines().next().map(String::from),
        }
    }

    /// The new-worktree form, when it is open.
    fn new_worktree_form(&mut self) -> Option<&mut Form> {
        match &mut self.dialog {
            Some(Open {
                dialog: Dialog::Form(form),
                purpose: Purpose::NewWorktree { .. },
            }) => Some(form),
            _ => None,
        }
    }

    /// Takes the config loaded again after it changed; an invalid one keeps the
    /// last valid one on screen.
    fn reload(&mut self, result: std::result::Result<Config, String>) {
        let config = match result {
            Ok(config) => config,
            Err(err) if self.config.is_ok() => return self.reload_error = Some(err),
            Err(err) => return self.config = Err(err),
        };
        if let Some(refresh) = &mut self.refresh {
            refresh.set_projects(&project_paths(&config));
        }
        self.config = Ok(config);
        self.reload_error = None;
    }

    /// Rebuilds the left pane from the latest sources; `now` dates agents' activity.
    fn update_view(&mut self, now: u64) {
        let view = match &self.config {
            Ok(config) => self.sources.view(config, now),
            Err(_) => Vec::new(),
        };
        let rows = self.sources.session_rows(&view, now, self.home.as_deref());
        self.sessions.set_rows(rows);
        if self.config.is_ok() {
            self.tree.set_view(view);
        }
        self.select_new_worktree();
    }

    /// Selects the worktree just created, once it is listed.
    fn select_new_worktree(&mut self) {
        if let Some(path) = &self.select_when_listed
            && self.tree.select_worktree(path)
        {
            self.select_when_listed = None;
        }
    }

    fn handle(&mut self, action: Action) {
        let worktrees = self.focus == Pane::Worktrees;
        let sessions = self.focus == Pane::Sessions;
        match action {
            Action::Quit => self.quit = true,
            Action::Up if sessions => self.sessions.up(),
            Action::Down if sessions => self.sessions.down(),
            Action::Collapse if sessions => self.sessions.collapse(),
            Action::Expand if sessions => self.sessions.expand(),
            // A session's current terminal, or one of its terminals
            Action::Jump if sessions => {
                if let Some(terminal) = self.sessions.selected_terminal() {
                    self.show(&terminal);
                }
            }
            Action::Up if worktrees => self.tree.up(),
            Action::Down if worktrees => self.tree.down(),
            Action::Collapse if worktrees => self.tree.collapse(),
            Action::Expand if worktrees => self.tree.expand(),
            Action::Jump if worktrees => self.jump(),
            Action::NewWorktree if worktrees => self.open_new_worktree(),
            Action::StartAgent if worktrees => self.choose_agent(),
            Action::Shell if worktrees => self.open(None, false),
            Action::Edit if worktrees => self.open(Some(EDITOR), true),
            Action::RemoveWorktree if worktrees => self.confirm_remove(),
            // Between Projects and Sessions only: Enter goes into the terminal, and
            // in it Tab is the program's
            Action::SwitchPane => {
                self.focus = match self.focus {
                    Pane::Worktrees => Pane::Sessions,
                    _ => Pane::Worktrees,
                }
            }
            // Each is filled in by its own step
            _ => {}
        }
    }

    /// The open dialog was answered or cancelled. Each kind of dialog acts on its
    /// answer here, and may keep it open, e.g. to show an error in a form.
    fn answered(&mut self, outcome: Outcome) {
        let Some(open) = self.dialog.take() else {
            return;
        };
        if outcome != Outcome::Submit {
            return;
        }
        match (open.purpose, open.dialog) {
            (Purpose::NewWorktree { project }, Dialog::Form(form)) => {
                self.create_worktree(project, form);
            }
            (Purpose::Agent(target), Dialog::Choice(choice)) => {
                if let Some(agent) = choice.selected() {
                    self.agent_chosen(target, agent);
                }
            }
            (Purpose::Restart(restart), Dialog::Confirm(_)) => {
                self.tasks.push(Task::RestartAgent(restart));
            }
            (Purpose::Remove(remove), Dialog::Confirm(_)) => {
                self.tasks.push(Task::RemoveWorktree(remove));
            }
            _ => {}
        }
    }

    /// `d` on a linked worktree: asks before removing it and closing its session.
    fn confirm_remove(&mut self) {
        let (Some(worktree), Some(project)) =
            (self.tree.selected_worktree(), self.tree.selected_project())
        else {
            return;
        };
        let name = worktree.label().to_string();
        if worktree.main {
            self.message = Some(format!("{name} is the main worktree: it cannot be removed"));
            return;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let path = config::tilde(&worktree.path, home.as_deref());
        let branch = match &worktree.branch {
            Some(branch) => format!("branch {branch}"),
            None => "detached".to_string(),
        };
        let after = match &worktree.session {
            Some(session) => format!("Its session {} is closed; the branch stays.", session.name),
            None => "The branch stays.".to_string(),
        };
        let question = format!("Remove the worktree {path} ({branch})? {after}");
        let remove = RemoveWorktree {
            name,
            project: project.path.clone(),
            path: worktree.path.clone(),
            session: worktree.session.as_ref().map(|s| s.id.clone()),
            force: false,
        };
        self.dialog = Some(Open {
            dialog: Dialog::Confirm(Confirm::new("Remove the worktree", &question, false)),
            purpose: Purpose::Remove(remove),
        });
    }

    /// `command` (`None`: a shell) in the selected worktree, in a new window of its
    /// session or in a new session; on a project, with `main`, in its main worktree.
    fn open(&mut self, command: Option<&str>, main: bool) {
        let Some(project) = self.tree.selected_project() else {
            return;
        };
        let worktree = match self.tree.selected_worktree() {
            Some(worktree) => worktree,
            None if main => match project.worktrees.iter().find(|w| w.main) {
                Some(worktree) => worktree,
                None => return,
            },
            None => return,
        };
        let name = worktree.label().to_string();
        self.tasks.push(Task::Program(Program {
            path: worktree.path.clone(),
            session: worktree.session.as_ref().map(|s| s.id.clone()),
            session_name: model::session_name(&project.name, &name),
            name,
            command: command.map(String::from),
        }));
    }

    /// `s` on a worktree: which agent, `default_agent` chosen first.
    fn choose_agent(&mut self) {
        let (Some(worktree), Some(project), Ok(config)) = (
            self.tree.selected_worktree(),
            self.tree.selected_project(),
            &self.config,
        ) else {
            return;
        };
        let name = worktree.label().to_string();
        let target = AgentFor {
            path: worktree.path.clone(),
            session_name: model::session_name(&project.name, &name),
            session: (worktree.session.as_ref()).map(|s| (s.id.clone(), s.agent_pane.clone())),
            name,
        };
        let agents = config.agents.keys().cloned().collect();
        let title = format!("Agent for {}", target.name);
        let choice = Choice::new(&title, agents, &config.default_agent);
        self.dialog = Some(Open {
            dialog: Dialog::Choice(choice),
            purpose: Purpose::Agent(target),
        });
    }

    /// Starts `agent` for a worktree without a session; with one, asks first,
    /// as restarting ends the agent running there.
    fn agent_chosen(&mut self, target: AgentFor, agent: &str) {
        let Some(command) = self.config.as_ref().ok().and_then(|c| c.agents.get(agent)) else {
            return;
        };
        let command = command.clone();
        let Some((session, pane)) = target.session else {
            self.tasks.push(Task::StartAgent(StartAgent {
                name: target.name,
                path: target.path,
                session: target.session_name,
                command,
            }));
            return;
        };
        let question = format!("Restart {agent} in {}?", target.name);
        let restart = RestartAgent {
            name: target.name,
            path: target.path,
            session,
            pane,
            command,
        };
        self.dialog = Some(Open {
            dialog: Dialog::Confirm(Confirm::new("Restart the agent", &question, true)),
            purpose: Purpose::Restart(restart),
        });
    }

    /// The form for a new worktree in the selected row's project: name, base and
    /// agent. On a worktree, the base is its branch; on the project, or a detached
    /// worktree, the project's `base`, else its remote's default branch
    /// (`origin/main`), else the branch checked out in its main worktree.
    fn open_new_worktree(&mut self) {
        let (Some(view), Ok(config)) = (self.tree.selected_project(), &self.config) else {
            return;
        };
        if let Some(error) = &view.error {
            let first = error.lines().next().unwrap_or_default();
            self.message = Some(format!("{}: {first}", view.name));
            return;
        }
        let project = config.projects.iter().find(|p| p.path == view.path);
        let main_branch = view.worktrees.iter().find(|w| w.main);
        let worktree_branch = self.tree.selected_worktree().and_then(|w| w.branch.clone());
        let base = worktree_branch
            .or_else(|| project.and_then(|p| p.base.clone()))
            .or_else(|| view.remote_default.clone())
            .or_else(|| main_branch.and_then(|w| w.branch.clone()))
            .unwrap_or_default();
        let agents = config.agents.keys().cloned().collect();
        let fields = vec![
            Field::text("Name", ""),
            Field::text("Base", &base),
            Field::choice("Agent", agents, &config.default_agent),
        ];
        let form = Form::new(&format!("New worktree in {}", view.name), fields);
        let purpose = Purpose::NewWorktree {
            project: view.path.clone(),
        };
        self.dialog = Some(Open {
            dialog: Dialog::Form(form),
            purpose,
        });
    }

    /// Starts creating the worktree the form asks for; the form stays open, busy,
    /// until git has answered.
    fn create_worktree(&mut self, project: PathBuf, mut form: Form) {
        let values: Vec<String> = form.values().into_iter().map(String::from).collect();
        match self.new_worktree_request(&project, &values) {
            Ok(request) => {
                form.set_busy(Some(&format!("Creating {}…", request.name)));
                self.tasks.push(Task::NewWorktree(request));
            }
            Err(error) => form.set_error(&error),
        }
        self.dialog = Some(Open {
            dialog: Dialog::Form(form),
            purpose: Purpose::NewWorktree { project },
        });
    }

    /// What to create for the form's name, base and agent, or why it cannot be.
    fn new_worktree_request(
        &self,
        project: &Path,
        values: &[String],
    ) -> std::result::Result<NewWorktree, String> {
        let [name, base, agent] = values else {
            return Err("The form has the wrong fields".to_string());
        };
        let (name, base) = (name.trim(), base.trim());
        if name.is_empty() {
            return Err("A name is needed".to_string());
        }
        let Ok(config) = &self.config else {
            return Err("config.toml is invalid".to_string());
        };
        let Some(settings) = config.projects.iter().find(|p| p.path == project) else {
            return Err(format!("{} is no longer a project", project.display()));
        };
        let Some(command) = config.agents.get(agent) else {
            return Err(format!("No agent named {agent}"));
        };
        let project_name = project.file_name().unwrap_or_default().to_string_lossy();
        Ok(NewWorktree {
            project: project.to_path_buf(),
            name: name.to_string(),
            base: (!base.is_empty()).then(|| base.to_string()),
            path: git::worktree_path(project, name, settings.worktree_dir.as_deref()),
            on_create: settings.on_create.clone(),
            session: model::session_name(&project_name, name),
            command: command.clone(),
        })
    }

    /// On a worktree, shows its session's current terminal; on a project, folds or
    /// unfolds it.
    fn jump(&mut self) {
        match self.tree.selected_worktree() {
            Some(WorktreeView {
                session:
                    Some(SessionView {
                        current: Some(terminal),
                        ..
                    }),
                ..
            }) => {
                let terminal = terminal.clone();
                self.show(&terminal);
            }
            // Starting an agent comes with `s` (#22)
            Some(worktree) => {
                self.message = Some(format!("No session for {} yet", worktree.label()));
            }
            None => self.tree.toggle(),
        }
    }

    /// The actions that do not apply to the selected row.
    fn hidden(&self) -> &'static [Action] {
        if self.focus == Pane::Sessions && self.sessions.selected().is_none() {
            return &[Action::Jump];
        }
        if self.focus != Pane::Worktrees {
            return &[];
        }
        match self.tree.selected_worktree() {
            Some(worktree) => match (worktree.session.is_some(), worktree.main) {
                (true, false) => &[],
                (true, true) => &[Action::RemoveWorktree],
                (false, false) => &[Action::Jump],
                (false, true) => &[Action::Jump, Action::RemoveWorktree],
            },
            None => &WORKTREE_ACTIONS,
        }
    }

    /// Shows `terminal` on the right and gives it the focus: the daemon sends its
    /// screen, then its output, over the stream.
    fn show(&mut self, terminal: &str) {
        let (rows, cols) = self.slot;
        self.send(Request::Attach {
            terminal: terminal.to_string(),
            rows,
            cols,
        });
        self.shown = Some(Shown {
            terminal: terminal.to_string(),
            screen: vt100::Parser::new(rows, cols, SCROLLBACK),
        });
        self.focus = Pane::Terminal;
    }

    /// Shows the current terminal of the session `step` away from the shown one's,
    /// or the window `step` away in its session, wrapping around; from the first or
    /// the last session when none is shown. The focus goes to it, as with Enter.
    fn step(&mut self, by: Step, step: isize) {
        let shown = self.shown.as_ref().map(|shown| shown.terminal.clone());
        let next = match by {
            Step::Session => self.sessions.step(shown.as_deref(), step),
            Step::Window => self.sessions.step_window(shown.as_deref(), step),
        };
        let Some(terminal) = next else {
            return;
        };
        self.show(&terminal);
    }

    /// What the daemon streams for the shown terminal.
    fn streamed(&mut self, reply: Reply) {
        match (reply, &mut self.shown) {
            (Reply::Screen(bytes), Some(shown)) => {
                let (rows, cols) = self.slot;
                shown.screen = vt100::Parser::new(rows, cols, SCROLLBACK);
                shown.screen.process(&bytes);
            }
            (Reply::Output(bytes), Some(shown)) => shown.screen.process(&bytes),
            // Its program ended, and the terminal with it
            // Its program ended, and the terminal with it: a neighbour shows instead
            (Reply::Exited, _) => {
                let next =
                    (self.shown.take()).and_then(|shown| self.sessions.ended(&shown.terminal));
                match next {
                    Some(terminal) => self.show(&terminal),
                    None if self.focus == Pane::Terminal => self.focus = Pane::Worktrees,
                    None => {}
                }
                if let Some(refresh) = &self.refresh {
                    refresh.sessions_now();
                }
            }
            (Reply::Error(message), _) => {
                self.shown = None;
                self.focus = Pane::Worktrees;
                self.message = Some(message);
            }
            _ => {}
        }
    }

    fn send(&self, request: Request) {
        if let Some(stream) = &self.stream {
            stream.send(request);
        }
    }

    /// The shown terminal's modes, which some keys' bytes follow.
    fn modes(&self) -> Modes {
        match &self.shown {
            Some(shown) => Modes {
                application_cursor: shown.screen.screen().application_cursor(),
                bracketed_paste: shown.screen.screen().bracketed_paste(),
            },
            None => Modes::default(),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let [main, hints] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(main);
        self.render_terminal(frame, right);
        // Sessions as tall as its rows and borders, up to a third of the column; not
        // drawn when a third has no room for one row, so the projects keep theirs
        let wanted = self.sessions.len().max(1) as u16 + 2;
        let height = match wanted.min(left.height / 3) {
            fits @ 3.. => fits,
            _ => 0,
        };
        let [projects, sessions] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).areas(left);
        // Without room for the sessions, their error goes below the projects
        self.render_projects(frame, projects, height == 0);
        if height > 0 {
            self.render_sessions(frame, sessions);
        }
        frame.render_widget(self.hint_bar(), hints);
        if let Some(open) = &mut self.dialog {
            open.dialog.render(frame, main);
        }
    }

    /// The shown terminal, at the pane's size, which the daemon follows; what to do
    /// when none is shown.
    fn render_terminal(&mut self, frame: &mut Frame, area: Rect) {
        let title = match &self.shown {
            Some(shown) => format!(" {} ", self.sessions.title(&shown.terminal)),
            None => " Terminal ".to_string(),
        };
        let block = self.pane_block_titled(title, Pane::Terminal);
        let inner = block.inner(area);
        let size = (inner.height.max(1), inner.width.max(1));
        if size != self.slot {
            self.slot = size;
            if let Some(shown) = &mut self.shown {
                shown.screen.screen_mut().set_size(size.0, size.1);
                self.send(Request::Resize {
                    rows: size.0,
                    cols: size.1,
                });
            }
        }
        match &self.shown {
            Some(shown) => {
                let mut cursor = tui_term::widget::Cursor::default();
                if self.focus != Pane::Terminal {
                    cursor.hide();
                }
                let widget = PseudoTerminal::new(shown.screen.screen())
                    .block(block)
                    .cursor(cursor);
                frame.render_widget(widget, area);
            }
            None => {
                let text = "Enter on a worktree or a session shows its terminal here";
                let body = Paragraph::new(text).wrap(Wrap { trim: false });
                frame.render_widget(body.block(block), area);
            }
        }
    }

    /// Every session the daemon has; its error at the bottom.
    fn render_sessions(&mut self, frame: &mut Frame, area: Rect) {
        let mut block = self.pane_block(" Sessions ", Pane::Sessions);
        if let Some(err) = self.sources.sessions_error() {
            let style = error_style();
            block = block.title_bottom(Line::styled(format!(" {err} "), style));
        }
        self.sessions.render(frame, area, block);
    }

    /// The projects, else why there are none; a reload error at the bottom, and
    /// the sessions' error with `sessions_error`.
    fn render_projects(&mut self, frame: &mut Frame, area: Rect, sessions_error: bool) {
        let mut block = self.pane_block(" Projects ", Pane::Worktrees);
        let style = error_style();
        if let (Some(err), Ok(config)) = (&self.reload_error, &self.config) {
            let err = one_line(err, &config.path);
            block = block.title_bottom(Line::styled(format!(" config.toml: {err} "), style));
        }
        if let Some(err) = self.sources.sessions_error().filter(|_| sessions_error) {
            block = block.title_bottom(Line::styled(format!(" {err} "), style));
        }
        let message = match &self.config {
            Ok(_) if !self.tree.is_empty() => return self.tree.render(frame, area, block),
            Ok(config) => format!(
                "No projects yet: run `tatou add` in a repository, or add a file to {}",
                config::projects_dir(&config.path).display()
            ),
            Err(err) => err.clone(),
        };
        let body = Paragraph::new(message).wrap(Wrap { trim: false });
        frame.render_widget(body.block(block), area);
    }

    /// The focused pane's border and title are in the accent color; the other
    /// panes are dim, borders, titles and contents.
    fn pane_block(&self, title: &'static str, pane: Pane) -> Block<'static> {
        self.pane_block_titled(title.to_string(), pane)
    }

    fn pane_block_titled(&self, title: String, pane: Pane) -> Block<'static> {
        let block = Block::bordered().title(title);
        if self.focus == pane {
            let accent = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
            block.border_style(accent).title_style(accent)
        } else {
            block.style(Style::new().add_modifier(Modifier::DIM))
        }
    }

    /// The message, else the dialog's keys, else what runs in the background, else
    /// the keys that apply to the selection.
    fn hint_bar(&self) -> Paragraph<'static> {
        if let Some(message) = &self.message {
            return Paragraph::new(message.clone());
        }
        if let (None, Some(progress)) = (&self.dialog, &self.progress) {
            return Paragraph::new(format!("… {progress}"));
        }
        if self.focus == Pane::Terminal && self.dialog.is_none() {
            let bold = Style::new().add_modifier(Modifier::BOLD);
            let keys = vec![
                Span::styled("M-d", bold),
                Span::raw(" dashboard  "),
                Span::styled("M-l/M-h", bold),
                Span::raw(" next/previous session  "),
                Span::styled("M-j/M-k", bold),
                Span::raw(" next/previous window; other keys go to the terminal"),
            ];
            return Paragraph::new(Line::from(keys));
        }
        let hints = match &self.dialog {
            Some(open) => open
                .dialog
                .hints()
                .iter()
                .map(|&(k, l)| (k.to_string(), l))
                .collect(),
            None => self.keymap.hints(self.focus, self.hidden()),
        };
        let mut spans = Vec::new();
        for (key, label) in hints {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(key, Style::new().add_modifier(Modifier::BOLD)));
            spans.push(Span::raw(format!(" {label}")));
        }
        Paragraph::new(Line::from(spans))
    }
}

fn project_paths(config: &Config) -> Vec<PathBuf> {
    config.projects.iter().map(|p| p.path.clone()).collect()
}

/// A config error in one line, for a border, where it may be cut: without the
/// file's path, and from a TOML error, why and then where, leaving out the excerpt.
/// toml's errors start with `TOML parse error at <position>` and end with the reason.
fn one_line(err: &str, path: &std::path::Path) -> String {
    let prefix = format!("{}: ", path.display());
    let err = err.strip_prefix(&prefix).unwrap_or(err);
    let lines: Vec<&str> = err
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    match lines[..] {
        [first, .., last] => {
            let position = first.strip_prefix("TOML parse error at ").unwrap_or(first);
            format!("{last} ({position})")
        }
        [only] => only.to_string(),
        [] => String::new(),
    }
}

/// Errors on a pane's border, bright even in a dim pane.
fn error_style() -> Style {
    Style::new().fg(Color::Red).remove_modifier(Modifier::DIM)
}

/// Seconds since the Unix epoch, like the daemon's activity times.
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
    use crate::model::{ProjectView, Status};
    use crate::protocol;

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

    /// The projects' pane's text inside its borders, on a 100-column screen, where
    /// the left column is 40 wide.
    fn projects_pane(app: &mut App, height: u16) -> Vec<String> {
        // Three more rows for the sessions' pane below, empty: the same room as before it
        let lines = screen(app, 100, height + 3);
        let bottom = lines.iter().position(|l| l.starts_with('└')).unwrap();
        // Taller when the sessions' pane has no room: only empty lines more
        let inner = &lines[1..bottom.min(height as usize - 2)];
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
            "┌ Projects ──────────────────┐┌ Terminal ──────────────────────────────────┐",
            "│▾ app                       ││Enter on a worktree or a session shows its  │",
            "│  · main  repo              ││terminal here                               │",
            "│  · fix-login               ││                                            │",
            "│▾ web                       ││                                            │",
            "│  · main  repo              ││                                            │",
            "└────────────────────────────┘└────────────────────────────────────────────┘",
            "n new  e edit  ? help  q quit",
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
        app.sources.apply(Update::Sessions(Err("no daemon".into())));
        let lines = screen(&mut app, 60, 5);
        assert!(lines[3].starts_with("└ no daemon ─"), "{lines:?}");
    }

    #[test]
    fn without_projects_shows_where_they_go() {
        let mut config = test_config();
        config.path = "/home/me/.config/tatou/config.toml".into();
        let mut app = App::new(Ok(config));
        assert_eq!(
            projects_pane(&mut app, 7),
            [
                "No projects yet: run `tatou add` in a",
                "repository, or add a file to",
                "/home/me/.config/tatou/projects",
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
        assert_eq!(app.focus, Pane::Sessions);
        let lines = screen(&mut app, 80, 12);
        assert_eq!(lines[11], "? help  q quit", "no session to jump to");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Pane::Worktrees, "no third pane");

        // Not into a shown terminal either, and in it Tab is the program's
        app.show("%1");
        app.focus = Pane::Sessions;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Pane::Worktrees);
        app.focus = Pane::Terminal;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Pane::Terminal);
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
            .send(Update::Sessions(Err("daemon: boom".into())))
            .unwrap();
        let failure = actions::Event::Failed("can't find session: $9".to_string());
        failures.send(failure).unwrap();
        app.apply_all(&received, &failed);
        assert_eq!(app.sources.sessions_error(), Some("daemon: boom"));
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
            current: Some("%7".into()),
        });
        showing(vec![app_project])
    }

    fn hint_line(app: &mut App) -> String {
        screen(app, 100, 6)[5].clone()
    }

    #[test]
    fn enter_shows_the_worktrees_terminal_and_gives_it_the_focus() {
        let mut app = with_a_session();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(shown(&app), Some("%7"));
        assert_eq!(app.focus, Pane::Terminal);
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
        assert_eq!(
            hint_line(&mut app),
            "n new  e edit  ? help  q quit",
            "project"
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            hint_line(&mut app),
            "n new  s start agent  t shell  e edit  p PR  ? help  q quit",
            "no session, and the main worktree: no remove"
        );
        press(&mut app, KeyCode::Char('j'));
        assert!(hint_line(&mut app).starts_with("⏎ jump  n new"), "session");
    }

    /// Applies `result` as if `config.toml` had just been loaded again.
    fn reload(app: &mut App, result: std::result::Result<Config, String>) {
        let (updates, received) = mpsc::channel();
        let (_failures, failed) = mpsc::channel();
        updates.send(Update::Config(result)).unwrap();
        app.apply_all(&received, &failed);
    }

    const TOML_ERROR: &str = "/c/config.toml: TOML parse error at line 4, column 1
  |
4 | popop = \"M-g\"
  | ^^^^^
unknown field `popop`, expected one of `popup`, `back`";

    #[test]
    fn an_invalid_reload_keeps_the_last_config_and_shows_why() {
        let mut config = test_config();
        config.path = "/c/config.toml".into();
        let mut app = showing(vec![project("/code/app", &[])]);
        app.config = Ok(config);
        reload(&mut app, Err(TOML_ERROR.into()));
        assert!(app.config.is_ok());
        let lines = screen(&mut app, 200, 5);
        assert!(lines[1].starts_with("│▾ app"), "still listed: {lines:?}");
        assert!(
            // Cut at the pane's width
            lines[3].starts_with("└ config.toml: unknown field `popop`, expected one of"),
            "{lines:?}"
        );
        // Fixed: the error goes away
        reload(&mut app, Ok(test_config()));
        assert_eq!(app.reload_error, None);
        assert!(screen(&mut app, 200, 5)[3].starts_with("└──"));
    }

    #[test]
    fn a_valid_config_replaces_an_invalid_one() {
        let mut app = App::new(Err("config.toml: broken".into()));
        reload(&mut app, Err("config.toml: still broken".into()));
        assert_eq!(
            app.config.as_ref().err().map(String::as_str),
            Some("config.toml: still broken")
        );
        reload(&mut app, Ok(test_config()));
        assert!(app.config.is_ok());
        assert_eq!(app.reload_error, None);
    }

    #[test]
    fn config_errors_fit_on_one_line() {
        let path = std::path::Path::new("/c/config.toml");
        assert_eq!(
            one_line(TOML_ERROR, path),
            "unknown field `popop`, expected one of `popup`, `back` (line 4, column 1)"
        );
        assert_eq!(
            one_line(
                "/c/config.toml: No such file or directory (os error 2)",
                path
            ),
            "No such file or directory (os error 2)"
        );
    }

    #[test]
    fn an_open_dialog_takes_the_keys_while_the_rest_keeps_refreshing() {
        let mut app = showing(vec![project("/code/app", &["fix"])]);
        let form = dialog::Form::new("New worktree", vec![dialog::Field::text("Name", "")]);
        app.dialog = Some(Open {
            dialog: Dialog::Form(form),
            purpose: Purpose::Notice,
        });
        for code in [KeyCode::Char('j'), KeyCode::Char('q'), KeyCode::Tab] {
            press(&mut app, code);
        }
        assert!(!app.quit, "q is typed into the form");
        assert_eq!(app.focus, Pane::Worktrees);
        assert_eq!(
            app.tree.selected().cloned(),
            Some(projects::Row::Project("/code/app".into()))
        );

        let lines = screen(&mut app, 80, 10);
        assert!(lines.iter().any(|l| l.contains("│Name  jq")), "{lines:?}");
        assert!(
            lines[9].starts_with("⏎ confirm  Tab next field"),
            "{lines:?}"
        );
        // Behind it, updates still arrive and are shown
        let (updates, received) = mpsc::channel();
        let (_failures, failed) = mpsc::channel();
        updates
            .send(Update::Sessions(Err("daemon: boom".into())))
            .unwrap();
        app.apply_all(&received, &failed);
        assert_eq!(app.sources.sessions_error(), Some("daemon: boom"));

        press(&mut app, KeyCode::Esc);
        assert!(app.dialog.is_none());
        press(&mut app, KeyCode::Char('j'));
        assert_ne!(
            app.tree.selected().cloned(),
            Some(projects::Row::Project("/code/app".into()))
        );
    }

    /// `app` with `/code/app` as its only project, `base` set or not, its main
    /// worktree on `main`.
    fn with_a_project(base: Option<&str>) -> App {
        let mut config = test_config();
        config.projects.push(config::Project {
            path: "/code/app".into(),
            base: base.map(String::from),
            worktree_dir: None,
            on_create: Some("npm install".into()),
            error: None,
            file: "/c/projects/app.toml".into(),
        });
        let mut app = App::new(Ok(config));
        app.tree.set_view(vec![project("/code/app", &[])]);
        app
    }

    fn form_values(app: &mut App) -> Vec<String> {
        let form = app.new_worktree_form().expect("the new-worktree form");
        form.values().into_iter().map(String::from).collect()
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn event(app: &mut App, event: actions::Event) {
        let (_updates, received) = mpsc::channel();
        let (events, happened) = mpsc::channel();
        events.send(event).unwrap();
        app.apply_all(&received, &happened);
    }

    #[test]
    fn n_opens_the_form_with_the_defaults() {
        let mut app = with_a_project(None);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            form_values(&mut app),
            ["", "main", "claude"],
            "the main worktree's branch"
        );
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|l| l.contains("┌ New worktree in app ")),
            "{lines:?}"
        );

        let mut app = with_a_project(Some("develop"));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            form_values(&mut app),
            ["", "develop", "claude"],
            "the project's base"
        );
    }

    /// The Base the `n` form starts with, with the selection `moves` rows down from
    /// the project, in a project with a `fix` worktree and a detached `spike`.
    fn base_after(base: Option<&str>, remote_default: Option<&str>, moves: usize) -> String {
        let mut app = with_a_project(base);
        let mut view = project("/code/app", &["fix"]);
        view.worktrees
            .push(worktree("/code/app.worktrees/spike", None, Status::Idle));
        view.remote_default = remote_default.map(String::from);
        app.tree.set_view(vec![view]);
        for _ in 0..moves {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('n'));
        form_values(&mut app)[1].clone()
    }

    #[test]
    fn the_base_follows_the_selected_row() {
        // On the project: its base, else the remote's default, else the main branch
        assert_eq!(
            base_after(Some("develop"), Some("origin/main"), 0),
            "develop"
        );
        assert_eq!(base_after(None, Some("origin/main"), 0), "origin/main");
        assert_eq!(base_after(None, None, 0), "main");
        // On a worktree: its branch, the main one's too
        assert_eq!(base_after(Some("develop"), Some("origin/main"), 1), "main");
        assert_eq!(base_after(Some("develop"), Some("origin/main"), 2), "fix");
        // Detached: the project's default
        assert_eq!(base_after(None, Some("origin/main"), 3), "origin/main");
    }

    #[test]
    fn a_name_is_needed() {
        let mut app = with_a_project(None);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "  ");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tasks, []);
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|l| l.contains("A name is needed")),
            "{lines:?}"
        );
    }

    #[test]
    fn the_form_asks_for_the_worktree_and_waits_for_git() {
        let mut app = with_a_project(None);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "fix/login");
        press(&mut app, KeyCode::Enter);
        let expected = NewWorktree {
            project: "/code/app".into(),
            name: "fix/login".into(),
            base: Some("main".into()),
            path: "/code/app.worktrees/fix-login".into(),
            on_create: Some("npm install".into()),
            session: "app-fix/login".into(),
            command: "claude".into(),
        };
        assert_eq!(app.tasks, [Task::NewWorktree(expected)]);
        // Busy: typing changes nothing
        type_text(&mut app, "x");
        assert_eq!(form_values(&mut app)[0], "fix/login");
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|l| l.contains("Creating fix/login…")),
            "{lines:?}"
        );

        // git refuses: the form shows why, and can be edited again
        event(
            &mut app,
            actions::Event::Refused("fatal: a branch named 'fix/login' already exists".into()),
        );
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|l| l.contains("already exists")),
            "{lines:?}"
        );
        type_text(&mut app, "2");
        assert_eq!(form_values(&mut app)[0], "fix/login2");
    }

    #[test]
    fn the_new_worktree_is_selected_once_listed_and_progress_shows_meanwhile() {
        let mut app = with_a_project(None);
        press(&mut app, KeyCode::Char('n'));
        type_text(&mut app, "fix");
        press(&mut app, KeyCode::Enter);
        let path = PathBuf::from("/code/app.worktrees/fix");
        event(
            &mut app,
            actions::Event::Added {
                project: "/code/app".into(),
                path: path.clone(),
            },
        );
        assert!(
            app.dialog.is_none(),
            "the form closes once git has added it"
        );
        event(
            &mut app,
            actions::Event::Progress("fix: running on_create".into()),
        );
        assert_eq!(hint_line(&mut app), "… fix: running on_create");

        app.select_new_worktree();
        assert!(app.select_when_listed.is_some(), "not listed yet");
        // Folded, and listed by the next refresh
        app.tree.collapse();
        app.tree.set_view(vec![project("/code/app", &["fix"])]);
        app.select_new_worktree();
        let selected = app.tree.selected_worktree().map(|w| w.path.clone());
        assert_eq!(selected, Some(path));
        assert!(app.select_when_listed.is_none());

        // Progress done, and the agent shown, with the focus
        event(&mut app, actions::Event::Started("%1".into()));
        assert_eq!(shown(&app), Some("%1"));
        assert!(
            hint_line(&mut app).starts_with("M-d dashboard"),
            "{}",
            hint_line(&mut app)
        );
    }

    #[test]
    fn a_failed_on_create_shows_its_output() {
        let mut app = with_a_project(None);
        event(
            &mut app,
            actions::Event::OnCreateFailed("npm ERR! missing script".into()),
        );
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|l| l.contains("┌ on_create failed ")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("npm ERR! missing script")),
            "{lines:?}"
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.dialog.is_none());
    }

    #[test]
    fn a_project_with_an_error_gets_no_form() {
        let mut app = with_a_project(None);
        let mut broken = project("/code/app", &[]);
        broken.error = Some("fatal: not a git repository\nmore".into());
        app.tree.set_view(vec![broken]);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.dialog.is_none());
        assert_eq!(hint_line(&mut app), "app: fatal: not a git repository");
    }

    /// `with_a_session`, the `fix` worktree selected.
    fn on_fix_with_a_session() -> App {
        let mut app = with_a_session();
        app.config = Ok(test_config());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        app
    }

    #[test]
    fn s_asks_which_agent_then_starts_it_without_a_session() {
        let mut app = with_a_session();
        app.config = Ok(test_config());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('s'));
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|l| l.contains("┌ Agent for main ")),
            "{lines:?}"
        );
        // default_agent is chosen first: Enter keeps it
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        let expected = StartAgent {
            name: "main".into(),
            path: "/code/app".into(),
            session: "app-main".into(),
            command: "codex".into(),
        };
        assert_eq!(app.tasks, [Task::StartAgent(expected)]);
        assert!(app.dialog.is_none());
    }

    #[test]
    fn s_on_a_session_confirms_before_restarting() {
        let mut app = on_fix_with_a_session();
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter);
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Restart claude in fix? [Y/n]")),
            "{lines:?}"
        );
        assert_eq!(app.tasks, [], "not before it is confirmed");
        press(&mut app, KeyCode::Enter);
        let expected = RestartAgent {
            name: "fix".into(),
            path: "/code/app.worktrees/fix".into(),
            session: "$3".into(),
            pane: None,
            command: "claude".into(),
        };
        assert_eq!(app.tasks, [Task::RestartAgent(expected)]);

        // No, or Esc: nothing restarts
        let mut app = on_fix_with_a_session();
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.tasks, []);
        assert!(app.dialog.is_none());
    }

    #[test]
    fn s_and_n_offer_agents_only() {
        let mut app = with_a_session();
        app.config = Ok(test_config());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('s'));
        let lines = screen(&mut app, 80, 14).join("\n");
        assert!(
            lines.contains("│claude") && lines.contains("│codex"),
            "{lines}"
        );
        assert!(!lines.contains("shell"), "{lines}");
        press(&mut app, KeyCode::Esc);

        press(&mut app, KeyCode::Char('n'));
        let mut agents = Vec::new();
        for _ in 0..3 {
            agents.push(form_values(&mut app)[2].clone());
            press(&mut app, KeyCode::Tab);
            press(&mut app, KeyCode::Tab);
            press(&mut app, KeyCode::Right);
            press(&mut app, KeyCode::Tab);
        }
        assert_eq!(agents, ["claude", "codex", "claude"], "two agents, round");
    }

    #[test]
    fn s_does_nothing_on_a_project() {
        let mut app = with_a_session();
        app.config = Ok(test_config());
        press(&mut app, KeyCode::Char('s'));
        assert!(app.dialog.is_none());
    }

    #[test]
    fn t_opens_a_shell_in_the_worktree() {
        let mut app = on_fix_with_a_session();
        press(&mut app, KeyCode::Char('t'));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('t'));
        let expected = [
            Task::Program(Program {
                name: "fix".into(),
                path: "/code/app.worktrees/fix".into(),
                session: Some("$3".into()),
                session_name: "app-fix".into(),
                command: None,
            }),
            Task::Program(Program {
                name: "main".into(),
                path: "/code/app".into(),
                session: None,
                session_name: "app-main".into(),
                command: None,
            }),
        ];
        assert_eq!(app.tasks, expected);
        // Not on a project
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.tasks.len(), 2);
    }

    #[test]
    fn e_opens_the_editor_in_the_worktree_or_the_projects_repository() {
        let mut app = on_fix_with_a_session();
        press(&mut app, KeyCode::Char('e'));
        // On the project: its main worktree
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('e'));
        let expected = [
            Task::Program(Program {
                name: "fix".into(),
                path: "/code/app.worktrees/fix".into(),
                session: Some("$3".into()),
                session_name: "app-fix".into(),
                command: Some(EDITOR.into()),
            }),
            Task::Program(Program {
                name: "main".into(),
                path: "/code/app".into(),
                session: None,
                session_name: "app-main".into(),
                command: Some(EDITOR.into()),
            }),
        ];
        assert_eq!(app.tasks, expected);
        assert!(hint_line(&mut app).contains("e edit"), "on a project");
        // Not from the sessions' pane
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.tasks.len(), 2);
    }

    #[test]
    fn d_asks_before_removing_and_no_is_the_default() {
        let mut app = on_fix_with_a_session();
        press(&mut app, KeyCode::Char('d'));
        let lines = screen(&mut app, 100, 14).join("\n");
        assert!(
            lines.contains("Remove the worktree /code/app.worktrees/fix (branch fix)? Its session"),
            "{lines}"
        );
        assert!(
            lines.contains("app-fix is closed; the branch stays. [y/N]"),
            "{lines}"
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tasks, [], "Enter is no");
        assert!(app.dialog.is_none());

        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        let expected = RemoveWorktree {
            name: "fix".into(),
            project: "/code/app".into(),
            path: "/code/app.worktrees/fix".into(),
            session: Some("$3".into()),
            force: false,
        };
        assert_eq!(app.tasks, [Task::RemoveWorktree(expected)]);
    }

    #[test]
    fn the_main_worktree_is_never_offered_for_removal() {
        let mut app = with_a_session();
        app.config = Ok(test_config());
        press(&mut app, KeyCode::Char('j'));
        assert!(
            !hint_line(&mut app).contains("d remove"),
            "{}",
            hint_line(&mut app)
        );
        press(&mut app, KeyCode::Char('d'));
        assert!(app.dialog.is_none());
        assert_eq!(
            hint_line(&mut app),
            "main is the main worktree: it cannot be removed"
        );
        press(&mut app, KeyCode::Char('j'));
        assert!(hint_line(&mut app).contains("d remove"), "a linked one");
    }

    #[test]
    fn forcing_is_a_second_explicit_question() {
        let mut app = on_fix_with_a_session();
        let remove = RemoveWorktree {
            name: "fix".into(),
            project: "/code/app".into(),
            path: "/code/app.worktrees/fix".into(),
            session: None,
            force: false,
        };
        let why = "fatal: '/code/app.worktrees/fix' contains modified or untracked files, use --force to delete it";
        let refused = || actions::Event::RemoveRefused {
            remove: remove.clone(),
            why: why.into(),
        };
        event(&mut app, refused());
        let lines = screen(&mut app, 120, 14).join("\n");
        assert!(lines.contains("git refused to remove fix"), "{lines}");
        assert!(
            lines.contains("contains modified or untracked files"),
            "{lines}"
        );
        assert!(
            lines.contains("Force the removal? Uncommitted changes and untracked files in it are"),
            "{lines}"
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tasks, [], "never forced by default");

        event(&mut app, refused());
        press(&mut app, KeyCode::Char('y'));
        let forced = RemoveWorktree {
            force: true,
            ..remove.clone()
        };
        assert_eq!(app.tasks, [Task::RemoveWorktree(forced)]);
    }

    #[test]
    fn removal_results_show() {
        let mut app = on_fix_with_a_session();
        event(
            &mut app,
            actions::Event::Removed {
                project: "/code/app".into(),
                name: "fix".into(),
            },
        );
        assert_eq!(hint_line(&mut app), "fix removed; its branch stays");
        let output = "fatal: cannot remove a locked working tree\nuse 'remove -f -f' to override";
        event(
            &mut app,
            actions::Event::Error {
                title: "fix not removed".into(),
                output: output.into(),
            },
        );
        let lines = screen(&mut app, 100, 14).join("\n");
        assert!(lines.contains("┌ fix not removed "), "{lines}");
        assert!(lines.contains("use 'remove -f -f' to override"), "{lines}");
    }

    /// A session in `path`, active at `NOW`, without terminals listed.
    fn daemon_session(id: &str, name: &str, path: &str) -> protocol::Session {
        protocol::Session {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            agent: None,
            activity: NOW,
            windows: Vec::new(),
        }
    }

    /// The terminal the dashboard shows.
    fn shown(app: &App) -> Option<&str> {
        app.shown.as_ref().map(|shown| shown.terminal.as_str())
    }

    const NOW: u64 = 1_000_000;

    /// `app` with its `fix` worktree and `sessions` on the server, as refreshed.
    fn with_sessions(sessions: Vec<protocol::Session>) -> App {
        let mut app = with_a_project(None);
        app.home = Some("/home/me".into());
        let worktrees = vec![
            git::Worktree {
                path: "/code/app".into(),
                head: None,
                branch: Some("main".into()),
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
            },
            git::Worktree {
                path: "/code/app.worktrees/fix".into(),
                head: None,
                branch: Some("fix".into()),
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
            },
        ];
        app.sources.apply(Update::Worktrees {
            project: "/code/app".into(),
            result: Ok(worktrees),
            remote_default: None,
        });
        app.sources.apply(Update::Sessions(Ok(sessions)));
        app.update_view(NOW);
        app
    }

    /// The sessions' pane, inside its borders, on a 100×`height` screen.
    fn sessions_pane(app: &mut App, height: u16) -> Vec<String> {
        let lines = screen(app, 100, height);
        let top = lines
            .iter()
            .position(|l| l.starts_with("┌ Sessions"))
            .expect("the pane");
        lines[top + 1..]
            .iter()
            .take_while(|l| !l.starts_with('└'))
            .map(|l| {
                l.chars()
                    .skip(1)
                    .take(38)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_sessions_pane_lists_every_session() {
        let mut app = with_sessions(vec![
            daemon_session("$1", "app-fix", "/code/app.worktrees/fix"),
            daemon_session("$2", "notes", "/home/me/notes"),
        ]);
        assert_eq!(
            sessions_pane(&mut app, 20),
            ["▾ ● app-fix  app/fix", "▾ ● notes  ~/notes"],
            "its worktree, else its folder"
        );
    }

    #[test]
    fn a_session_shows_what_runs_in_its_current_window() {
        let running = |command: &str| protocol::Session {
            windows: vec![protocol::Window {
                id: "%1".into(),
                index: 0,
                current: true,
                command: command.into(),
            }],
            ..daemon_session("$1", "app-fix", "/code/app.worktrees/fix")
        };
        let mut app = with_sessions(vec![running("claude")]);
        assert_eq!(
            sessions_pane(&mut app, 20),
            ["▾ ● app-fix  claude  app/fix", "    0 claude *"]
        );
        // Another window, or another program: the next refresh says so
        app.sources
            .apply(Update::Sessions(Ok(vec![running("zsh")])));
        app.update_view(NOW);
        assert_eq!(
            sessions_pane(&mut app, 20),
            ["▾ ● app-fix  zsh  app/fix", "    0 zsh *"]
        );
    }

    /// `daemon_session` with windows 0 (`claude`) and 1 (`zsh`, current).
    fn with_windows(id: &str, name: &str, path: &str) -> protocol::Session {
        let window = |n: u32, current, command: &str| protocol::Window {
            id: format!("%{id}{n}").replace('$', ""),
            index: n,
            current,
            command: command.into(),
        };
        protocol::Session {
            windows: vec![window(0, false, "claude"), window(1, true, "zsh")],
            ..daemon_session(id, name, path)
        }
    }

    #[test]
    fn sessions_show_their_windows_as_a_tree() {
        let mut app = with_sessions(vec![
            with_windows("$1", "app-fix", "/code/app.worktrees/fix"),
            with_windows("$2", "notes", "/home/me/notes"),
        ]);
        app.focus = Pane::Sessions;
        let tree = [
            "▾ ● app-fix  zsh  app/fix",
            "    0 claude",
            "    1 zsh *",
            "▾ ● notes  zsh  ~/notes",
            "    0 claude",
            "    1 zsh *",
        ];
        assert_eq!(sessions_pane(&mut app, 40), tree);

        // Fold app-fix: its windows go, and Down goes to notes
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(
            sessions_pane(&mut app, 40)[..2],
            ["▸ ● app-fix  zsh  app/fix", "▾ ● notes  zsh  ~/notes"]
        );
        press(&mut app, KeyCode::Char('l'));
        // l again: into its first window; h from there: back to the session
        press(&mut app, KeyCode::Char('l'));
        let window = sessions::Row::Window {
            session: "$1".into(),
            window: "%10".into(),
        };
        assert_eq!(app.sessions.selected(), Some(&window));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(
            app.sessions.selected(),
            Some(&sessions::Row::Session("$1".into()))
        );
        assert_eq!(
            sessions_pane(&mut app, 40),
            tree,
            "h from a window does not fold"
        );
    }

    #[test]
    fn enter_on_a_window_shows_that_window() {
        let mut app = with_sessions(vec![with_windows(
            "$1",
            "app-fix",
            "/code/app.worktrees/fix",
        )]);
        app.focus = Pane::Sessions;
        // The session's current window
        press(&mut app, KeyCode::Enter);
        assert_eq!(shown(&app), Some("%11"));
        // Window 0, from the sessions' pane again
        app.focus = Pane::Sessions;
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(shown(&app), Some("%10"));
        app.focus = Pane::Sessions;
        press(&mut app, KeyCode::Down);

        // A window opened before it: still on window 1
        let mut more = with_windows("$1", "app-fix", "/code/app.worktrees/fix");
        more.windows.insert(
            0,
            protocol::Window {
                id: "%9".into(),
                index: 0,
                current: false,
                command: "sh".into(),
            },
        );
        app.sources.apply(Update::Sessions(Ok(vec![more])));
        app.update_view(NOW);
        let window = sessions::Row::Window {
            session: "$1".into(),
            window: "%11".into(),
        };
        assert_eq!(app.sessions.selected(), Some(&window));
    }

    #[test]
    fn an_ended_terminal_gives_way_to_its_neighbour() {
        let mut app = with_sessions(vec![
            with_windows("$1", "app-fix", "/code/app.worktrees/fix"),
            with_windows("$2", "notes", "/home/me/notes"),
            with_windows("$3", "zzz", "/tmp"),
        ]);
        // Another terminal of its session: the one after, else before
        app.show("%10");
        app.streamed(Reply::Exited);
        assert_eq!(shown(&app), Some("%11"));
        assert_eq!(app.focus, Pane::Terminal);
        // Its session's last: the next session's current terminal
        app.streamed(Reply::Exited);
        assert_eq!(shown(&app), Some("%21"));
        // The last session's last: the previous session's
        app.show("%30");
        app.streamed(Reply::Exited);
        app.show("%31");
        app.streamed(Reply::Exited);
        assert_eq!(shown(&app), Some("%21"));
        // Nothing left: the pane empties, and the focus goes back
        for _ in 0..2 {
            app.streamed(Reply::Exited);
        }
        assert_eq!(shown(&app), None);
        assert_eq!(app.focus, Pane::Worktrees);
    }

    fn alt(app: &mut App, c: char) {
        app.input(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::ALT,
        )));
    }

    #[test]
    fn alt_l_and_alt_h_step_through_the_sessions() {
        let mut app = with_sessions(vec![
            with_windows("$1", "app-fix", "/code/app.worktrees/fix"),
            with_windows("$2", "notes", "/home/me/notes"),
            with_windows("$3", "zzz", "/tmp"),
        ]);
        // Nothing shown: the first, and the focus goes to its terminal
        alt(&mut app, 'l');
        assert_eq!(shown(&app), Some("%11"), "its current window");
        assert_eq!(app.focus, Pane::Terminal);
        let selected = app.sessions.selected().cloned();
        assert_eq!(selected, Some(sessions::Row::Session("$1".into())));
        // From the terminal, whose program never sees them: the focus stays there
        app.focus = Pane::Terminal;
        alt(&mut app, 'l');
        assert_eq!(shown(&app), Some("%21"));
        assert_eq!(app.focus, Pane::Terminal);
        alt(&mut app, 'l');
        alt(&mut app, 'l');
        assert_eq!(shown(&app), Some("%11"), "round to the first");
        alt(&mut app, 'h');
        assert_eq!(shown(&app), Some("%31"), "back round to the last");
        // From another window of a session, its session counts
        app.show("%20");
        alt(&mut app, 'h');
        assert_eq!(shown(&app), Some("%11"));
    }

    #[test]
    fn alt_j_and_alt_k_step_through_the_shown_sessions_windows() {
        let mut app = with_sessions(vec![
            with_windows("$1", "app-fix", "/code/app.worktrees/fix"),
            with_windows("$2", "notes", "/home/me/notes"),
        ]);
        // Nothing shown: the first session's first window
        alt(&mut app, 'j');
        assert_eq!(shown(&app), Some("%10"));
        assert_eq!(app.focus, Pane::Terminal, "the focus goes to it");
        let selected = app.sessions.selected().cloned();
        let window = |w: &str| sessions::Row::Window {
            session: "$1".into(),
            window: w.into(),
        };
        assert_eq!(selected, Some(window("%10")));
        alt(&mut app, 'j');
        assert_eq!(shown(&app), Some("%11"));
        alt(&mut app, 'j');
        assert_eq!(shown(&app), Some("%10"), "round, within its session");
        alt(&mut app, 'k');
        assert_eq!(shown(&app), Some("%11"));
        // In another session, its windows
        app.show("%20");
        alt(&mut app, 'k');
        assert_eq!(shown(&app), Some("%21"));
    }

    #[test]
    fn without_sessions_the_pane_says_so() {
        let mut app = with_sessions(vec![]);
        assert_eq!(sessions_pane(&mut app, 20), ["No sessions"]);
    }

    #[test]
    fn the_sessions_pane_takes_at_most_a_third_of_the_column() {
        let many = (0..20)
            .map(|n| daemon_session(&format!("${n}"), &format!("s{n:02}"), "/tmp"))
            .collect();
        let mut app = with_sessions(many);
        // 30 rows: a column of 29, so 9 for the pane, 7 inside its borders
        let rows = sessions_pane(&mut app, 30);
        assert_eq!(rows.len(), 7, "{rows:?}");
        // Moving past the last shown row scrolls
        app.focus = Pane::Sessions;
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('j'));
        }
        let rows = sessions_pane(&mut app, 30);
        assert!(rows.iter().any(|r| r.contains("s10")), "{rows:?}");
    }

    #[test]
    fn enter_in_the_sessions_pane_jumps_and_the_selection_follows_its_session() {
        let notes = protocol::Session {
            windows: vec![protocol::Window {
                id: "%2".into(),
                index: 0,
                current: true,
                command: "vim".into(),
            }],
            ..daemon_session("$2", "notes", "/home/me/notes")
        };
        let mut app = with_sessions(vec![
            daemon_session("$1", "app-fix", "/code/app.worktrees/fix"),
            notes,
        ]);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Pane::Sessions);
        assert!(hint_line(&mut app).starts_with("⏎ jump  ? help"));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(shown(&app), Some("%2"));
        // A session listed before it: still on notes
        app.sources.apply(Update::Sessions(Ok(vec![
            daemon_session("$3", "aaa", "/tmp"),
            daemon_session("$1", "app-fix", "/code/app.worktrees/fix"),
            daemon_session("$2", "notes", "/home/me/notes"),
        ])));
        app.update_view(NOW);
        assert_eq!(
            app.sessions.selected(),
            Some(&sessions::Row::Session("$2".into())),
            "still notes"
        );
        // The tree did not move meanwhile
        assert_eq!(
            app.tree.selected().cloned(),
            Some(projects::Row::Project("/code/app".into()))
        );
    }

    #[test]
    fn a_tmux_error_shows_below_the_sessions() {
        let mut app = with_sessions(vec![]);
        app.sources.apply(Update::Sessions(Err("no daemon".into())));
        let lines = screen(&mut app, 60, 20);
        assert!(lines[18].starts_with("└ no daemon ─"), "{lines:?}");
    }

    /// The style of each cell on a `width`×`height` screen, by (x, y).
    fn styles(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn dim(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> bool {
        buffer[(x, y)].modifier.contains(Modifier::DIM)
    }

    fn accent(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> bool {
        let cell = &buffer[(x, y)];
        cell.fg == ACCENT
            && cell.modifier.contains(Modifier::BOLD)
            && !cell.modifier.contains(Modifier::DIM)
    }

    #[test]
    fn the_focused_pane_stands_out_and_the_others_are_dim() {
        let mut app = with_sessions(vec![daemon_session(
            "$1",
            "app-fix",
            "/code/app.worktrees/fix",
        )]);
        // 100×20: Projects at (0, 0), Sessions' top border lower down
        let lines = screen(&mut app, 100, 20);
        let sessions = lines
            .iter()
            .position(|l| l.starts_with("┌ Sessions"))
            .unwrap() as u16;
        let corners = [(0, 0), (0, sessions)];
        let panes = [Pane::Worktrees, Pane::Sessions];
        for (i, focused) in panes.into_iter().enumerate() {
            assert_eq!(app.focus, focused);
            let buffer = styles(&mut app, 100, 20);
            for (j, &(x, y)) in corners.iter().enumerate() {
                if i == j {
                    assert!(accent(&buffer, x, y), "{focused:?}: its border");
                    assert!(accent(&buffer, x + 2, y), "{focused:?}: its title");
                } else {
                    assert!(
                        dim(&buffer, x, y) && !accent(&buffer, x, y),
                        "{focused:?}: {:?}",
                        panes[j]
                    );
                }
            }
            press(&mut app, KeyCode::Tab);
        }
    }

    #[test]
    fn a_dim_pane_dims_its_contents_and_keeps_its_selection() {
        let mut app = with_sessions(vec![daemon_session(
            "$1",
            "app-fix",
            "/code/app.worktrees/fix",
        )]);
        // The selected project row, "▾ app", at (1, 1)
        let buffer = styles(&mut app, 100, 20);
        assert!(!dim(&buffer, 3, 1), "the focused pane's contents");
        press(&mut app, KeyCode::Tab);
        let buffer = styles(&mut app, 100, 20);
        assert!(dim(&buffer, 3, 1), "now dim");
        assert!(
            buffer[(3, 1)].modifier.contains(Modifier::REVERSED),
            "still selected"
        );
    }

    #[test]
    fn errors_stay_bright_in_a_dim_pane() {
        let mut app = with_sessions(vec![]);
        app.sources.apply(Update::Sessions(Err("no daemon".into())));
        // Projects focused: the sessions' pane, with the error, is dim
        let lines = screen(&mut app, 60, 20);
        let y = lines
            .iter()
            .position(|l| l.starts_with("└ no daemon"))
            .unwrap() as u16;
        let buffer = styles(&mut app, 60, 20);
        assert!(dim(&buffer, 0, y), "the border");
        let cell = &buffer[(3, y)];
        assert_eq!(cell.fg, Color::Red);
        assert!(!cell.modifier.contains(Modifier::DIM), "the error");
    }

    fn test_config() -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        config::load_from(&path, None).unwrap()
    }
}
