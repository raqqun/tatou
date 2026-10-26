//! Themes: the dashboard's colours, built in or from `themes/` next to the config.

use std::fs;
use std::io;
use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use crate::config;

/// The theme used when `config.toml` names none.
pub const DEFAULT: &str = "terminal";

/// The themes compiled into tatou, by name, each from its file in `themes/`; the
/// default first. A theme is contributed by adding its file there and a line here.
const BUILT_IN: [(&str, &str); 2] = [
    (DEFAULT, include_str!("../themes/terminal.toml")),
    ("monokai-pro", include_str!("../themes/monokai-pro.toml")),
];

/// The dashboard's colours, with the default theme's for those a theme leaves out.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    /// The dashboard's, and the shown terminal's cells in the default colours.
    pub background: Color,
    pub foreground: Color,
    /// The focused pane's border and title.
    pub accent: Color,
    /// The selected row's background; `None` reverses the row.
    pub selection: Option<Color>,
    pub error: Color,
    /// The needs-input status.
    pub input: Color,
    /// The busy status.
    pub busy: Color,
    /// The `repo` tag.
    pub tag: Color,
    /// Keys in the hint bar.
    pub key: Color,
}

impl Theme {
    /// The background and text colours, under everything the dashboard draws.
    pub fn base(&self) -> Style {
        Style::new().fg(self.foreground).bg(self.background)
    }

    /// The selected row of a list.
    pub fn selected(&self) -> Style {
        match self.selection {
            Some(color) => Style::new().bg(color),
            None => Style::new().add_modifier(Modifier::REVERSED),
        }
    }
}

/// A theme's file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, deserialize_with = "color")]
    background: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    foreground: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    accent: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    selection: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    error: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    input: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    busy: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    tag: Option<Color>,
    #[serde(default, deserialize_with = "color")]
    key: Option<Color>,
}

/// A colour as ratatui reads it: `#rrggbb`, an ANSI name, an index or `reset`.
fn color<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Color>, D::Error> {
    let text = String::deserialize(deserializer)?;
    match text.parse() {
        Ok(color) => Ok(Some(color)),
        Err(_) => Err(D::Error::custom(format!(
            "`{text}` is not a colour: use \"#rrggbb\", a name such as \"cyan\" or \
             \"light-red\", an index from 0 to 255, or \"reset\""
        ))),
    }
}

/// The default theme, as built in.
pub fn terminal() -> Theme {
    let reset = Theme {
        name: DEFAULT.to_string(),
        background: Color::Reset,
        foreground: Color::Reset,
        accent: Color::Reset,
        selection: None,
        error: Color::Reset,
        input: Color::Reset,
        busy: Color::Reset,
        tag: Color::Reset,
        key: Color::Reset,
    };
    // Every built-in theme is tested to parse (`every_built_in_theme_parses`)
    let file = toml::from_str(BUILT_IN[0].1).expect("the built-in default theme parses");
    resolve(DEFAULT, file, &reset)
}

/// The theme called `name`: `<dir>/<name>.toml`, else the built-in one.
pub fn load(dir: &Path, name: &str) -> Result<Theme, String> {
    if name.is_empty() || name.starts_with('.') || name.contains('/') {
        return Err(format!("theme = \"{name}\" is not a theme's name"));
    }
    let path = dir.join(format!("{name}.toml"));
    let (text, origin) = match fs::read_to_string(&path) {
        Ok(text) => (text, path.display().to_string()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            match BUILT_IN.iter().find(|(built_in, _)| *built_in == name) {
                Some((_, text)) => (text.to_string(), format!("built-in theme {name}")),
                None => {
                    return Err(format!(
                        "theme = \"{name}\" is not one of the themes: {}",
                        names(dir).join(", ")
                    ));
                }
            }
        }
        Err(err) => return Err(format!("{}: {err}", path.display())),
    };
    let file = toml::from_str(&text).map_err(|err| format!("{origin}: {err}"))?;
    Ok(resolve(name, file, &terminal()))
}

/// Every theme's name, built in or in `dir`, sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let files = config::toml_files(dir).unwrap_or_default();
    let mut names: Vec<String> = BUILT_IN.iter().map(|(name, _)| name.to_string()).collect();
    names.extend(
        files
            .iter()
            .filter_map(|file| file.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned()),
    );
    names.sort();
    names.dedup();
    names
}

