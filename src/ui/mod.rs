//! The dashboard: ratatui rendering and input handling only.

mod dialog;
mod projects;
mod selection;
mod sessions;
mod terminal;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

use crate::actions::{self, EDITOR, End, NewWorktree, Program, RemoveWorktree, Task};
use crate::client::{Client, Stream};
use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::git;
use crate::keys::{Action, Key, Keymap, Pane};
use crate::model::{self, SessionView, Sources, WorktreeView};
use crate::protocol::{Reply, Request};
use crate::refresh::{ConfigWatch, Refresh, Update};
use crate::term::{self, Modes};
use crate::theme::{self, Theme};
use dialog::{Choice, Confirm, Dialog, Field, Form, Message, Outcome};
use projects::Tree;
use selection::{Selection, Unit};
use sessions::Sessions;

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

/// The width of the Projects and Sessions column; the terminal takes the rest.
const LEFT_WIDTH: u16 = 40;

/// Two clicks on one cell within this are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// How long the pane keeps a size before the shown terminal gets it: one resize
/// at the end of a drag, not one a frame, each followed by its whole screen.
const RESIZE_AFTER: Duration = Duration::from_millis(100);

/// Lines the wheel scrolls the shown terminal's history by, a step.
const WHEEL_LINES: isize = 3;

/// Lines of scrollback kept for the shown terminal: all the daemon has.
const SCROLLBACK: usize = crate::daemon::SCROLLBACK;

