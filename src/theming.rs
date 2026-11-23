//! Color themes: a handful of built-in presets plus user-defined ones dropped into the
//! `themes/` folder (in the data folder, see `crate::paths`) as `.toml` files.
use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{paths, settings::Settings};

const EXAMPLE_THEME: &str = r#"# Copy this file, rename it to something.toml, and edit the colors to make your own
# theme. It shows up as a preset button in Settings after clicking "Reload" there (or
# restarting the app). You can also set colors in Settings and use "Save as theme".
# Colors are [red, green, blue], each 0-255.
#
# A theme with the same name as a built-in one (e.g. "Classic") replaces it.
# If `name` is left out, the file name is used.
name = "My Theme"
background = [0, 0, 0]
past_line = [200, 180, 255]
current_line = [255, 255, 255]
future_line = [180, 210, 255]
# Optional: color of icons, titles and selected buttons. Without it, it's derived from
# current_line.
accent = [180, 165, 230]
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default)]
    pub name: Cow<'static, str>,
    pub background: [u8; 3],
    pub past_line: [u8; 3],
    pub current_line: [u8; 3],
    pub future_line: [u8; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<[u8; 3]>,
    /// The file a custom theme was loaded from; `None` for built-in themes.
    #[serde(skip)]
    pub source: Option<PathBuf>,
}

impl Theme {
    const fn builtin(
        name: &'static str,
        background: [u8; 3],
        past_line: [u8; 3],
        current_line: [u8; 3],
        future_line: [u8; 3],
        accent: [u8; 3],
    ) -> Self {
        Self {
            name: Cow::Borrowed(name),
            background,
            past_line,
            current_line,
            future_line,
            accent: Some(accent),
            source: None,
        }
    }

    /// A theme holding the colors currently set in `settings`.
    pub fn from_settings(name: &str, settings: &Settings) -> Self {
        Self {
            name: Cow::Owned(name.to_owned()),
            background: settings.background_color,
            past_line: settings.past_line_color,
            current_line: settings.current_line_color,
            future_line: settings.future_line_color,
            accent: settings.accent_color,
            source: None,
        }
    }

    pub const fn apply_to(&self, settings: &mut Settings) {
        settings.background_color = self.background;
        settings.past_line_color = self.past_line;
        settings.current_line_color = self.current_line;
        settings.future_line_color = self.future_line;
        settings.accent_color = self.accent;
    }

    /// Whether `settings` currently uses exactly this theme's colors.
    pub fn is_applied_to(&self, settings: &Settings) -> bool {
        settings.background_color == self.background
            && settings.past_line_color == self.past_line
            && settings.current_line_color == self.current_line
            && settings.future_line_color == self.future_line
            && settings.accent_color == self.accent
    }

    fn same_name(&self, other: &Self) -> bool {
        self.name.trim().eq_ignore_ascii_case(other.name.trim())
    }
}

/// The default theme, also used for `Settings::default()`.
pub const CLASSIC: Theme = Theme::builtin(
    "Classic",
    [0, 0, 0],
    [200, 180, 255],
    [255, 255, 255],
    [180, 210, 255],
    [180, 165, 230],
);

pub static BUILTIN_THEMES: [Theme; 7] = [
    CLASSIC,
    Theme::builtin(
        "Sunset",
        [20, 8, 5],
        [255, 180, 120],
        [255, 255, 255],
        [255, 110, 90],
        [240, 150, 110],
    ),
    Theme::builtin(
        "Ocean",
        [0, 8, 15],
        [150, 220, 255],
        [255, 255, 255],
        [80, 160, 220],
        [110, 190, 235],
    ),
    Theme::builtin(
        "Forest",
        [4, 10, 5],
        [180, 255, 180],
        [255, 255, 255],
        [90, 200, 120],
        [130, 210, 140],
    ),
    Theme::builtin(
        "Mono",
        [0, 0, 0],
        [160, 160, 160],
        [255, 255, 255],
        [100, 100, 100],
        [170, 170, 170],
    ),
    Theme::builtin(
        "Metal",
        [6, 6, 8],
        [140, 140, 150],
        [255, 255, 255],
        [150, 25, 30],
        [200, 60, 70],
    ),
    Theme::builtin(
        "Retrowave",
        [12, 4, 30],
        [255, 60, 180],
        [80, 255, 255],
        [140, 70, 220],
        [255, 90, 190],
    ),
];

