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

/// Completed AI review sessions, keyed by PR URL.
pub fn ai_sessions_path() -> Option<PathBuf> {
    Some(config_dir()?.join("ai_sessions.json"))
}

/// Launched (interactive tmux) AI reviews, keyed by PR URL.
pub fn ai_launched_path() -> Option<PathBuf> {
    Some(config_dir()?.join("ai_launched.json"))
}

/// Working directory for headless `claude` runs. Review reports are saved
/// under `<dir>/reviews/`, and sessions are resumed from this directory
/// (Claude Code looks up sessions per project directory).
pub fn workspace_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("workspace"))
}

/// Identity of the dedicated browser window review reports are opened into
/// (so repeated opens land as tabs in the same window).
pub fn review_browser_window_path() -> Option<PathBuf> {
    Some(config_dir()?.join("review_browser_window.json"))
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
