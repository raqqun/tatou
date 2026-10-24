//! Puts the terminal in the state the UI needs, and always puts it back: when the
//! guard is dropped, whatever the way out, and before a panic message is printed.

use std::io::{self, Stdout, stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, ThreadId};

use crossterm::cursor::Show;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Whether the terminal is in the UI's state and needs restoring.
static ENTERED: AtomicBool = AtomicBool::new(false);

/// Restores the terminal when dropped.
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Raw mode, the alternate screen and mouse capture, until the guard is dropped.
pub fn enter() -> io::Result<(Tui, Guard)> {
    install_panic_hook(thread::current().id());
    enable_raw_mode()?;
    // From here on, a failure still restores what was already changed
    ENTERED.store(true, Ordering::SeqCst);
    let guard = Guard;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
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
    let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen, Show);
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
