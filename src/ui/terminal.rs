//! Puts the terminal in the state the UI needs, and always puts it back: when the
//! guard is dropped, whatever the way out, and before a panic message is printed.

use std::io::{self, Stdout, Write, stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, ThreadId};

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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

/// Raw mode, the alternate screen, bracketed paste and the mouse, until the guard
/// is dropped: the terminal then selects text only with its bypass key held
/// (Shift, Option in iTerm2), and every click is the dashboard's. Where
/// the terminal supports the kitty keyboard protocol, keys come disambiguated: a
/// legacy terminal sends Shift-Enter as Enter, and an agent needs to tell them apart.
pub fn enter() -> io::Result<(Tui, Guard)> {
    install_panic_hook(thread::current().id());
    enable_raw_mode()?;
    // From here on, a failure still restores what was already changed
    ENTERED.store(true, Ordering::SeqCst);
    let guard = Guard;
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    // A terminal that cannot answer simply keeps sending legacy keys
    if supports_keyboard_enhancement().unwrap_or(false) {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        execute!(stdout(), PushKeyboardEnhancementFlags(flags))?;
        ENHANCED.store(true, Ordering::SeqCst);
    }
    let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    Ok((terminal, guard))
}

/// Asks the terminal to put `text` in the clipboard, with OSC 52: for when no
/// clipboard tool is there, e.g. over SSH. A terminal that does not support it
/// ignores it.
pub fn copy(text: &str) -> io::Result<()> {
    let mut out = stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

/// Standard base64, padded; OSC 52 is its only use, not worth a crate.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            match i <= chunk.len() {
                true => encoded.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char),
                false => encoded.push('='),
            }
        }
    }
    encoded
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
    let _ = execute!(
        stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_as_rfc_4648_says() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ];
        for (text, encoded) in cases {
            assert_eq!(base64(text.as_bytes()), encoded, "{text}");
        }
        assert_eq!(base64("héllo ~/a?".as_bytes()), "aMOpbGxvIH4vYT8=");
    }
}
