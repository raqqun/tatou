//! Keymap: defaults, config overrides and action dispatch.
//!
//! The UI turns keys into actions here and handles actions, never raw keys, so
//! overrides from the config (#41) only change this table.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Where an action applies; the hint bar shows the focused pane's actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Worktrees,
    /// Every session on tatou's server, below the projects.
    Sessions,
    /// Navigation and the app itself, in any pane.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Collapse,
    Expand,
    SwitchPane,
    Jump,
    NewWorktree,
    StartAgent,
    Shell,
    OpenPr,
    RemoveWorktree,
    Edit,
    Help,
    Quit,
}

impl Action {
    // Not read yet: checking `[keys]` names and the help overlay list them (#41, #44)
    #[allow(dead_code)]
    pub const ALL: [Action; 14] = [
        Action::Up,
        Action::Down,
        Action::Collapse,
        Action::Expand,
        Action::SwitchPane,
        Action::Jump,
        Action::NewWorktree,
        Action::StartAgent,
        Action::Shell,
        Action::Edit,
        Action::OpenPr,
        Action::RemoveWorktree,
        Action::Help,
        Action::Quit,
    ];

    /// The action's name in `[keys]`.
    // Not called yet: `[keys]` overrides use it (#41)
    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::Collapse => "collapse",
            Action::Expand => "expand",
            Action::SwitchPane => "switch_pane",
            Action::Jump => "jump",
            Action::NewWorktree => "new_worktree",
            Action::StartAgent => "start_agent",
            Action::Shell => "shell",
            Action::OpenPr => "open_pr",
            Action::RemoveWorktree => "remove_worktree",
            Action::Edit => "edit",
            Action::Help => "help",
            Action::Quit => "quit",
        }
    }

    /// What the hint bar calls it.
    pub fn label(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::Collapse => "collapse",
            Action::Expand => "expand",
            Action::SwitchPane => "switch pane",
            Action::Jump => "show",
            Action::NewWorktree => "new",
            Action::StartAgent => "start agent",
            Action::Shell => "shell",
            Action::OpenPr => "PR",
            Action::RemoveWorktree => "remove",
            Action::Edit => "edit",
            Action::Help => "help",
            Action::Quit => "quit",
        }
    }

    pub fn pane(self) -> Pane {
        match self {
            Action::Jump
            | Action::NewWorktree
            | Action::StartAgent
            | Action::Shell
            | Action::Edit
            | Action::OpenPr
            | Action::RemoveWorktree => Pane::Worktrees,
            Action::Up
            | Action::Down
            | Action::Collapse
            | Action::Expand
            | Action::SwitchPane
            | Action::Help
            | Action::Quit => Pane::Any,
        }
    }

    /// Whether the action does something in `pane`: its own, and jumping in the
    /// sessions' pane too.
    pub fn applies(self, pane: Pane) -> bool {
        let own = self.pane();
        own == pane || own == Pane::Any || (self == Action::Jump && pane == Pane::Sessions)
    }

    /// Moving around is not worth space in the hint bar.
    fn is_navigation(self) -> bool {
        matches!(
            self,
            Action::Up | Action::Down | Action::Collapse | Action::Expand | Action::SwitchPane
        )
    }
}

/// A key as the keymap matches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Key {
    fn plain(code: KeyCode) -> Key {
        Key {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn char(c: char) -> Key {
        Key::plain(KeyCode::Char(c))
    }

    fn ctrl(c: char) -> Key {
        Key {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
        }
    }

    /// The key of a key press. Shift is part of a character (`?`, `A`), not a
    /// modifier of it, whatever the terminal reports.
    pub fn from_event(event: KeyEvent) -> Key {
        let mut modifiers = event.modifiers;
        if let KeyCode::Char(_) = event.code {
            modifiers.remove(KeyModifiers::SHIFT);
        }
        Key {
            code: event.code,
            modifiers,
        }
    }

    /// How the hint bar shows it.
    pub fn label(self) -> String {
        let code = match self.code {
            KeyCode::Enter => "⏎".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            other => other.to_string(),
        };
        let mut label = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            label.push_str("C-");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            label.push_str("M-");
        }
        label + &code
    }
}

pub struct Keymap {
    /// In order of preference: an action's first key is the one the hint bar shows.
    bindings: Vec<(Key, Action)>,
}

impl Keymap {
    /// The keys from DESIGN §7, plus vi keys and arrows to move.
    pub fn defaults() -> Keymap {
        let bindings = vec![
            (Key::char('k'), Action::Up),
            (Key::plain(KeyCode::Up), Action::Up),
            (Key::char('j'), Action::Down),
            (Key::plain(KeyCode::Down), Action::Down),
            (Key::char('h'), Action::Collapse),
            (Key::plain(KeyCode::Left), Action::Collapse),
            (Key::char('l'), Action::Expand),
            (Key::plain(KeyCode::Right), Action::Expand),
            (Key::plain(KeyCode::Tab), Action::SwitchPane),
            (Key::plain(KeyCode::Enter), Action::Jump),
            (Key::char('n'), Action::NewWorktree),
            (Key::char('s'), Action::StartAgent),
            (Key::char('t'), Action::Shell),
            (Key::char('e'), Action::Edit),
            (Key::char('p'), Action::OpenPr),
            (Key::char('d'), Action::RemoveWorktree),
            (Key::char('?'), Action::Help),
            (Key::char('q'), Action::Quit),
            (Key::ctrl('c'), Action::Quit),
        ];
        Keymap { bindings }
    }

