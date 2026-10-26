//! Keys, pastes and the mouse, as crossterm reports them, back into the bytes a
//! program in a terminal reads: what an xterm would send for them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui_term::vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// What the shown terminal's program asked for, which changes some keys' bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes {
    /// Arrows as `ESC O A` rather than `ESC [ A`.
    pub application_cursor: bool,
    /// Pastes between `ESC [200~` and `ESC [201~`.
    pub bracketed_paste: bool,
    /// Which mouse events it reads; none by default.
    pub mouse: MouseProtocolMode,
    pub mouse_encoding: MouseProtocolEncoding,
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

/// The bytes for `event`, at (`column`, `row`) from the terminal's top left
/// corner; none when the program did not ask for this kind of event, or when its
/// encoding cannot say where it happened.
pub fn mouse_bytes(event: MouseEvent, column: u16, row: u16, modes: Modes) -> Vec<u8> {
    use MouseProtocolMode as Mode;
    let motion = matches!(modes.mouse, Mode::ButtonMotion | Mode::AnyMotion);
    // xterm's button codes: 0 to 2 for buttons, 3 for a release in the legacy
    // encodings, 64 and up for the wheel, plus 32 for motion
    let (button, release) = match event.kind {
        MouseEventKind::Down(button) => (button_code(button), false),
        MouseEventKind::Up(button) if modes.mouse != Mode::Press => (button_code(button), true),
        MouseEventKind::Drag(button) if motion => (button_code(button) + 32, false),
        MouseEventKind::Moved if modes.mouse == Mode::AnyMotion => (3 + 32, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        _ => return Vec::new(),
    };
    if modes.mouse == Mode::None {
        return Vec::new();
    }
    let modifiers = event.modifiers;
    let held = 4 * u32::from(modifiers.contains(KeyModifiers::SHIFT))
        + 8 * u32::from(modifiers.contains(KeyModifiers::ALT))
        + 16 * u32::from(modifiers.contains(KeyModifiers::CONTROL));
    let (x, y) = (u32::from(column) + 1, u32::from(row) + 1);
    if modes.mouse_encoding == MouseProtocolEncoding::Sgr {
        let end = if release { 'm' } else { 'M' };
        return format!("\x1b[<{};{x};{y}{end}", button + held).into_bytes();
    }
    // The legacy encodings cannot tell which button was released
    let button = if release { 3 } else { button };
    let mut bytes = b"\x1b[M".to_vec();
    for value in [button + held, x, y] {
        // Each value is a character, 32 past it: one byte, else UTF-8 in mode 1005
        let Some(c) = char::from_u32(32 + value) else {
            return Vec::new();
        };
        match modes.mouse_encoding {
            MouseProtocolEncoding::Utf8 if c as u32 <= 0x7ff => {
                bytes.extend(c.to_string().as_bytes());
            }
            MouseProtocolEncoding::Default if c as u32 <= 0xff => bytes.push(c as u8),
            _ => return Vec::new(),
        }
    }
    bytes
}

fn button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
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

    fn mouse(kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
        // Where the dashboard saw it; `mouse_bytes` is told where it is in the pane
        MouseEvent {
            kind,
            column: 50,
            row: 10,
            modifiers,
        }
    }

    fn reading(mode: MouseProtocolMode, encoding: MouseProtocolEncoding) -> Modes {
        Modes {
            mouse: mode,
            mouse_encoding: encoding,
            ..Modes::default()
        }
    }

    const LEFT: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
    const NONE: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn the_mouse_goes_only_to_a_program_that_asked() {
        use MouseProtocolEncoding::Sgr;
        use MouseProtocolMode::*;
        let left = mouse(LEFT, NONE);
        let up = mouse(MouseEventKind::Up(MouseButton::Left), NONE);
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left), NONE);
        let moved = mouse(MouseEventKind::Moved, NONE);
        let wheel = mouse(MouseEventKind::ScrollDown, NONE);
        let sent = |event, mode| !mouse_bytes(event, 0, 0, reading(mode, Sgr)).is_empty();
        assert!(!sent(left, None), "not asked");
        assert!(!sent(wheel, None));
        assert!(sent(left, Press) && sent(wheel, Press));
        assert!(!sent(up, Press), "presses only");
        assert!(sent(up, PressRelease) && !sent(drag, PressRelease));
        assert!(sent(drag, ButtonMotion) && !sent(moved, ButtonMotion));
        assert!(sent(moved, AnyMotion));
    }

    #[test]
    fn sgr_says_which_button_where_and_what_was_held() {
        let modes = reading(MouseProtocolMode::AnyMotion, MouseProtocolEncoding::Sgr);
        let bytes = |kind, modifiers, column, row| {
            String::from_utf8(mouse_bytes(mouse(kind, modifiers), column, row, modes)).unwrap()
        };
        assert_eq!(bytes(LEFT, NONE, 0, 0), "\x1b[<0;1;1M");
        let right_up = MouseEventKind::Up(MouseButton::Right);
        assert_eq!(bytes(right_up, NONE, 4, 2), "\x1b[<2;5;3m");
        let drag = MouseEventKind::Drag(MouseButton::Middle);
        assert_eq!(
            bytes(drag, KeyModifiers::CONTROL, 299, 99),
            "\x1b[<49;300;100M"
        );
        assert_eq!(bytes(MouseEventKind::Moved, NONE, 1, 1), "\x1b[<35;2;2M");
        assert_eq!(
            bytes(MouseEventKind::ScrollUp, KeyModifiers::SHIFT, 0, 0),
            "\x1b[<68;1;1M"
        );
        let alt = bytes(MouseEventKind::ScrollDown, KeyModifiers::ALT, 0, 0);
        assert_eq!(alt, "\x1b[<73;1;1M");
    }

    #[test]
    fn the_legacy_encodings_stop_where_they_cannot_say_where() {
        let legacy = reading(
            MouseProtocolMode::PressRelease,
            MouseProtocolEncoding::Default,
        );
        let up = MouseEventKind::Up(MouseButton::Right);
        assert_eq!(mouse_bytes(mouse(LEFT, NONE), 0, 0, legacy), b"\x1b[M !!");
        assert_eq!(mouse_bytes(mouse(up, NONE), 9, 4, legacy), b"\x1b[M#*%");
        assert_eq!(
            mouse_bytes(mouse(LEFT, NONE), 222, 0, legacy),
            b"\x1b[M \xff!"
        );
        assert_eq!(
            mouse_bytes(mouse(LEFT, NONE), 223, 0, legacy),
            b"",
            "past 255"
        );

        let utf8 = reading(MouseProtocolMode::PressRelease, MouseProtocolEncoding::Utf8);
        let far = mouse_bytes(mouse(LEFT, NONE), 223, 0, utf8);
        assert_eq!(far, "\x1b[M \u{100}!".as_bytes());
        assert_eq!(
            mouse_bytes(mouse(LEFT, NONE), 2015, 0, utf8),
            b"",
            "past 2047"
        );
    }
}
