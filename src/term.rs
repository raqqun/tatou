//! Keys and pastes, as crossterm reports them, back into the bytes a program in
//! a terminal reads: what an xterm would send for them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the shown terminal's program asked for, which changes some keys' bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes {
    /// Arrows as `ESC O A` rather than `ESC [ A`.
    pub application_cursor: bool,
    /// Pastes between `ESC [200~` and `ESC [201~`.
    pub bracketed_paste: bool,
}

/// The bytes for `key`; none for keys a terminal sends nothing for.
pub fn key_bytes(key: KeyEvent, modes: Modes) -> Vec<u8> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // xterm's modifier parameter: 1, plus 1 for Shift, 2 for Alt, 4 for Ctrl
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let mut bytes = match key.code {
        KeyCode::Char(c) if ctrl => match control(c) {
            Some(byte) => vec![byte],
            None => c.to_string().into_bytes(),
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // Shift-Enter, which the dashboard's terminal reports only through the kitty
        // keyboard protocol, goes as Alt-Enter: agents read both as a new line, and
        // this one needs nothing of the shown terminal
        KeyCode::Enter if shift => return b"\x1b\r".to_vec(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => return b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => return cursor(b'A', modifier, modes),
        KeyCode::Down => return cursor(b'B', modifier, modes),
        KeyCode::Right => return cursor(b'C', modifier, modes),
        KeyCode::Left => return cursor(b'D', modifier, modes),
        KeyCode::Home => return cursor(b'H', modifier, modes),
        KeyCode::End => return cursor(b'F', modifier, modes),
        KeyCode::Insert => return tilde(2, modifier),
        KeyCode::Delete => return tilde(3, modifier),
        KeyCode::PageUp => return tilde(5, modifier),
        KeyCode::PageDown => return tilde(6, modifier),
        KeyCode::F(n @ 1..=4) => return function(b"PQRS"[usize::from(n - 1)], modifier),
        KeyCode::F(n @ 5..=12) => {
            let codes = [15, 17, 18, 19, 20, 21, 23, 24];
            return tilde(codes[usize::from(n - 5)], modifier);
        }
        _ => return Vec::new(),
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// The bytes for pasting `text`: line ends as Enter, inside the brackets when the
/// program asked for them.
pub fn paste_bytes(text: &str, modes: Modes) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// Ctrl with `c`: its control character, when it has one.
fn control(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// An arrow, Home or End: `ESC [ A`, `ESC O A` in application mode, `ESC [1;5A`
/// with modifiers.
fn cursor(letter: u8, modifier: u8, modes: Modes) -> Vec<u8> {
    match (modifier, modes.application_cursor) {
        (1, true) => vec![0x1b, b'O', letter],
        (1, false) => vec![0x1b, b'[', letter],
        (m, _) => format!("\x1b[1;{m}{}", letter as char).into_bytes(),
    }
}

/// `ESC [3~`, `ESC [3;5~` with modifiers.
fn tilde(code: u8, modifier: u8) -> Vec<u8> {
    match modifier {
        1 => format!("\x1b[{code}~").into_bytes(),
        m => format!("\x1b[{code};{m}~").into_bytes(),
    }
}

/// F1 to F4: `ESC O P`, `ESC [1;5P` with modifiers.
fn function(letter: u8, modifier: u8) -> Vec<u8> {
    match modifier {
        1 => vec![0x1b, b'O', letter],
        m => format!("\x1b[1;{m}{}", letter as char).into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
        key_bytes(KeyEvent::new(code, modifiers), Modes::default())
    }

    #[test]
    fn characters_and_control_keys() {
        assert_eq!(bytes(KeyCode::Char('a'), KeyModifiers::NONE), b"a");
        assert_eq!(
            bytes(KeyCode::Char('é'), KeyModifiers::NONE),
            "é".as_bytes()
        );
        assert_eq!(bytes(KeyCode::Char('c'), KeyModifiers::CONTROL), [3]);
        assert_eq!(bytes(KeyCode::Char('d'), KeyModifiers::CONTROL), [4]);
        assert_eq!(bytes(KeyCode::Char(' '), KeyModifiers::CONTROL), [0]);
        assert_eq!(bytes(KeyCode::Char('['), KeyModifiers::CONTROL), [0x1b]);
        assert_eq!(bytes(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(
            bytes(
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            [0x1b, 24]
        );
    }

    #[test]
    fn editing_keys() {
        assert_eq!(bytes(KeyCode::Enter, KeyModifiers::NONE), b"\r");
        assert_eq!(bytes(KeyCode::Enter, KeyModifiers::SHIFT), b"\x1b\r");
        assert_eq!(bytes(KeyCode::Enter, KeyModifiers::ALT), b"\x1b\r");
        assert_eq!(bytes(KeyCode::Backspace, KeyModifiers::NONE), [0x7f]);
        assert_eq!(bytes(KeyCode::Tab, KeyModifiers::NONE), b"\t");
        assert_eq!(bytes(KeyCode::BackTab, KeyModifiers::SHIFT), b"\x1b[Z");
        assert_eq!(bytes(KeyCode::Esc, KeyModifiers::NONE), [0x1b]);
        assert_eq!(bytes(KeyCode::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(bytes(KeyCode::PageUp, KeyModifiers::CONTROL), b"\x1b[5;5~");
    }

    #[test]
    fn arrows_follow_the_cursor_mode() {
        assert_eq!(bytes(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        let application = Modes {
            application_cursor: true,
            ..Modes::default()
        };
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(key_bytes(up, application), b"\x1bOA");
        assert_eq!(bytes(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
        assert_eq!(
            bytes(KeyCode::Right, KeyModifiers::SHIFT | KeyModifiers::ALT),
            b"\x1b[1;4C"
        );
        assert_eq!(bytes(KeyCode::Home, KeyModifiers::NONE), b"\x1b[H");
    }

    #[test]
    fn function_keys() {
        assert_eq!(bytes(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(bytes(KeyCode::F(5), KeyModifiers::NONE), b"\x1b[15~");
        assert_eq!(bytes(KeyCode::F(12), KeyModifiers::SHIFT), b"\x1b[24;2~");
        assert_eq!(bytes(KeyCode::F(13), KeyModifiers::NONE), b"");
    }

    #[test]
    fn pastes_end_lines_with_enter_and_follow_the_brackets() {
        assert_eq!(paste_bytes("a\nb\r\nc", Modes::default()), b"a\rb\rc");
        let bracketed = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        assert_eq!(paste_bytes("x", bracketed), b"\x1b[200~x\x1b[201~");
    }
}
