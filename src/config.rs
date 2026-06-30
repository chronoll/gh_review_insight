//! Persistent settings shared between the GUI and CLI.

use std::collections::HashMap;
use std::path::PathBuf;

fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/gh-review-insight"))
}

pub fn colors_path() -> Option<PathBuf> {
    Some(config_dir()?.join("colors.json"))
}

pub fn excludes_path() -> Option<PathBuf> {
    Some(config_dir()?.join("excludes.json"))
}

pub fn ignored_path() -> Option<PathBuf> {
    Some(config_dir()?.join("ignored.json"))
}

pub fn load_colors() -> HashMap<String, [u8; 3]> {
    colors_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn load_excludes() -> Vec<String> {
    excludes_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn load_ignored() -> Vec<String> {
    ignored_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}
