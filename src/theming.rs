//! Color themes: a handful of built-in presets plus user-defined ones dropped into the
//! `themes/` folder as `.toml` files.
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

pub const THEMES_DIR: &str = "themes";

const EXAMPLE_THEME: &str = r#"# Copy this file, rename it to something.toml, and edit the colors to make your own
# theme. It'll show up as a preset button in Settings the next time you open the app.
# Colors are [red, green, blue], each 0-255.
name = "My Theme"
background = [0, 0, 0]
past_line = [200, 180, 255]
current_line = [255, 255, 255]
future_line = [180, 210, 255]
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub background: [u8; 3],
    pub past_line: [u8; 3],
    pub current_line: [u8; 3],
    pub future_line: [u8; 3],
}

impl Theme {
    fn new(
        name: &str,
        background: [u8; 3],
        past_line: [u8; 3],
        current_line: [u8; 3],
        future_line: [u8; 3],
    ) -> Self {
        Self {
            name: name.to_owned(),
            background,
            past_line,
            current_line,
            future_line,
        }
    }
}

pub fn builtin_themes() -> Vec<Theme> {
    vec![
        Theme::new(
            "Classic",
            [0, 0, 0],
            [200, 180, 255],
            [255, 255, 255],
            [180, 210, 255],
        ),
        Theme::new(
            "Sunset",
            [20, 8, 5],
            [255, 180, 120],
            [255, 255, 255],
            [255, 110, 90],
        ),
        Theme::new(
            "Ocean",
            [0, 8, 15],
            [150, 220, 255],
            [255, 255, 255],
            [80, 160, 220],
        ),
        Theme::new(
            "Forest",
            [4, 10, 5],
            [180, 255, 180],
            [255, 255, 255],
            [90, 200, 120],
        ),
        Theme::new(
            "Mono",
            [0, 0, 0],
            [160, 160, 160],
            [255, 255, 255],
            [100, 100, 100],
        ),
        Theme::new(
            "Metal",
            [6, 6, 8],
            [140, 140, 150],
            [255, 255, 255],
            [150, 25, 30],
        ),
        Theme::new(
            "Retrowave",
            [12, 4, 30],
            [255, 60, 180],
            [80, 255, 255],
            [140, 70, 220],
        ),
    ]
}

/// Creates the `themes/` folder (if missing) and seeds it with a documented example file,
/// so users have something to copy without needing to read source code.
pub fn ensure_themes_dir() {
    if fs::create_dir_all(THEMES_DIR).is_err() {
        return;
    }
    let example_path = Path::new(THEMES_DIR).join("example.toml.sample");
    if !example_path.exists() {
        let _ = fs::write(example_path, EXAMPLE_THEME);
    }
}

/// Scans `themes/` for `.toml` files (the `.sample` example is skipped since it doesn't
/// end in `.toml`) and parses each as a `Theme`. Invalid files are logged and skipped
/// rather than failing the whole load.
pub fn load_custom_themes() -> Vec<Theme> {
    let Ok(entries) = fs::read_dir(THEMES_DIR) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|entry| {
            let path = entry.path();
            let contents = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to read theme file {path:?}: {e}");
                    return None;
                }
            };
            match toml::from_str::<Theme>(&contents) {
                Ok(theme) => {
                    debug!("Loaded custom theme '{}' from {path:?}", theme.name);
                    Some(theme)
                }
                Err(e) => {
                    warn!("Failed to parse theme file {path:?}: {e}");
                    None
                }
            }
        })
        .collect()
}
