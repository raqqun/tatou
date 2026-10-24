//! The dashboard: ratatui rendering and input handling only.

mod terminal;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::keys::{Action, Key, Keymap, Pane};
use crate::model::Sources;
use crate::refresh::{Refresh, Update};
use crate::tmux::{self, Server};

/// How long to wait for input before applying updates and drawing again.
const TICK: Duration = Duration::from_millis(100);

/// Runs the dashboard until the user quits.
pub fn run() -> Result<()> {
    let config = config::load().map_err(|err| err.to_string());
    let (updates, received) = mpsc::channel();
    // Dropped when the dashboard ends, which stops the workers
    let _refresh = match &config {
        Ok(config) => {
            let projects: Vec<PathBuf> = config.projects.iter().map(|p| p.path.clone()).collect();
            let server = Server::tatou(&tmux::conf_path()?);
            Some(Refresh::start(server, &projects, &updates))
        }
        Err(_) => None,
    };

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
        app.apply_all(&received);
        if app.quit {
            return Ok(());
        }
    }
}

struct App {
    /// The error is shown in place of the projects.
    config: std::result::Result<Config, String>,
    sources: Sources,
    keymap: Keymap,
    focus: Pane,
    quit: bool,
}

impl App {
    fn new(config: std::result::Result<Config, String>) -> App {
        App {
            config,
            sources: Sources::default(),
            keymap: Keymap::defaults(),
            focus: Pane::Worktrees,
            quit: false,
        }
    }

    fn input(&mut self, event: Event) {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
            && let Some(action) = self.keymap.action(Key::from_event(key))
        {
            self.handle(action);
        }
    }

    /// Applies every update that has arrived, without waiting for more.
    fn apply_all(&mut self, received: &Receiver<Update>) {
        while let Ok(update) = received.try_recv() {
            self.sources.apply(update);
        }
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
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

    fn render(&self, frame: &mut Frame) {
        let [main, hints] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(main);
        self.render_projects(frame, left);
        frame.render_widget(self.pane_block(" Files ", Pane::Files), right);
        frame.render_widget(self.hint_bar(), hints);
    }

    fn render_projects(&self, frame: &mut Frame, area: Rect) {
        let block = self.pane_block(" Projects ", Pane::Worktrees);
        let body = match &self.config {
            Ok(_) => Paragraph::new(""),
            Err(err) => Paragraph::new(err.as_str()).wrap(Wrap { trim: false }),
        };
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

    fn hint_bar(&self) -> Paragraph<'static> {
        let mut spans = Vec::new();
        for (key, label) in self.keymap.hints(self.focus) {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(key, Style::new().add_modifier(Modifier::BOLD)));
            spans.push(Span::raw(format!(" {label}")));
        }
        Paragraph::new(Line::from(spans))
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn app() -> App {
        App::new(Err("not loaded".into()))
    }

    fn press(app: &mut App, code: KeyCode) {
        app.input(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    /// The text on screen, one string per line, styles left out.
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
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

    #[test]
    fn renders_the_empty_frame() {
        let app = App::new(Ok(test_config()));
        let expected = [
            "┌ Projects ──────────────────┐┌ Files ─────────────────────────────────────┐",
            "│                            ││                                            │",
            "│                            ││                                            │",
            "└────────────────────────────┘└────────────────────────────────────────────┘",
            "⏎ jump  n new  s start agent  t shell  p PR  d remove  ? help  q quit",
        ];
        assert_eq!(screen(&app, 76, 5), expected);
    }

    #[test]
    fn a_config_error_shows_in_place_of_the_projects() {
        let app = App::new(Err("config.toml: unknown field `popop`".into()));
        let lines = screen(&app, 100, 6);
        assert!(lines[1].contains("config.toml: unknown field"), "{lines:?}");
    }

    #[test]
    fn tab_switches_the_focus_and_the_hints() {
        let mut app = app();
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Pane::Files);
        let lines = screen(&app, 80, 4);
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
        updates
            .send(Update::Sessions(Err("tmux: boom".into())))
            .unwrap();
        app.apply_all(&received);
        assert_eq!(app.sources.sessions_error(), Some("tmux: boom"));
        // Nothing more to apply: returns at once
        app.apply_all(&received);
    }

    fn test_config() -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        config::load_from(&path, None).unwrap()
    }
}