/// The worktree actions, hidden from the hint bar on a project row.
const WORKTREE_ACTIONS: [Action; 4] = [
    Action::Jump,
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
        app.tick(Instant::now());
        if let Some(text) = app.clipboard.take() {
            terminal::copy(&text).map_err(Error::Terminal)?;
        }
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

/// Where the panes were last drawn, to find what a click is on; empty when not drawn.
#[derive(Default)]
struct Areas {
    projects: Rect,
    sessions: Rect,
    /// The shown terminal's pane, borders included, and inside them.
    terminal: Rect,
    terminal_inner: Rect,
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
    /// The removal to run once confirmed, forced or not.
    Remove(RemoveWorktree),
    /// The window or session to end once confirmed.
    End(End),
    /// The theme to save in the config at `config`; every theme, loaded, to show
    /// each as it is selected.
    Theme {
        config: PathBuf,
        themes: Vec<(String, std::result::Result<Theme, String>)>,
    },
    /// Only to read, e.g. a command's output.
    Notice,
}

struct App {
    /// The error is shown in place of the projects, until a valid config is loaded.
    config: std::result::Result<Config, String>,
    /// Why the config could not be loaded again; the last valid one stays meanwhile.
    reload_error: Option<String>,
    /// The config's, else the default one; while choosing, the one selected.
    theme: Theme,
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
    areas: Areas,
    /// The last left click: when, on which cell, and how many there were in a row
    /// there, up to three.
    last_click: Option<(Instant, u16, u16, u8)>,
    /// Text selected with the mouse in the shown terminal, highlighted until the
    /// next click, key or output.
    selection: Option<Selection>,
    /// Whether the mouse is still down, extending the selection: output then keeps it.
    selecting: bool,
    /// Text for the dashboard's terminal to copy, with no clipboard tool to.
    clipboard: Option<String>,
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
    /// Whether `slot` changed since the last tick.
    slot_changed: bool,
    /// When to give the shown terminal the pane's size, once it has held.
    resize_due: Option<Instant>,
    /// The size the daemon last gave the shown terminal, and draws its screens at.
    sent: (u16, u16),
    quit: bool,
}

impl App {
    fn new(config: std::result::Result<Config, String>) -> App {
        let theme = match &config {
            Ok(config) => config.theme.clone(),
            Err(_) => theme::terminal(),
        };
        App {
            config,
            reload_error: None,
            theme,
            refresh: None,
            sources: Sources::default(),
            tree: Tree::default(),
            sessions: Sessions::default(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            keymap: Keymap::defaults(),
            focus: Pane::Worktrees,
            dialog: None,
            areas: Areas::default(),
            last_click: None,
            selection: None,
            selecting: false,
            clipboard: None,
            message: None,
            progress: None,
            select_when_listed: None,
            tasks: Vec::new(),
            stream: None,
            shown: None,
            slot: (24, 80),
            slot_changed: false,
            resize_due: None,
            sent: (24, 80),
            quit: false,
        }
    }

    fn input(&mut self, event: Event) {
        if let Event::Paste(text) = &event
            && self.focus == Pane::Terminal
        {
            let bytes = term::paste_bytes(text, self.modes());
            return self.input_to_program(bytes);
        }
        if let Event::Mouse(mouse) = event {
            return self.mouse(mouse, Instant::now());
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
        // A page of the shown terminal's history, from anywhere but a dialog; on the
        // alternate screen, which has none, they are the program's
        let page = (self.slot.0 as isize - 1).max(1);
        let scroll = match (key.code, key.modifiers) {
            (KeyCode::PageUp, KeyModifiers::SHIFT) => Some(page),
            (KeyCode::PageDown, KeyModifiers::SHIFT) => Some(-page),
            _ => None,
        };
        if let (Some(lines), None) = (scroll, &self.dialog)
            && self.scroll(lines)
        {
            return;
        }
        // Every key goes to the shown terminal's program, but these and the one back
        if self.focus == Pane::Terminal && self.dialog.is_none() {
            if (key.code, key.modifiers) == BACK {
                self.focus = Pane::Worktrees;
            } else {
                self.input_to_program(term::key_bytes(key, self.modes()));
            }
            return;
        }
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.message = None;
        if let Some(open) = &mut self.dialog {
            match open.dialog.key(key) {
                Outcome::Open => self.preview_theme(),
                outcome => self.answered(outcome),
            }
            return;
        }
        if let Some(action) = self.keymap.action(Key::from_event(key)) {
            self.handle(action);
        }
    }

    /// A click anywhere in a pane moves the focus there; on a row it selects it, and
    /// a double one acts as Enter. The wheel moves the selection of the pane under
    /// it. In the shown terminal, the mouse goes to its program when it asked for
    /// it; else the wheel is ↑/↓ on the alternate screen, as in an xterm. An open
    /// dialog takes the mouse; `now` times double clicks.
    fn mouse(&mut self, mouse: MouseEvent, now: Instant) {
        let (x, y) = (mouse.column, mouse.row);
        let position = Position::new(x, y);
        let click = mouse.kind == MouseEventKind::Down(MouseButton::Left);
        let mut clicks = 0;
        if click {
            self.message = None;
            clicks = match self.last_click {
                Some((at, cx, cy, n))
                    if (cx, cy) == (x, y) && now.saturating_duration_since(at) <= DOUBLE_CLICK =>
                {
                    n % 3 + 1
                }
                _ => 1,
            };
            self.last_click = Some((now, x, y, clicks));
        }
        let double = clicks == 2;
        let wheel = match mouse.kind {
            MouseEventKind::ScrollUp => Some(KeyCode::Up),
            MouseEventKind::ScrollDown => Some(KeyCode::Down),
            _ => None,
        };
        if let Some(open) = &mut self.dialog {
            let outcome = match (click, wheel) {
                (true, _) => open.dialog.click(position, double),
                // Not in a form, where ↑↓ move between its fields
                (_, Some(code)) if !matches!(open.dialog, Dialog::Form(_)) => {
                    open.dialog.key(KeyEvent::new(code, KeyModifiers::NONE))
                }
                _ => return,
            };
            match outcome {
                Outcome::Open => self.preview_theme(),
                outcome => self.answered(outcome),
            }
            return;
        }
        // A selection follows the mouse wherever it is dragged, and is copied when
        // the mouse goes up
        if self.selection.is_some() {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    let cell = self.cell_at(x, y);
                    if let Some(selection) = &mut self.selection {
                        selection.extend(cell);
                    }
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => return self.copy_selection(),
                _ => {}
            }
        }
        if click {
            self.selection = None;
        }
        if self.areas.terminal.contains(position) {
            if self.shown.is_none() {
                return;
            }
            if click {
                self.focus = Pane::Terminal;
            }
            let inner = self.areas.terminal_inner;
            if !inner.contains(position) {
                return;
            }
            let modes = self.modes();
            // A program that leaves the mouse to the terminal: tatou selects, as a
            // terminal would
            if click && modes.mouse == vt100::MouseProtocolMode::None {
                let unit = match clicks {
                    2 => Unit::Word,
                    3 => Unit::Line,
                    _ => Unit::Cell,
                };
                self.selection = Some(Selection::new(self.cell_at(x, y), unit));
                self.selecting = true;
                return;
            }
            let alternate = self
                .shown
                .as_ref()
                .is_some_and(|s| s.screen.screen().alternate_screen());
            let reading = modes.mouse != vt100::MouseProtocolMode::None;
            let bytes = match wheel {
                // The history, on the main screen
                Some(code) if !reading && !alternate => {
                    let lines = if code == KeyCode::Up {
                        WHEEL_LINES
                    } else {
                        -WHEEL_LINES
                    };
                    self.scroll(lines);
                    return;
                }
                // less, man and the like scroll with the arrows
                Some(code) if !reading => {
                    term::key_bytes(KeyEvent::new(code, KeyModifiers::NONE), modes)
                }
                _ => term::mouse_bytes(mouse, x - inner.x, y - inner.y, modes),
            };
            if !bytes.is_empty() {
                self.input_to_program(bytes);
            }
            return;
        }
        let pane = if self.areas.projects.contains(position) {
            Pane::Worktrees
        } else if self.areas.sessions.contains(position) {
            Pane::Sessions
        } else {
            return;
        };
        match (pane, wheel) {
            (Pane::Worktrees, Some(KeyCode::Up)) => return self.tree.up(),
            (Pane::Worktrees, Some(_)) => return self.tree.down(),
            (_, Some(KeyCode::Up)) => return self.sessions.up(),
            (_, Some(_)) => return self.sessions.down(),
            (_, None) if !click => return,
            _ => {}
        }
        // Anywhere in the pane, its borders and the space below its rows included
        self.focus = pane;
        let hit = match pane {
            Pane::Worktrees => self.tree.click(x, y),
            _ => self.sessions.click(x, y),
        };
        let Some(arrow) = hit else {
            return;
        };
        match (arrow, pane) {
            (true, Pane::Worktrees) => self.tree.toggle(),
            (true, _) => self.sessions.toggle(),
            (false, _) if double => self.handle(Action::Jump),
            _ => {}
        }
    }

    /// Gives the shown terminal the pane's size once it has held for `RESIZE_AFTER`;
    /// `now` is the time.
    fn tick(&mut self, now: Instant) {
        if std::mem::take(&mut self.slot_changed) {
            self.resize_due = Some(now + RESIZE_AFTER);
        }
        if self.resize_due.is_some_and(|due| now >= due) {
            self.resize_due = None;
            if self.shown.is_some() && self.slot != self.sent {
                let (rows, cols) = self.slot;
                self.send(Request::Resize { rows, cols });
                self.sent = self.slot;
            }
        }
    }

    /// Sends `bytes` to the shown terminal's program, the view back at the bottom
    /// first, where they land, and the selection gone.
    fn input_to_program(&mut self, bytes: Vec<u8>) {
        if let Some(shown) = &mut self.shown {
            shown.screen.screen_mut().set_scrollback(0);
        }
        self.selection = None;
        self.send(Request::Input(bytes));
    }

    /// Moves the shown terminal's view `lines` back through its history, forward
    /// when negative, between its start and the screen. False when it has none to
    /// move through: none is shown, or it is on the alternate screen.
    fn scroll(&mut self, lines: isize) -> bool {
        let Some(shown) = &mut self.shown else {
            return false;
        };
        let screen = shown.screen.screen_mut();
        if screen.alternate_screen() {
            return false;
        }
        // Clamped to the history's length by vt100
        screen.set_scrollback(screen.scrollback().saturating_add_signed(lines));
        self.selection = None;
        true
    }

    /// How far back the shown terminal's view is, and how long its history: (0, _)
    /// at the bottom.
    fn scrolled(&mut self) -> (usize, usize) {
        let Some(shown) = &mut self.shown else {
            return (0, 0);
        };
        let screen = shown.screen.screen_mut();
        let at = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let len = screen.scrollback();
        screen.set_scrollback(at);
        (at, len)
    }

    /// The shown terminal's cell at (`x`, `y`), or the nearest one inside it.
    fn cell_at(&self, x: u16, y: u16) -> (u16, u16) {
        let inner = self.areas.terminal_inner;
        let clamp = |value: u16, start: u16, size: u16| {
            value.clamp(start, start + size.saturating_sub(1)) - start
        };
        (
            clamp(y, inner.y, inner.height),
            clamp(x, inner.x, inner.width),
        )
    }

    /// The mouse went up: the selection, unless empty, goes to the clipboard.
    fn copy_selection(&mut self) {
        self.selecting = false;
        let (Some(selection), Some(shown)) = (&self.selection, &self.shown) else {
            return;
        };
        if selection.is_empty() {
            return self.selection = None;
        }
        let text = selection.text(shown.screen.screen());
        if !text.is_empty() {
            self.tasks.push(Task::Copy(text));
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
            Event::Ended => {
                if let Some(refresh) = &self.refresh {
                    refresh.sessions_now();
                }
            }
            Event::Error { title, output } => {
                self.progress = None;
                let message = Dialog::Message(Message::new(&title, &output));
                self.open_over(message, Purpose::Notice, &output);
            }
            // Shown already; the config's reload applies it to every dashboard
            Event::ThemeSaved(name) => {
                self.message = Some(format!("Theme {name}, saved in config.toml"));
            }
            Event::ThemeNotSaved(err) => {
                self.theme = self.configured_theme();
                self.message = Some(format!("The theme is not saved: {err}"));
            }
            Event::Copied(chars) => self.message = Some(format!("Copied {chars} characters")),
            Event::OpenedPr(number) => {
                self.message = Some(format!("#{number} opened in the browser"))
            }
            Event::NoClipboardTool(text) => {
                let chars = text.chars().count();
                self.message = Some(format!(
                    "No clipboard tool: asked the terminal to copy {chars} characters"
                ));
                self.clipboard = Some(text);
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
        if !self.choosing_theme() {
            self.theme = config.theme.clone();
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
            Action::Theme => self.choose_theme(),
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
            Action::Shell if worktrees => self.open(None, false),
            Action::Edit if worktrees => self.open(Some(EDITOR), true),
            Action::OpenPr if worktrees => self.open_pr(),
            Action::RemoveWorktree if worktrees => self.confirm_remove(),
            Action::RemoveWorktree if sessions => self.confirm_end(),
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
        if let Purpose::Theme { .. } = open.purpose {
            return self.theme_answered(outcome, open);
        }
        if outcome != Outcome::Submit {
            return;
        }
        match (open.purpose, open.dialog) {
            (Purpose::NewWorktree { project }, Dialog::Form(form)) => {
                self.create_worktree(project, form);
            }
            (Purpose::Remove(remove), Dialog::Confirm(_)) => {
                self.tasks.push(Task::RemoveWorktree(remove));
            }
            (Purpose::End(end), Dialog::Confirm(_)) => self.tasks.push(Task::End(end)),
            _ => {}
        }
    }

    /// `T`: the themes, the one in use selected.
    fn choose_theme(&mut self) {
        let Ok(config) = &self.config else {
            return self.message = Some("Fix config.toml to choose a theme".to_string());
        };
        let dir = config::themes_dir(&config.path);
        let themes: Vec<_> = theme::names(&dir)
            .into_iter()
            .map(|name| {
                let file = dir.join(format!("{name}.toml"));
                let loaded = theme::load(&dir, &name)
                    .map_err(|err| format!("{name}: {}", one_line(&err, &file)));
                (name, loaded)
            })
            .collect();
        let names = themes.iter().map(|(name, _)| name.clone()).collect();
        let choice = Choice::new("Theme", names, &self.theme.name);
        let purpose = Purpose::Theme {
            config: config.path.clone(),
            themes,
        };
        self.dialog = Some(Open {
            dialog: Dialog::Choice(choice),
            purpose,
        });
    }

    /// The theme dialog's selection, and the theme it shows: `Err` when it is in error.
    fn selected_theme(&self) -> Option<(&Path, std::result::Result<&Theme, &str>)> {
        let Some(Open {
            dialog: Dialog::Choice(choice),
            purpose: Purpose::Theme { config, themes },
        }) = &self.dialog
        else {
            return None;
        };
        let name = choice.selected()?;
        let (_, loaded) = themes.iter().find(|(n, _)| n == name)?;
        Some((config, loaded.as_ref().map_err(String::as_str)))
    }

    fn choosing_theme(&self) -> bool {
        matches!(
            self.dialog,
            Some(Open {
                purpose: Purpose::Theme { .. },
                ..
            })
        )
    }

    /// Shows the theme selected in the theme dialog, or why it cannot be.
    fn preview_theme(&mut self) {
        match self.selected_theme() {
            Some((_, Ok(theme))) => self.theme = theme.clone(),
            Some((_, Err(err))) => self.message = Some(err.to_string()),
            None => {}
        }
    }

    /// Enter saves the theme selected, unless it is in error; Esc puts the theme back.
    fn theme_answered(&mut self, outcome: Outcome, open: Open) {
        self.dialog = Some(open);
        if outcome != Outcome::Submit {
            self.dialog = None;
            self.theme = self.configured_theme();
            return;
        }
        let save = match self.selected_theme() {
            Some((_, Err(err))) => return self.message = Some(err.to_string()),
            Some((config, Ok(theme))) => (config.to_path_buf(), theme.clone()),
            None => return self.dialog = None,
        };
        self.dialog = None;
        let (config, theme) = save;
        let unchanged = theme.name == self.configured_theme().name;
        self.theme = theme;
        if !unchanged {
            let name = self.theme.name.clone();
            self.tasks.push(Task::SaveTheme { config, name });
        }
    }

    /// The theme the config names, else the default one.
    fn configured_theme(&self) -> Theme {
        match &self.config {
            Ok(config) => config.theme.clone(),
            Err(_) => theme::terminal(),
        }
    }

    /// `d` in the sessions' pane: asks before ending the selected window, or session.
    fn confirm_end(&mut self) {
        let Some((end, what)) = self.sessions.selected_end() else {
            return;
        };
        let (title, programs) = match end {
            End::Window(_) => ("End the window", "Its program stops"),
            End::Session(_) => ("End the session", "Its programs stop"),
        };
        let question = format!("End {what}? {programs}; the worktree stays.");
        self.dialog = Some(Open {
            dialog: Dialog::Confirm(Confirm::new(title, &question, false)),
            purpose: Purpose::End(end),
        });
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

    /// `p` on a worktree: its branch's pull request in the browser, through `gh`.
    fn open_pr(&mut self) {
        let (Some(project), Some(worktree)) =
            (self.tree.selected_project(), self.tree.selected_worktree())
        else {
            return;
        };
        match &worktree.pr {
            Some(pr) => self.tasks.push(Task::OpenPr {
                project: project.path.clone(),
                number: pr.number,
            }),
            None => self.message = Some(format!("No pull request for {}", worktree.label())),
        }
    }

    /// The form for a new worktree in the selected row's project: name and base.
    /// On a worktree, the base is its branch; on the project, or a detached
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
        let fields = vec![Field::text("Name", ""), Field::text("Base", &base)];
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

    /// What to create for the form's name and base, or why it cannot be.
    fn new_worktree_request(
        &self,
        project: &Path,
        values: &[String],
    ) -> std::result::Result<NewWorktree, String> {
        let [name, base] = values else {
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
        let project_name = project.file_name().unwrap_or_default().to_string_lossy();
        Ok(NewWorktree {
            project: project.to_path_buf(),
            name: name.to_string(),
            base: (!base.is_empty()).then(|| base.to_string()),
            path: git::worktree_path(project, name, settings.worktree_dir.as_deref()),
            on_create: settings.on_create.clone(),
            session: model::session_name(&project_name, name),
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
            // `t` opens a shell there, `n` a new worktree with one
            Some(worktree) => {
                self.message = Some(format!("No session for {} yet", worktree.label()));
            }
            None => self.tree.toggle(),
        }
    }

    /// The actions that do not apply to the selected row.
    fn hidden(&self) -> Vec<Action> {
        if self.focus == Pane::Sessions && self.sessions.selected().is_none() {
            return vec![Action::Jump, Action::RemoveWorktree];
        }
        if self.focus != Pane::Worktrees {
            return Vec::new();
        }
        let Some(worktree) = self.tree.selected_worktree() else {
            return WORKTREE_ACTIONS.to_vec();
        };
        let mut hidden = Vec::new();
        if worktree.session.is_none() {
            hidden.push(Action::Jump);
        }
        if worktree.pr.is_none() {
            hidden.push(Action::OpenPr);
        }
        if worktree.main {
            hidden.push(Action::RemoveWorktree);
        }
        hidden
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
        self.sent = self.slot;
        self.resize_due = None;
        self.shown = Some(Shown {
            terminal: terminal.to_string(),
            screen: vt100::Parser::new(rows, cols, SCROLLBACK),
        });
        self.selection = None;
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
            // On attach, and after each resize: the daemon's screen, at its size
            (Reply::Screen(bytes), Some(shown)) => {
                let (rows, cols) = self.sent;
                shown.screen = vt100::Parser::new(rows, cols, SCROLLBACK);
                shown.screen.process(&bytes);
                self.selection = None;
            }
            // What was selected may have moved; kept while the mouse is still down
            (Reply::Output(bytes), Some(shown)) => {
                shown.screen.process(&bytes);
                if !self.selecting {
                    self.selection = None;
                }
            }
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
                mouse: shown.screen.screen().mouse_protocol_mode(),
                mouse_encoding: shown.screen.screen().mouse_protocol_encoding(),
            },
            None => Modes::default(),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        frame.render_widget(Block::new().style(self.theme.base()), frame.area());
        let [main, hints] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        // Half the screen at most, so that the terminal always shows
        let width = LEFT_WIDTH.min(main.width / 2);
        let [left, right] =
            Layout::horizontal([Constraint::Length(width), Constraint::Min(1)]).areas(main);
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
        self.areas.projects = projects;
        self.areas.sessions = if height > 0 {
            sessions
        } else {
            Rect::default()
        };
        frame.render_widget(self.hint_bar(), hints);
        if let Some(open) = &mut self.dialog {
            open.dialog.render(frame, main, &self.theme);
        }
    }

    /// The shown terminal, at the pane's size, which the daemon follows; what to do
    /// when none is shown.
    fn render_terminal(&mut self, frame: &mut Frame, area: Rect) {
        let (back, history) = self.scrolled();
        let title = match &self.shown {
            Some(shown) if back > 0 => {
                let name = self.sessions.title(&shown.terminal);
                format!(" {name} · ↑ {back} of {history} ")
            }
            Some(shown) => format!(" {} ", self.sessions.title(&shown.terminal)),
            None => " Terminal ".to_string(),
        };
        let block = self.pane_block_titled(title, Pane::Terminal);
        let inner = block.inner(area);
        self.areas.terminal = area;
        self.areas.terminal_inner = inner;
        let (rows, cols) = crate::daemon::MIN_SIZE;
        let size = (inner.height.max(rows), inner.width.max(cols));
        // The daemon resizes it once the size holds, then sends its screen anew
        if size != self.slot {
            self.slot = size;
            self.slot_changed = true;
        }
        match &self.shown {
            Some(shown) => {
                let mut cursor = tui_term::widget::Cursor::default();
                if self.focus != Pane::Terminal || back > 0 {
                    cursor.hide();
                }
                let widget = PseudoTerminal::new(shown.screen.screen())
                    .block(block)
                    .cursor(cursor);
                frame.render_widget(widget, area);
                self.paint_defaults(frame, inner);
                self.paint_selection(frame, inner);
            }
            None => {
                let text = "Enter on a worktree or a session shows its terminal here";
                let body = Paragraph::new(text).wrap(Wrap { trim: false });
                frame.render_widget(body.block(block), area);
            }
        }
    }

    /// The theme's colours in the cells of `area` its program left in the default
    /// ones; those it set itself are kept.
    fn paint_defaults(&self, frame: &mut Frame, area: Rect) {
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        if (fg, bg) == (Color::Reset, Color::Reset) {
            return;
        }
        let buffer = frame.buffer_mut();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut buffer[(x, y)];
                if cell.fg == Color::Reset {
                    cell.fg = fg;
                }
                if cell.bg == Color::Reset {
                    cell.bg = bg;
                }
            }
        }
    }

    /// The selected cells of the shown terminal, drawn in `area`, as the theme's
    /// selected row.
    fn paint_selection(&self, frame: &mut Frame, area: Rect) {
        let (Some(selection), Some(shown)) = (&self.selection, &self.shown) else {
            return;
        };
        if selection.is_empty() {
            return;
        }
        let bounds = selection.bounds(shown.screen.screen());
        let style = self.theme.selected();
        let buffer = frame.buffer_mut();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if Selection::contains(bounds, (y - area.y, x - area.x)) {
                    buffer[(x, y)].set_style(style);
                }
            }
        }
    }

    /// Every session the daemon has; its error at the bottom.
    fn render_sessions(&mut self, frame: &mut Frame, area: Rect) {
        let mut block = self.pane_block(" Sessions ", Pane::Sessions);
        if let Some(err) = self.sources.sessions_error() {
            let style = error_style(&self.theme);
            block = block.title_bottom(Line::styled(format!(" {err} "), style));
        }
        self.sessions.render(frame, area, block, &self.theme);
    }

    /// The projects, else why there are none; a reload error at the bottom, and
    /// the sessions' error with `sessions_error`.
    fn render_projects(&mut self, frame: &mut Frame, area: Rect, sessions_error: bool) {
        let mut block = self.pane_block(" Projects ", Pane::Worktrees);
        let style = error_style(&self.theme);
        if let (Some(err), Ok(config)) = (&self.reload_error, &self.config) {
            let err = one_line(err, &config.path);
            block = block.title_bottom(Line::styled(format!(" config.toml: {err} "), style));
        }
        if let Some(err) = self.sources.sessions_error().filter(|_| sessions_error) {
            block = block.title_bottom(Line::styled(format!(" {err} "), style));
        }
        let message = match &self.config {
            Ok(_) if !self.tree.is_empty() => {
                return self.tree.render(frame, area, block, &self.theme);
            }
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
            let accent = Style::new()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD);
            block.border_style(accent).title_style(accent)
        } else {
            block.style(Style::new().add_modifier(Modifier::DIM))
        }
    }

    /// The message, else the dialog's keys, else what runs in the background, else
    /// the keys that apply to the selection.
    fn hint_bar(&mut self) -> Paragraph<'static> {
        if let Some(message) = &self.message {
            return Paragraph::new(message.clone());
        }
        if let (None, Some(progress)) = (&self.dialog, &self.progress) {
            return Paragraph::new(format!("… {progress}"));
        }
        if self.focus == Pane::Terminal && self.dialog.is_none() {
            let bold = self.key_style();
            let rest = match self.scrolled().0 {
                0 => "; other keys go to the terminal",
                _ => "; any key: back to the bottom",
            };
            let keys = vec![
                Span::styled("M-d", bold),
                Span::raw(" dashboard  "),
                Span::styled("M-l/M-h", bold),
                Span::raw(" session  "),
                Span::styled("M-j/M-k", bold),
                Span::raw(" window  "),
                Span::styled("S-PgUp/PgDn", bold),
                Span::raw(" scroll"),
                Span::raw(rest),
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
            None => self.keymap.hints(self.focus, &self.hidden()),
        };
        let mut spans = Vec::new();
        for (key, label) in hints {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(key, self.key_style()));
            spans.push(Span::raw(format!(" {label}")));
        }
        Paragraph::new(Line::from(spans))
    }

    /// Keys in the hint bar.
    fn key_style(&self) -> Style {
        Style::new().fg(self.theme.key).add_modifier(Modifier::BOLD)
    }
}

fn project_paths(config: &Config) -> Vec<PathBuf> {
    config.projects.iter().map(|p| p.path.clone()).collect()
}

/// The index of the list item drawn on line `y`, in a list drawn in `area` from
/// item `offset` on, its items `heights` lines tall; `None` past the last one.
fn row_at(heights: &[u16], offset: usize, area: Rect, y: u16) -> Option<usize> {
    if y < area.y || y >= area.bottom() {
        return None;
    }
    let mut top = area.y;
    for (index, &height) in heights.iter().enumerate().skip(offset) {
        if y < top + height {
            return Some(index);
        }
        top += height;
    }
    None
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
fn error_style(theme: &Theme) -> Style {
    Style::new().fg(theme.error).remove_modifier(Modifier::DIM)
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
    use std::sync::mpsc::Receiver;

    use crate::client::Stream;
    use crate::gh;
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
        // The left column 40 wide, the terminal the rest
        let expected = [
            "┌ Projects ────────────────────────────┐┌ Terminal ──────────────────────────────────────┐",
            "│▾ app                                 ││Enter on a worktree or a session shows its      │",
            "│  · main  repo                        ││terminal here                                   │",
            "│  · fix-login                         ││                                                │",
            "│▾ web                                 ││                                                │",
            "│  · main  repo                        ││                                                │",
            "└──────────────────────────────────────┘└────────────────────────────────────────────────┘",
            "n new  e edit  T theme  ? help  q quit",
        ];
        assert_eq!(screen(&mut app, 90, 8), expected);
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

    fn pr(number: u32, state: gh::State, checks: Option<gh::Checks>) -> gh::Pr {
        gh::Pr {
            number,
            state,
            checks,
        }
    }

    #[test]
    fn each_pull_request_has_its_badge_against_the_right_edge() {
        use gh::{Checks, State};
        let badges = [
            ("passing", pr(48, State::Open, Some(Checks::Passed))),
            ("pending", pr(46, State::Open, Some(Checks::Pending))),
            ("failing", pr(47, State::Open, Some(Checks::Failed))),
            ("no-checks", pr(42, State::Open, None)),
            ("wip", pr(53, State::Draft, Some(Checks::Passed))),
            ("shipped", pr(52, State::Merged, Some(Checks::Passed))),
            ("abandoned", pr(51, State::Closed, Some(Checks::Failed))),
            (
                "feature/a-very-long-branch-name",
                pr(128, State::Open, Some(Checks::Failed)),
            ),
        ];
        let mut app_project = project("/code/app", &[]);
        app_project
            .worktrees
            .extend(badges.into_iter().map(|(name, pr)| {
                let mut worktree = worktree(&format!("/code/{name}"), Some(name), Status::Quiet);
                worktree.pr = Some(pr);
                worktree
            }));
        let mut app = showing(vec![app_project]);
        assert_eq!(
            projects_pane(&mut app, 13)[1..],
            [
                "  · main  repo",
                "  ○ passing                     #48 ✓",
                "  ○ pending                     #46 ●",
                "  ○ failing                     #47 ✗",
                "  ○ no-checks                     #42",
                "  ○ wip                   #53 draft ✓",
                "  ○ shipped                #52 merged",
                "  ○ abandoned              #51 closed",
                "  ○ feature/a-very-long-bran…  #128 ✗",
            ]
        );
    }

    #[test]
    fn p_opens_the_pull_request_or_says_there_is_none() {
        let mut app = with_a_session();
        app.tree.set_view({
            let mut app_project = project("/code/app", &["fix"]);
            app_project.worktrees[1].pr = Some(pr(42, gh::State::Open, None));
            vec![app_project]
        });
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.tasks, []);
        assert_eq!(hint_line(&mut app), "No pull request for main");

        press(&mut app, KeyCode::Char('j'));
        assert!(
            hint_line(&mut app).contains("p PR"),
            "{}",
            hint_line(&mut app)
        );
        press(&mut app, KeyCode::Char('p'));
        let expected = Task::OpenPr {
            project: "/code/app".into(),
            number: 42,
        };
        assert_eq!(app.tasks, [expected]);
        event(&mut app, actions::Event::OpenedPr(42));
        assert_eq!(hint_line(&mut app), "#42 opened in the browser");
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
        assert_eq!(
            lines[11], "T theme  ? help  q quit",
            "no session to jump to"
        );
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
            "n new  e edit  T theme  ? help  q quit",
            "project"
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            hint_line(&mut app),
            "n new  t shell  e edit  T theme  ? help  q quit",
            "no session, no pull request, and the main worktree: no remove"
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
            lines[3].starts_with("└ config.toml: unknown field `popop`, e┘"),
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
            ["", "main"],
            "the main worktree's branch"
        );
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines.iter().any(|l| l.contains("┌ New worktree in app ")),
            "{lines:?}"
        );

        let mut app = with_a_project(Some("develop"));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(form_values(&mut app), ["", "develop"], "the project's base");
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

        // Progress done, and the shell shown, with the focus
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

    fn daemon_session(id: &str, name: &str, path: &str) -> protocol::Session {
        protocol::Session {
            id: id.into(),
            name: name.into(),
            path: path.into(),
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
    fn the_left_column_keeps_its_width_and_the_terminal_takes_the_rest() {
        let mut app = showing(vec![project("/code/app", &[])]);
        for width in [100, 200] {
            let top = &screen(&mut app, width, 6)[0];
            let left: String = top.chars().take_while(|c| *c != '┐').collect();
            assert_eq!(left.chars().count() + 1, 40, "{width}: {top}");
            assert_eq!(
                top.chars().count(),
                width as usize,
                "the terminal to the edge"
            );
        }
        // Narrower than 80: half each, so that the terminal still shows
        let top = &screen(&mut app, 60, 6)[0];
        let left: String = top.chars().take_while(|c| *c != '┐').collect();
        assert_eq!(left.chars().count() + 1, 30, "{top}");
    }

    #[test]
    fn d_ends_the_selected_window_or_session_once_confirmed() {
        let mut app = with_sessions(vec![with_windows(
            "$1",
            "app-fix",
            "/code/app.worktrees/fix",
        )]);
        app.focus = Pane::Sessions;
        assert!(
            hint_line(&mut app).contains("d end"),
            "{}",
            hint_line(&mut app)
        );
        // The session: all its windows, after a yes
        press(&mut app, KeyCode::Char('d'));
        let lines = screen(&mut app, 100, 14).join("\n");
        assert!(lines.contains("End the session"), "{lines}");
        assert!(
            lines.contains("app-fix and its 2 windows (claude, zsh)"),
            "{lines}"
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.tasks, [], "no by default");
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.tasks, [Task::End(End::Session("$1".into()))]);
        // A window
        app.tasks.clear();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('d'));
        let lines = screen(&mut app, 100, 14).join("\n");
        assert!(lines.contains("claude in app-fix:0"), "{lines}");
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.tasks, [Task::End(End::Window("%10".into()))]);
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
        assert!(hint_line(&mut app).starts_with("⏎ jump  d end  T theme  ? help"));
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
        cell.fg == Color::Cyan
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

    #[test]
    fn a_theme_colours_the_dashboard_and_the_terminals_default_cells() {
        let mut app = with_sessions(vec![daemon_session(
            "$1",
            "app-fix",
            "/code/app.worktrees/fix",
        )]);
        let dir = tempfile::tempdir().unwrap();
        app.theme = theme::load(dir.path(), "monokai-pro").unwrap();
        let mut screen = vt100::Parser::new(18, 58, SCROLLBACK);
        screen.process(b"\x1b[31mred\x1b[0m plain");
        app.shown = Some(Shown {
            terminal: "%7".into(),
            screen,
        });
        let (background, foreground) = (Color::Rgb(0x2d, 0x2a, 0x2e), Color::Rgb(0xfc, 0xfc, 0xfa));
        let buffer = styles(&mut app, 100, 20);
        assert_eq!(
            buffer[(0, 0)].fg,
            Color::Rgb(0xff, 0xd8, 0x66),
            "the accent"
        );
        assert_eq!(
            buffer[(3, 1)].bg,
            Color::Rgb(0x5b, 0x59, 0x5c),
            "the selection"
        );
        assert!(!buffer[(3, 1)].modifier.contains(Modifier::REVERSED));
        assert_eq!(buffer[(99, 19)].bg, background, "the hint bar");
        // The terminal's inside starts at (41, 1)
        let red = &buffer[(41, 1)];
        assert_eq!(
            (red.fg, red.bg),
            (Color::Indexed(1), background),
            "its program's colour"
        );
        let plain = &buffer[(45, 1)];
        assert_eq!(
            (plain.fg, plain.bg),
            (foreground, background),
            "the defaults"
        );
    }

    #[test]
    fn the_default_theme_leaves_the_terminals_colours() {
        let mut app = with_sessions(vec![]);
        let mut screen = vt100::Parser::new(18, 58, SCROLLBACK);
        screen.process(b"plain");
        app.shown = Some(Shown {
            terminal: "%7".into(),
            screen,
        });
        let buffer = styles(&mut app, 100, 20);
        let plain = &buffer[(41, 1)];
        assert_eq!((plain.fg, plain.bg), (Color::Reset, Color::Reset));
    }

    fn mouse_at(app: &mut App, kind: MouseEventKind, x: u16, y: u16, now: Instant) {
        let event = MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        app.mouse(event, now);
    }

    const CLICK: MouseEventKind = MouseEventKind::Down(MouseButton::Left);

    /// A click, long after any other one.
    fn click(app: &mut App, x: u16, y: u16) {
        app.last_click = None;
        mouse_at(app, CLICK, x, y, Instant::now());
    }

    #[test]
    fn row_at_counts_each_items_lines_from_the_scroll_offset() {
        let area = Rect::new(1, 5, 30, 4);
        let heights = [1, 3, 1, 1, 1];
        let rows: Vec<_> = (4..10).map(|y| row_at(&heights, 1, area, y)).collect();
        assert_eq!(rows, [None, Some(1), Some(1), Some(1), Some(2), None]);
        assert_eq!(row_at(&[1, 1], 0, area, 7), None, "past the last item");
    }

    #[test]
    fn a_click_selects_a_row_and_moves_the_focus_to_its_pane() {
        let mut app = with_sessions(vec![daemon_session(
            "$1",
            "app-fix",
            "/code/app.worktrees/fix",
        )]);
        press(&mut app, KeyCode::Tab);
        let lines = screen(&mut app, 100, 20);
        // "▾ app", "· main  repo", "· fix" from (1, 1) down
        click(&mut app, 8, 3);
        assert_eq!(app.focus, Pane::Worktrees);
        let selected = app.tree.selected_worktree().map(|w| w.path.clone());
        assert_eq!(selected, Some("/code/app.worktrees/fix".into()));

        let top = lines
            .iter()
            .position(|l| l.starts_with("┌ Sessions"))
            .unwrap();
        click(&mut app, 8, top as u16 + 1);
        assert_eq!(app.focus, Pane::Sessions);
        assert_eq!(
            app.sessions.selected(),
            Some(&sessions::Row::Session("$1".into()))
        );
        let selected = app.tree.selected().cloned();
        click(&mut app, 8, 8);
        assert_eq!(app.focus, Pane::Worktrees, "below the rows");
        assert_eq!(
            app.tree.selected().cloned(),
            selected,
            "nothing selected there"
        );
        click(&mut app, 20, top as u16);
        assert_eq!(app.focus, Pane::Sessions, "on its border");
        click(&mut app, 39, 4);
        assert_eq!(app.focus, Pane::Worktrees, "on the other border");
    }

    #[test]
    fn a_project_with_an_error_takes_its_lines_and_a_scrolled_list_its_offset() {
        let mut broken = project("/code/broken", &[]);
        broken.worktrees.clear();
        broken.error = Some("fatal: not a git repository\nhint: check the path".into());
        let mut app = showing(vec![broken, project("/code/web", &["a", "b", "c"])]);
        screen(&mut app, 100, 20);
        click(&mut app, 8, 2);
        assert_eq!(
            app.tree.selected(),
            Some(&projects::Row::Project("/code/broken".into()))
        );
        click(&mut app, 8, 5);
        let main = app.tree.selected_worktree().map(|w| w.path.clone());
        assert_eq!(main, Some("/code/web".into()), "under ▾ web");

        // Scrolled to the last worktree: the project with its error is out of sight
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        let rows = projects_pane(&mut app, 6);
        assert_eq!(rows[0], "▾ web", "{rows:?}");
        click(&mut app, 8, 1);
        assert_eq!(
            app.tree.selected(),
            Some(&projects::Row::Project("/code/web".into()))
        );
    }

    #[test]
    fn a_double_click_does_what_enter_does() {
        let mut app = with_a_session();
        screen(&mut app, 100, 20);
        let start = Instant::now();
        let later = |ms| start + Duration::from_millis(ms);
        mouse_at(&mut app, CLICK, 8, 3, start);
        mouse_at(&mut app, CLICK, 8, 3, later(500));
        assert_eq!(shown(&app), None, "too slow");
        mouse_at(&mut app, CLICK, 9, 3, later(600));
        assert_eq!(shown(&app), None, "another cell");
        mouse_at(&mut app, CLICK, 9, 3, later(700));
        assert_eq!(shown(&app), Some("%7"));
        assert_eq!(app.focus, Pane::Terminal);
    }

    #[test]
    fn a_click_on_a_projects_arrow_folds_it() {
        let mut app = with_a_session();
        screen(&mut app, 100, 20);
        click(&mut app, 1, 1);
        assert_eq!(projects_pane(&mut app, 6)[..2], ["▸ app", ""]);
        click(&mut app, 1, 1);
        assert_eq!(projects_pane(&mut app, 6)[1], "  · main  repo");
        assert_eq!(shown(&app), None);
    }

    #[test]
    fn the_wheel_moves_the_selection_under_it_and_not_the_focus() {
        let mut app = with_a_session();
        press(&mut app, KeyCode::Tab);
        screen(&mut app, 100, 20);
        let now = Instant::now();
        mouse_at(&mut app, MouseEventKind::ScrollDown, 20, 5, now);
        mouse_at(&mut app, MouseEventKind::ScrollDown, 20, 5, now);
        let selected = app.tree.selected_worktree().map(|w| w.path.clone());
        assert_eq!(selected, Some("/code/app.worktrees/fix".into()));
        assert_eq!(app.focus, Pane::Sessions);
    }

    #[test]
    fn the_mouse_goes_to_the_terminals_program_when_it_asked() {
        let mut app = with_sessions(vec![]);
        let (stream, requests) = Stream::testing();
        app.stream = Some(stream);
        app.shown = Some(Shown {
            terminal: "%7".into(),
            screen: vt100::Parser::new(18, 58, SCROLLBACK),
        });
        let inputs = || -> Vec<Vec<u8>> {
            let requests = requests.try_iter();
            requests
                .filter_map(|request| match request {
                    Request::Input(bytes) => Some(bytes),
                    _ => None,
                })
                .collect()
        };
        screen(&mut app, 100, 20);
        // The terminal's inside starts at (41, 1)
        click(&mut app, 70, 0);
        assert_eq!(app.focus, Pane::Terminal, "on its border");
        click(&mut app, 8, 8);
        assert_eq!(app.focus, Pane::Worktrees, "back, from the terminal");
        click(&mut app, 45, 2);
        assert_eq!(app.focus, Pane::Terminal);
        let now = Instant::now();
        mouse_at(&mut app, MouseEventKind::ScrollUp, 45, 2, now);
        assert!(inputs().is_empty(), "not asked, on the main screen");

        let shown = app.shown.as_mut().unwrap();
        shown.screen.process(b"\x1b[?1049h\x1b[?1h");
        mouse_at(&mut app, MouseEventKind::ScrollUp, 45, 2, now);
        mouse_at(&mut app, MouseEventKind::ScrollDown, 45, 2, now);
        let arrows: Vec<&[u8]> = vec![b"\x1bOA", b"\x1bOB"];
        assert_eq!(inputs(), arrows, "the alternate screen's wheel, as arrows");

        let shown = app.shown.as_mut().unwrap();
        shown.screen.process(b"\x1b[?1000h\x1b[?1006h");
        click(&mut app, 45, 2);
        mouse_at(&mut app, MouseEventKind::ScrollUp, 41, 1, now);
        mouse_at(&mut app, MouseEventKind::Moved, 42, 1, now);
        click(&mut app, 70, 0);
        let sent: Vec<&[u8]> = vec![b"\x1b[<0;5;2M", b"\x1b[<64;1;1M"];
        assert_eq!(inputs(), sent, "no motion in mode 1000, nor the border");
    }

    /// A shell's screen shown, "one two" and "three four", its inside from (41, 1).
    fn with_a_shell() -> App {
        let mut app = with_sessions(vec![]);
        let mut screen = vt100::Parser::new(18, 58, SCROLLBACK);
        screen.process(b"one two\r\nthree four");
        app.shown = Some(Shown {
            terminal: "%7".into(),
            screen,
        });
        screen_of(&mut app);
        app
    }

    fn screen_of(app: &mut App) -> ratatui::buffer::Buffer {
        styles(app, 100, 20)
    }

    fn selected(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> bool {
        buffer[(x, y)].modifier.contains(Modifier::REVERSED)
    }

    const UP: MouseEventKind = MouseEventKind::Up(MouseButton::Left);
    const DRAG: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);

    #[test]
    fn a_drag_in_a_shell_selects_and_copies_on_release() {
        let mut app = with_a_shell();
        let now = Instant::now();
        mouse_at(&mut app, CLICK, 45, 1, now);
        assert_eq!(app.focus, Pane::Terminal);
        assert!(
            !selected(&screen_of(&mut app), 45, 1),
            "a click selects nothing"
        );
        mouse_at(&mut app, DRAG, 43, 2, now);
        let buffer = screen_of(&mut app);
        assert!(
            selected(&buffer, 45, 1) && selected(&buffer, 98, 1),
            "to the line's end"
        );
        assert!(selected(&buffer, 41, 2) && selected(&buffer, 43, 2));
        assert!(!selected(&buffer, 44, 1) && !selected(&buffer, 44, 2));
        mouse_at(&mut app, UP, 43, 2, now);
        assert_eq!(app.tasks, [Task::Copy("two\nthr".into())]);
        assert!(
            selected(&screen_of(&mut app), 45, 1),
            "highlighted until the next click"
        );

        event(&mut app, actions::Event::Copied(7));
        assert_eq!(app.message.as_deref(), Some("Copied 7 characters"));
        press(&mut app, KeyCode::Char('x'));
        assert!(app.selection.is_none(), "a key to the program clears it");

        app.tasks.clear();
        mouse_at(&mut app, CLICK, 45, 1, now + Duration::from_secs(1));
        mouse_at(&mut app, UP, 45, 1, now);
        assert_eq!(app.tasks, [], "a click alone copies nothing");
        // Dragged off the pane: up to its edge
        mouse_at(&mut app, CLICK, 42, 2, now + Duration::from_secs(2));
        mouse_at(&mut app, DRAG, 20, 0, now);
        mouse_at(&mut app, UP, 20, 0, now);
        assert_eq!(app.tasks, [Task::Copy("one two\nth".into())]);
    }

    #[test]
    fn a_double_click_selects_a_word_and_a_triple_one_the_line() {
        let mut app = with_a_shell();
        let now = Instant::now();
        let soon = |ms| now + Duration::from_millis(ms);
        mouse_at(&mut app, CLICK, 46, 1, now);
        mouse_at(&mut app, UP, 46, 1, now);
        mouse_at(&mut app, CLICK, 46, 1, soon(100));
        mouse_at(&mut app, UP, 46, 1, soon(100));
        assert_eq!(app.tasks, [Task::Copy("two".into())]);
        mouse_at(&mut app, CLICK, 46, 1, soon(200));
        mouse_at(&mut app, UP, 46, 1, soon(200));
        assert_eq!(app.tasks[1], Task::Copy("one two".into()));
        assert!(selected(&screen_of(&mut app), 41, 1), "the whole line");
    }

    #[test]
    fn a_program_that_reads_the_mouse_selects_itself() {
        let mut app = with_a_shell();
        let shown = app.shown.as_mut().unwrap();
        shown.screen.process(b"\x1b[?1000h");
        let now = Instant::now();
        mouse_at(&mut app, CLICK, 45, 1, now);
        mouse_at(&mut app, DRAG, 43, 2, now);
        mouse_at(&mut app, UP, 43, 2, now);
        assert!(app.selection.is_none());
        assert_eq!(app.tasks, []);
    }

    #[test]
    fn output_clears_the_selection_once_the_mouse_is_up() {
        let mut app = with_a_shell();
        let now = Instant::now();
        mouse_at(&mut app, CLICK, 45, 1, now);
        mouse_at(&mut app, DRAG, 47, 1, now);
        app.streamed(Reply::Output(b"!".to_vec()));
        assert!(app.selection.is_some(), "still dragging");
        mouse_at(&mut app, UP, 47, 1, now);
        app.streamed(Reply::Output(b"!".to_vec()));
        assert!(app.selection.is_none());
    }

    #[test]
    fn without_a_clipboard_tool_the_terminal_is_asked_to_copy() {
        let mut app = with_a_shell();
        event(&mut app, actions::Event::NoClipboardTool("two".into()));
        assert_eq!(app.clipboard.as_deref(), Some("two"));
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("asked the terminal")
        );
    }

    /// A shell that printed 1 to 100 on 18 rows, 83 lines of history, shown from
    /// (41, 1) on the pane's 17, a page of 16 lines; and what it is sent.
    fn with_a_long_shell() -> (App, Receiver<Request>) {
        let mut app = with_sessions(vec![]);
        let (stream, requests) = Stream::testing();
        app.stream = Some(stream);
        let mut screen = vt100::Parser::new(18, 58, SCROLLBACK);
        for n in 1..=100 {
            screen.process(format!("{n}\r\n").as_bytes());
        }
        app.shown = Some(Shown {
            terminal: "%7".into(),
            screen,
        });
        screen_of(&mut app);
        (app, requests)
    }

    fn inputs(requests: &Receiver<Request>) -> Vec<Vec<u8>> {
        let requests = requests.try_iter();
        requests
            .filter_map(|request| match request {
                Request::Input(bytes) => Some(bytes),
                _ => None,
            })
            .collect()
    }

    fn shift(app: &mut App, code: KeyCode) {
        app.input(Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT)));
    }

    /// The pane's title and its first line.
    fn terminal_top(app: &mut App) -> (String, String) {
        let lines = screen(app, 100, 20);
        let title: String = lines[0].chars().skip(40).collect();
        let first: String = lines[1].chars().skip(41).collect();
        (title, first.trim_end_matches(['│', ' ']).to_string())
    }

    #[test]
    fn the_wheel_and_shift_page_keys_browse_the_history() {
        let (mut app, requests) = with_a_long_shell();
        let now = Instant::now();
        mouse_at(&mut app, MouseEventKind::ScrollUp, 45, 5, now);
        assert_eq!(app.scrolled(), (3, 83));
        let (title, first) = terminal_top(&mut app);
        assert!(title.starts_with("┌ %7 · ↑ 3 of 83 ─"), "{title}");
        assert_eq!(first, "81");
        mouse_at(&mut app, MouseEventKind::ScrollDown, 45, 5, now);
        assert_eq!(app.scrolled(), (0, 83));
        assert!(terminal_top(&mut app).0.starts_with("┌ %7 ─"));

        // From the projects too, a page less a line at a time
        assert_eq!(app.focus, Pane::Worktrees);
        shift(&mut app, KeyCode::PageUp);
        shift(&mut app, KeyCode::PageUp);
        assert_eq!(app.scrolled(), (32, 83));
        shift(&mut app, KeyCode::PageDown);
        assert_eq!(app.scrolled(), (16, 83));
        app.focus = Pane::Terminal;
        for _ in 0..10 {
            shift(&mut app, KeyCode::PageUp);
        }
        assert_eq!(terminal_top(&mut app).1, "1", "its start");
        assert!(hint_line_of(&mut app).contains("any key: back to the bottom"));
        assert_eq!(
            inputs(&requests),
            Vec::<Vec<u8>>::new(),
            "never the program's"
        );
    }

    fn hint_line_of(app: &mut App) -> String {
        screen(app, 100, 20)[19].clone()
    }

    #[test]
    fn a_key_goes_back_to_the_bottom_and_still_reaches_the_program() {
        let (mut app, requests) = with_a_long_shell();
        app.focus = Pane::Terminal;
        shift(&mut app, KeyCode::PageUp);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.scrolled(), (0, 83));
        assert_eq!(inputs(&requests), [b"x".to_vec()]);
        assert!(hint_line_of(&mut app).contains("other keys go to the terminal"));
    }

    #[test]
    fn a_resize_waits_for_the_size_to_hold_then_for_the_daemons_screen() {
        let (mut app, requests) = with_a_long_shell();
        let resizes = || -> Vec<(u16, u16)> {
            let requests = requests.try_iter();
            requests
                .filter_map(|request| match request {
                    Request::Resize { rows, cols } => Some((rows, cols)),
                    _ => None,
                })
                .collect()
        };
        let now = Instant::now();
        let later = |ms| now + Duration::from_millis(ms);
        // Drawn at 100×20: the pane is 17×58 inside
        app.tick(now);
        app.tick(later(50));
        assert_eq!(resizes(), [], "not held long enough");
        screen(&mut app, 90, 20);
        app.tick(later(60));
        app.tick(later(150));
        assert_eq!(resizes(), [], "still being dragged");
        app.tick(later(160));
        assert_eq!(resizes(), [(17, 48)], "once, at the last size");
        let shown = app.shown.as_ref().unwrap();
        assert_eq!(shown.screen.screen().size(), (18, 58), "left to the daemon");

        app.streamed(Reply::Screen(b"redrawn".to_vec()));
        let shown = app.shown.as_ref().unwrap();
        assert_eq!(
            shown.screen.screen().size(),
            (17, 48),
            "the daemon's, at its size"
        );
        assert_eq!(shown.screen.screen().contents(), "redrawn");
        app.tick(later(500));
        assert_eq!(resizes(), [], "nothing more");
    }

    #[test]
    fn output_keeps_the_view_where_it_is() {
        let (mut app, _requests) = with_a_long_shell();
        mouse_at(&mut app, MouseEventKind::ScrollUp, 45, 5, Instant::now());
        app.streamed(Reply::Output(b"101\r\n".to_vec()));
        assert_eq!(app.scrolled(), (4, 84));
        assert_eq!(terminal_top(&mut app).1, "81", "the same line on top");
    }

    #[test]
    fn the_alternate_screen_has_no_history_and_gets_the_keys() {
        let (mut app, requests) = with_a_long_shell();
        app.focus = Pane::Terminal;
        app.streamed(Reply::Output(b"\x1b[?1049h".to_vec()));
        shift(&mut app, KeyCode::PageUp);
        assert_eq!(app.scrolled(), (0, 0));
        assert_eq!(inputs(&requests), [b"\x1b[5;2~".to_vec()]);
    }

    #[test]
    fn a_program_reading_the_mouse_gets_the_wheel_and_shift_page_keys_still_scroll() {
        let (mut app, requests) = with_a_long_shell();
        app.streamed(Reply::Output(b"\x1b[?1000h\x1b[?1006h".to_vec()));
        mouse_at(&mut app, MouseEventKind::ScrollUp, 41, 1, Instant::now());
        assert_eq!(app.scrolled().0, 0);
        assert_eq!(inputs(&requests), [b"\x1b[<64;1;1M".to_vec()]);
        shift(&mut app, KeyCode::PageUp);
        assert_eq!(app.scrolled().0, 16);
    }

    #[test]
    fn a_selection_in_the_scrolled_view_copies_what_it_shows() {
        let (mut app, _requests) = with_a_long_shell();
        for _ in 0..10 {
            shift(&mut app, KeyCode::PageUp);
        }
        screen_of(&mut app);
        let now = Instant::now();
        mouse_at(&mut app, CLICK, 41, 1, now);
        mouse_at(&mut app, DRAG, 41, 2, now);
        mouse_at(&mut app, UP, 41, 2, now);
        assert_eq!(app.tasks, [Task::Copy("1\n2".into())]);
        mouse_at(&mut app, MouseEventKind::ScrollDown, 45, 5, now);
        assert!(app.selection.is_none(), "the view moved");
    }

    #[test]
    fn clicks_pick_in_choice_lists_and_focus_form_fields() {
        let (_dir, mut app) = with_themes("");
        press(&mut app, KeyCode::Char('T'));
        let lines = screen(&mut app, 100, 20);
        let y = lines
            .iter()
            .position(|l| l.contains("monokai-pro"))
            .unwrap() as u16;
        let x = lines[y as usize].chars().position(|c| c == 'm').unwrap() as u16;
        click(&mut app, 0, y);
        assert_eq!(choice(&app), Some("terminal"), "outside the dialog");
        click(&mut app, x, y);
        assert_eq!(choice(&app), Some("monokai-pro"));
        assert_eq!(app.theme.name, "monokai-pro", "previewed");
        mouse_at(&mut app, MouseEventKind::ScrollDown, x, y, Instant::now());
        assert_eq!(choice(&app), Some("terminal"), "the wheel moves");
        let now = Instant::now();
        mouse_at(&mut app, CLICK, x, y, now);
        mouse_at(&mut app, CLICK, x, y, now + Duration::from_millis(100));
        assert!(app.dialog.is_none(), "chosen");
        assert_eq!(app.tasks.len(), 1, "{:?}", app.tasks);

        let mut app = with_a_project(None);
        press(&mut app, KeyCode::Char('n'));
        let lines = screen(&mut app, 80, 12);
        let y = lines.iter().position(|l| l.contains("Base")).unwrap() as u16;
        let x = lines[y as usize].chars().position(|c| c == 'B').unwrap() as u16;
        click(&mut app, x, y);
        type_text(&mut app, "x");
        assert_eq!(form_values(&mut app), ["", "mainx"]);
    }

    /// With a config whose `themes/` has an invalid theme, `broken`; the themes
    /// are listed as broken, monokai-pro, terminal.
    fn with_themes(config: &str) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, config).unwrap();
        std::fs::create_dir(dir.path().join("themes")).unwrap();
        std::fs::write(dir.path().join("themes/broken.toml"), "border = \"red\"\n").unwrap();
        let app = App::new(config::load_from(&path, None).map_err(|err| err.to_string()));
        (dir, app)
    }

    fn choice(app: &App) -> Option<&str> {
        match &app.dialog {
            Some(Open {
                dialog: Dialog::Choice(choice),
                purpose: Purpose::Theme { .. },
            }) => choice.selected(),
            _ => None,
        }
    }

    #[test]
    fn t_previews_each_theme_and_esc_puts_it_back() {
        let (_dir, mut app) = with_themes("");
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(choice(&app), Some("terminal"), "the one in use");
        assert!(hint_line(&mut app).contains("choose"), "the dialog's keys");
        press(&mut app, KeyCode::Up);
        assert_eq!(choice(&app), Some("monokai-pro"));
        assert_eq!(app.theme.name, "monokai-pro", "shown as selected");
        assert_eq!(app.tasks, [], "not saved yet");
        press(&mut app, KeyCode::Esc);
        assert!(app.dialog.is_none());
        assert_eq!(app.theme.name, "terminal", "put back");
        assert_eq!(app.tasks, []);
    }

    #[test]
    fn enter_saves_the_theme_chosen() {
        let (dir, mut app) = with_themes("");
        press(&mut app, KeyCode::Char('T'));
        press(&mut app, KeyCode::Enter);
        assert!(app.dialog.is_none());
        assert_eq!(app.tasks, [], "the theme in use: nothing to save");

        press(&mut app, KeyCode::Char('T'));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert!(app.dialog.is_none());
        assert_eq!(app.theme.name, "monokai-pro");
        let config = dir.path().join("config.toml");
        let name = "monokai-pro".to_string();
        assert_eq!(app.tasks, [Task::SaveTheme { config, name }]);

        event(&mut app, actions::Event::ThemeSaved("monokai-pro".into()));
        assert_eq!(app.theme.name, "monokai-pro");
        event(&mut app, actions::Event::ThemeNotSaved("read-only".into()));
        assert_eq!(app.theme.name, "terminal", "back to the config's");
        assert!(app.message.as_deref().unwrap().contains("read-only"));
    }

    #[test]
    fn a_theme_in_error_shows_why_and_cannot_be_chosen() {
        let (_dir, mut app) = with_themes("theme = \"monokai-pro\"\n");
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(choice(&app), Some("monokai-pro"));
        press(&mut app, KeyCode::Up);
        assert_eq!(choice(&app), Some("broken"));
        assert_eq!(app.theme.name, "monokai-pro", "the last good one stays");
        let hints = hint_line(&mut app);
        assert!(
            hints.starts_with("broken: unknown field `border`"),
            "{hints}"
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(choice(&app), Some("broken"), "still open");
        assert!(app.message.is_some());
        assert_eq!(app.tasks, []);
    }

    #[test]
    fn a_reload_while_choosing_keeps_the_theme_shown() {
        let (_dir, mut app) = with_themes("");
        press(&mut app, KeyCode::Char('T'));
        press(&mut app, KeyCode::Up);
        reload(&mut app, Ok(test_config()));
        assert_eq!(app.theme.name, "monokai-pro");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.theme.name, "terminal");
        let (_dir, other) = with_themes("theme = \"monokai-pro\"\n");
        reload(&mut app, other.config);
        assert_eq!(app.theme.name, "monokai-pro", "not choosing: the config's");
    }

    fn test_config() -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        config::load_from(&path, None).unwrap()
    }
}
