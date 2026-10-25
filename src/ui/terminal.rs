//! Puts the terminal in the state the UI needs, and always puts it back: when the
//! guard is dropped, whatever the way out, and before a panic message is printed.

use std::io::{self, Stdout, stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, ThreadId};

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Whether the terminal is in the UI's state and needs restoring.
static ENTERED: AtomicBool = AtomicBool::new(false);

/// Whether the terminal reports keys with the kitty keyboard protocol, to undo.
static ENHANCED: AtomicBool = AtomicBool::new(false);

/// Restores the terminal when dropped.
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Raw mode, the alternate screen and bracketed paste, until the guard is dropped.
/// The mouse is left to the terminal, to select text in the shown terminal. Where
/// the terminal supports the kitty keyboard protocol, keys come disambiguated: a
/// legacy terminal sends Shift-Enter as Enter, and an agent needs to tell them apart.
pub fn enter() -> io::Result<(Tui, Guard)> {
    install_panic_hook(thread::current().id());
    enable_raw_mode()?;
    // From here on, a failure still restores what was already changed
    ENTERED.store(true, Ordering::SeqCst);
    let guard = Guard;
    execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    // A terminal that cannot answer simply keeps sending legacy keys
    if supports_keyboard_enhancement().unwrap_or(false) {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        execute!(stdout(), PushKeyboardEnhancementFlags(flags))?;
        ENHANCED.store(true, Ordering::SeqCst);
    }
    let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    Ok((terminal, guard))
}

/// Undoes `enter`, once: after a panic both the hook and the guard call it, and
/// leaving the alternate screen a second time would move the cursor back over the
/// panic message.
fn restore() {
    if !ENTERED.swap(false, Ordering::SeqCst) {
        return;
    }
    // Nothing more can be done if the terminal refuses
    if ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout(), DisableBracketedPaste, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

/// A panic in the UI thread restores the terminal before the message is printed,
/// so that it is readable and the shell usable. A panic in another thread leaves
/// the UI running, so it goes to the log instead of over the screen.
fn install_panic_hook(ui: ThreadId) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if thread::current().id() == ui {
            restore();
            previous(info);
        } else {
            let name = thread::current().name().map(String::from);
            tracing::error!(thread = ?name, %info, "a background thread panicked");
        }
    }));
}