/// Creates the `themes/` folder (if missing) and seeds it with a documented example file,
/// so users have something to copy without needing to read source code. Runs before
/// logging is set up, so returns any failure for the caller to log.
pub fn ensure_themes_dir() -> Option<String> {
    let themes_dir = paths::themes_dir();
    if let Err(e) = fs::create_dir_all(&themes_dir) {
        return Some(format!("Failed to create themes folder {}: {e}", themes_dir.display()));
    }
    let example_path = themes_dir.join("example.toml.sample");
    if !example_path.exists()
        && let Err(e) = fs::write(&example_path, EXAMPLE_THEME)
    {
        return Some(format!("Failed to write {}: {e}", example_path.display()));
    }
    None
}

/// All themes to offer in Settings: the built-in ones followed by the custom ones from
/// the `themes/` folder (see `merge_themes`).
pub fn load_themes() -> Vec<Theme> {
    merge_themes(&BUILTIN_THEMES, load_custom_themes())
}

/// Built-in themes in their fixed order, with any custom theme of the same name
/// (case-insensitive) taking its place, followed by the remaining custom themes.
fn merge_themes(builtins: &[Theme], custom: Vec<Theme>) -> Vec<Theme> {
    let mut themes = builtins.to_vec();
    for theme in custom {
        match themes.iter_mut().find(|t| t.same_name(&theme)) {
            Some(existing) => *existing = theme,
            None => themes.push(theme),
        }
    }
    themes
}

/// Scans `themes/` for `.toml` files (the `.sample` example is skipped since it doesn't
/// end in `.toml`) and parses each as a `Theme`. Invalid files are logged and skipped
/// rather than failing the whole load.
fn load_custom_themes() -> Vec<Theme> {
    let Ok(entries) = fs::read_dir(paths::themes_dir()) else {
        return Vec::new();
    };

    let themes = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|path| {
            let contents = fs::read_to_string(&path)
                .inspect_err(|e| warn!("Failed to read theme file {path:?}: {e}"))
                .ok()?;
            let theme = parse_theme(&contents, &path)
                .inspect_err(|e| warn!("Failed to parse theme file {path:?}: {e}"))
                .ok()?;
            debug!("Loaded custom theme '{}' from {path:?}", theme.name);
            Some(theme)
        })
        .collect();
    dedupe_and_sort(themes)
}

fn parse_theme(contents: &str, path: &Path) -> Result<Theme, toml::de::Error> {
    let mut theme = toml::from_str::<Theme>(contents)?;
    if theme.name.trim().is_empty() {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        theme.name = Cow::Owned(stem.unwrap_or_else(|| "Unnamed".to_owned()));
    }
    theme.source = Some(path.to_owned());
    Ok(theme)
}

/// Custom themes sharing a name (case-insensitive) collapse into one: the one whose file
/// name sorts last wins, so the outcome doesn't depend on directory listing order. The
/// result is sorted by name.
fn dedupe_and_sort(mut themes: Vec<Theme>) -> Vec<Theme> {
    themes.sort_by(|a, b| a.source.cmp(&b.source));
    let mut unique: Vec<Theme> = Vec::with_capacity(themes.len());
    for theme in themes {
        match unique.iter_mut().find(|t| t.same_name(&theme)) {
            Some(existing) => *existing = theme,
            None => unique.push(theme),
        }
    }
    unique.sort_by_key(|t| t.name.trim().to_lowercase());
    unique
}

/// Writes `theme` to the `themes/` folder. If a custom theme of the same name is already
/// loaded (see `existing`), its file is overwritten; otherwise a new file named after the
/// theme is created (or overwritten). Returns the path written.
pub fn save_theme(theme: &Theme, existing: &[Theme]) -> Result<PathBuf, String> {
    let name = theme.name.trim();
    if name.is_empty() {
        return Err("Enter a name for the theme first".to_owned());
    }
    let path = existing
        .iter()
        .find(|t| t.same_name(theme))
        .and_then(|t| t.source.clone())
        .unwrap_or_else(|| paths::themes_dir().join(format!("{}.toml", file_stem_for(name))));
    let contents =
        toml::to_string_pretty(theme).map_err(|e| format!("Failed to serialize theme: {e}"))?;
    fs::create_dir_all(paths::themes_dir())
        .and_then(|()| fs::write(&path, contents))
        .map_err(|e| format!("Failed to save theme to {}: {e}", path.display()))?;
    debug!("Saved theme '{name}' to {path:?}");
    Ok(path)
}