/// `file`'s colours, and `base`'s where it has none.
fn resolve(name: &str, file: File, base: &Theme) -> Theme {
    Theme {
        name: name.to_string(),
        background: file.background.unwrap_or(base.background),
        foreground: file.foreground.unwrap_or(base.foreground),
        accent: file.accent.unwrap_or(base.accent),
        selection: file.selection.or(base.selection),
        error: file.error.unwrap_or(base.error),
        input: file.input.unwrap_or(base.input),
        busy: file.busy.unwrap_or(base.busy),
        tag: file.tag.unwrap_or(base.tag),
        key: file.key.unwrap_or(base.key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themes(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    #[test]
    fn every_built_in_theme_parses() {
        let dir = themes(&[]);
        for (name, _) in BUILT_IN {
            assert_eq!(load(dir.path(), name).unwrap().name, name);
        }
    }

    #[test]
    fn the_default_theme_is_the_terminals_palette() {
        let theme = terminal();
        assert_eq!(theme.base(), Style::new().fg(Color::Reset).bg(Color::Reset));
        assert_eq!(theme.accent, Color::Cyan);
        assert_eq!(theme.error, Color::Red);
        assert_eq!(theme.input, Color::Yellow);
        assert_eq!(theme.tag, Color::Cyan);
        assert_eq!(
            theme.selected(),
            Style::new().add_modifier(Modifier::REVERSED)
        );
    }

    #[test]
    fn monokai_pro_is_built_in() {
        let theme = load(themes(&[]).path(), "monokai-pro").unwrap();
        assert_eq!(theme.background, Color::Rgb(0x2d, 0x2a, 0x2e));
        assert_eq!(theme.foreground, Color::Rgb(0xfc, 0xfc, 0xfa));
        assert_eq!(
            theme.selected(),
            Style::new().bg(Color::Rgb(0x5b, 0x59, 0x5c))
        );
    }

    #[test]
    fn colours_are_hex_names_indexes_or_reset() {
        let text = r##"
            background = "#00ff7f"
            foreground = "light-red"
            accent = "Bright Blue"
            selection = "236"
            error = "reset"
        "##;
        let dir = themes(&[("mine.toml", text)]);
        let theme = load(dir.path(), "mine").unwrap();
        assert_eq!(theme.background, Color::Rgb(0x00, 0xff, 0x7f));
        assert_eq!(theme.foreground, Color::LightRed);
        assert_eq!(theme.accent, Color::LightBlue);
        assert_eq!(theme.selection, Some(Color::Indexed(236)));
        assert_eq!(theme.error, Color::Reset);
    }

    #[test]
    fn keys_left_out_take_the_default_themes_colours() {
        let dir = themes(&[("mine.toml", "accent = \"green\"\n")]);
        let theme = load(dir.path(), "mine").unwrap();
        let expected = Theme {
            name: "mine".to_string(),
            accent: Color::Green,
            ..terminal()
        };
        assert_eq!(theme, expected);
    }

    #[test]
    fn bad_colours_and_unknown_keys_are_errors_with_their_line() {
        let dir = themes(&[
            ("colour.toml", "accent = \"cyan\"\nerror = \"#12\"\n"),
            ("key.toml", "accent = \"cyan\"\nborder = \"red\"\n"),
        ]);
        let err = load(dir.path(), "colour").unwrap_err();
        assert!(err.contains("colour.toml"), "{err}");
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("`#12` is not a colour"), "{err}");
        let err = load(dir.path(), "key").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("unknown field `border`"), "{err}");
    }

    #[test]
    fn a_users_theme_replaces_a_built_in_one() {
        let dir = themes(&[("monokai-pro.toml", "accent = \"green\"\n")]);
        let theme = load(dir.path(), "monokai-pro").unwrap();
        assert_eq!(theme.accent, Color::Green);
        assert_eq!(theme.background, Color::Reset);
    }

    #[test]
    fn an_unknown_theme_names_the_themes() {
        let dir = themes(&[("mine.toml", ""), (".hidden.toml", ""), ("notes.txt", "")]);
        assert_eq!(names(dir.path()), ["mine", "monokai-pro", "terminal"]);
        let err = load(dir.path(), "nope").unwrap_err();
        assert_eq!(
            err,
            "theme = \"nope\" is not one of the themes: mine, monokai-pro, terminal"
        );
    }

    #[test]
    fn a_theme_is_a_file_name_never_a_path() {
        let dir = themes(&[]);
        for name in ["", "../mine", ".hidden", "a/b"] {
            let err = load(dir.path(), name).unwrap_err();
            assert!(err.ends_with("is not a theme's name"), "{err}");
        }
    }
}