    pub fn action(&self, key: Key) -> Option<Action> {
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == key)
            .map(|(_, action)| *action)
    }

    /// The hint bar for `pane`: its actions, then help and quit, each with its first
    /// key; `hidden` are the ones that do not apply to the selection.
    pub fn hints(&self, pane: Pane, hidden: &[Action]) -> Vec<(String, &'static str)> {
        let mut hints: Vec<(String, &'static str)> = Vec::new();
        let mut shown: Vec<Action> = Vec::new();
        let wanted = |action: Action| action.applies(pane) && !hidden.contains(&action);
        for (key, action) in &self.bindings {
            if wanted(*action) && !action.is_navigation() && !shown.contains(action) {
                shown.push(*action);
                hints.push((key.label(), action.label()));
            }
        }
        hints
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEventKind;

    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        let mut event = KeyEvent::new(code, modifiers);
        event.kind = KeyEventKind::Press;
        Keymap::defaults().action(Key::from_event(event))
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        assert_eq!(
            press(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(
            press(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(press(KeyCode::Char('c'), KeyModifiers::NONE), None);
    }

    #[test]
    fn shifted_characters_are_their_own_keys() {
        // Terminals report `?` with or without Shift; both are help
        assert_eq!(
            press(KeyCode::Char('?'), KeyModifiers::SHIFT),
            Some(Action::Help)
        );
        assert_eq!(
            press(KeyCode::Char('?'), KeyModifiers::NONE),
            Some(Action::Help)
        );
        // An uppercase letter is not its lowercase one
        assert_eq!(press(KeyCode::Char('D'), KeyModifiers::SHIFT), None);
        assert_eq!(
            press(KeyCode::Char('d'), KeyModifiers::NONE),
            Some(Action::RemoveWorktree)
        );
    }

    #[test]
    fn vi_keys_and_arrows_move() {
        assert_eq!(
            press(KeyCode::Char('j'), KeyModifiers::NONE),
            Some(Action::Down)
        );
        assert_eq!(press(KeyCode::Down, KeyModifiers::NONE), Some(Action::Down));
        assert_eq!(
            press(KeyCode::Left, KeyModifiers::NONE),
            Some(Action::Collapse)
        );
        assert_eq!(
            press(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::Jump)
        );
    }

    #[test]
    fn every_action_has_a_unique_name_and_a_default_key() {
        let keymap = Keymap::defaults();
        for action in Action::ALL {
            let others = Action::ALL.iter().filter(|a| a.name() == action.name());
            assert_eq!(others.count(), 1, "{action:?}");
            assert!(
                keymap.bindings.iter().any(|(_, a)| *a == action),
                "{action:?} has no key"
            );
        }
    }

    #[test]
    fn no_key_is_bound_twice() {
        let keymap = Keymap::defaults();
        for (i, (key, _)) in keymap.bindings.iter().enumerate() {
            let later = keymap.bindings[i + 1..].iter().find(|(k, _)| k == key);
            assert!(later.is_none(), "{key:?}");
        }
    }

    #[test]
    fn hints_show_the_focused_pane_then_help_and_quit() {
        let keymap = Keymap::defaults();
        let worktrees: Vec<_> = keymap.hints(Pane::Worktrees, &[]);
        let labels: Vec<_> = worktrees.iter().map(|(k, a)| format!("{k} {a}")).collect();
        assert_eq!(
            labels,
            [
                "⏎ show",
                "n new",
                "s start agent",
                "t shell",
                "e edit",
                "p PR",
                "d remove",
                "? help",
                "q quit"
            ]
        );
        let without_jump: Vec<_> = keymap
            .hints(Pane::Worktrees, &[Action::Jump, Action::Help])
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(without_jump, ["n", "s", "t", "e", "p", "d", "q"]);
        let sessions: Vec<_> = keymap
            .hints(Pane::Sessions, &[])
            .into_iter()
            .map(|(k, a)| format!("{k} {a}"))
            .collect();
        assert_eq!(sessions, ["⏎ show", "? help", "q quit"]);
    }

    #[test]
    fn key_labels() {
        assert_eq!(Key::ctrl('c').label(), "C-c");
        assert_eq!(Key::plain(KeyCode::Tab).label(), "Tab");
        assert_eq!(Key::plain(KeyCode::F(5)).label(), "F5");
    }
}