/// A file-name-safe version of a theme name.
fn file_stem_for(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = stem.trim();
    if stem.is_empty() {
        "theme".to_owned()
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(name: &str, file: &str, background: [u8; 3]) -> Theme {
        Theme {
            name: Cow::Owned(name.to_owned()),
            background,
            source: Some(PathBuf::from(file)),
            ..CLASSIC
        }
    }

    #[test]
    fn default_settings_use_classic() {
        assert!(CLASSIC.is_applied_to(&Settings::default()));
    }

    #[test]
    fn apply_then_matches() {
        let mut settings = Settings::default();
        for theme in &BUILTIN_THEMES {
            theme.apply_to(&mut settings);
            assert!(theme.is_applied_to(&settings), "{}", theme.name);
            let others = BUILTIN_THEMES.iter().filter(|t| t.name != theme.name);
            for other in others {
                assert!(!other.is_applied_to(&settings), "{} vs {}", theme.name, other.name);
            }
        }
    }

    #[test]
    fn parse_full_theme() {
        let theme = parse_theme(EXAMPLE_THEME, Path::new("x/mine.toml")).unwrap();
        assert_eq!(theme.name, "My Theme");
        assert_eq!(theme.accent, Some([180, 165, 230]));
        assert_eq!(theme.source.as_deref(), Some(Path::new("x/mine.toml")));
    }

    #[test]
    fn parse_without_name_or_accent() {
        let toml = "background = [1, 2, 3]\npast_line = [4, 5, 6]\n\
                    current_line = [7, 8, 9]\nfuture_line = [10, 11, 12]\n";
        let theme = parse_theme(toml, Path::new("themes/Dusk.toml")).unwrap();
        assert_eq!(theme.name, "Dusk");
        assert_eq!(theme.accent, None);
        assert_eq!(theme.background, [1, 2, 3]);
    }

    #[test]
    fn parse_rejects_missing_colors() {
        assert!(parse_theme("name = \"x\"", Path::new("x.toml")).is_err());
    }

    #[test]
    fn serialized_theme_round_trips() {
        let theme = custom("Round", "r.toml", [9, 9, 9]);
        let toml = toml::to_string_pretty(&theme).unwrap();
        let back = parse_theme(&toml, Path::new("r.toml")).unwrap();
        assert_eq!(back.name, theme.name);
        assert_eq!(back.background, theme.background);
        assert_eq!(back.accent, theme.accent);

        let no_accent = Theme {
            accent: None,
            ..theme
        };
        let toml = toml::to_string_pretty(&no_accent).unwrap();
        assert!(!toml.contains("accent"));
    }

    #[test]
    fn custom_themes_dedupe_and_sort() {
        let themes = dedupe_and_sort(vec![
            custom("zeta", "c.toml", [1, 1, 1]),
            custom("Alpha", "b.toml", [2, 2, 2]),
            custom("alpha", "a.toml", [3, 3, 3]),
        ]);
        let names: Vec<_> = themes.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, ["Alpha", "zeta"]);
        // b.toml sorts after a.toml, so it wins
        assert_eq!(themes[0].background, [2, 2, 2]);
    }

    #[test]
    fn custom_theme_overrides_builtin_in_place() {
        let themes = merge_themes(
            &BUILTIN_THEMES,
            vec![custom("ocean", "o.toml", [7, 7, 7]), custom("Mine", "m.toml", [8, 8, 8])],
        );
        assert_eq!(themes.len(), BUILTIN_THEMES.len() + 1);
        assert_eq!(themes[2].name, "ocean");
        assert_eq!(themes[2].background, [7, 7, 7]);
        assert_eq!(themes.last().unwrap().name, "Mine");
    }

    #[test]
    fn file_stems_are_safe() {
        assert_eq!(file_stem_for("My Theme"), "My Theme");
        assert_eq!(file_stem_for(" a/b:c "), "a_b_c");
        assert_eq!(file_stem_for("???"), "___");
        assert_eq!(file_stem_for("  "), "theme");
    }
}
