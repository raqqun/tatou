//! The only platform-specific code: `copy(text)`, and `notify(title, body)` (3.3).

use crate::exec;

/// The clipboard tools to try, in order, as (program, arguments): `pbcopy` on
/// macOS; on Linux `wl-copy` under Wayland, then X11's `xclip` and `xsel`, which
/// XWayland serves too.
fn tools(macos: bool, wayland: bool) -> Vec<(&'static str, &'static [&'static str])> {
    if macos {
        return vec![("pbcopy", &[])];
    }
    let mut tools: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if wayland {
        tools.push(("wl-copy", &[]));
    }
    tools.push(("xclip", &["-selection", "clipboard"]));
    tools.push(("xsel", &["--clipboard", "--input"]));
    tools
}

/// Puts `text` in the system's clipboard with the first tool that does it, and
/// says which; `None` when none could, missing or failing (no display over SSH),
/// for the dashboard's terminal to be asked instead.
pub fn copy(text: &str) -> Option<&'static str> {
    let macos = cfg!(target_os = "macos");
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    for (program, args) in tools(macos, wayland) {
        match exec::feed(program, args, text.as_bytes()) {
            Ok(()) => return Some(program),
            Err(err) => tracing::debug!(program, %err, "cannot copy"),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_has_its_clipboard_tools() {
        let names = |macos, wayland| -> Vec<&str> {
            tools(macos, wayland)
                .into_iter()
                .map(|(program, _)| program)
                .collect()
        };
        assert_eq!(names(true, false), ["pbcopy"]);
        assert_eq!(names(false, true), ["wl-copy", "xclip", "xsel"]);
        assert_eq!(names(false, false), ["xclip", "xsel"]);
    }
}
