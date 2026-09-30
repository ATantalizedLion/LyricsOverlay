//! Where the app keeps its files: `%APPDATA%\LyricsOverlay`, so the exe can live (and be
//! run from) anywhere without scattering config, cache and logs next to it.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

const APP_FOLDER: &str = "LyricsOverlay";

/// Things older versions kept in the working directory, moved over by `init`.
const LEGACY_ITEMS: &[&str] = &["config.toml", "cache", "themes"];

pub fn data_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("APPDATA")
            .map_or_else(|| PathBuf::from("."), |dir| PathBuf::from(dir).join(APP_FOLDER))
    })
}

pub fn config_file() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn themes_dir() -> PathBuf {
    data_dir().join("themes")
}

/// Resolves a user-configured path: relative paths are taken relative to `data_dir`,
/// absolute ones are used as is.
pub fn resolve(path: &str) -> PathBuf {
    data_dir().join(path)
}

/// Creates the data folder, and on first run moves over files from older versions that
/// kept them in the working directory. Runs before logging is set up, so returns what it
/// did for the caller to log.
pub fn init() -> Vec<String> {
    let mut messages = Vec::new();
    if let Err(e) = fs::create_dir_all(data_dir()) {
        messages.push(format!("Failed to create data folder {}: {e}", data_dir().display()));
        return messages;
    }

    if config_file().exists() || !Path::new("config.toml").exists() {
        return messages;
    }
    for item in LEGACY_ITEMS {
        let from = Path::new(item);
        if !from.exists() {
            continue;
        }
        let to = data_dir().join(item);
        match move_item(from, &to) {
            Ok(()) => messages.push(format!("Moved {} to {}", from.display(), to.display())),
            Err(e) => messages.push(format!(
                "Failed to move {} to {}: {e}",
                from.display(),
                to.display()
            )),
        }
    }
    messages
}

/// Renames `from` to `to`, falling back to copy + delete when that's not possible (e.g.
/// the working directory is on another drive).
fn move_item(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir(from, to)?;
        fs::remove_dir_all(from)
    } else {
        fs::copy(from, to)?;
        fs::remove_file(from)
    }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
