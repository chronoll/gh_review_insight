//! Claude Code CLI integration.
//!
//! The GUI launches one interactive `claude` per pull request, so each run is
//! visible (and continuable) exactly like normal terminal usage. Runs are
//! hosted by herdr (a terminal workspace manager with a socket API) — one
//! tab per PR in a dedicated workspace. A headless variant (`claude -p`,
//! JSON output, saved report) is kept below for a possible future switch
//! back.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config;

/// Locations commonly holding the `claude` binary. Home-relative entries are
/// expanded at runtime; a GUI launch (Dock / Spotlight) has a minimal PATH.
fn claude_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for rel in [".local/bin/claude", ".claude/local/claude"] {
            candidates.push(home.join(rel).to_string_lossy().into_owned());
        }
    }
    candidates.push("/opt/homebrew/bin/claude".to_string());
    candidates.push("/usr/local/bin/claude".to_string());
    candidates
}

/// Resolve the `claude` binary, mirroring how `gh.rs` resolves `gh`.
fn resolve_claude(preferred: &str) -> String {
    resolve_claude_in(preferred, &claude_candidates())
}

fn resolve_claude_in(preferred: &str, candidates: &[String]) -> String {
    if preferred.contains('/') && Path::new(preferred).exists() {
        return preferred.to_string();
    }
    for candidate in candidates {
        if Path::new(candidate).exists() {
            return candidate.clone();
        }
    }
    preferred.to_string()
}

/// PATH augmented with the common bin dirs, for tools `claude` itself spawns
/// (`gh` など). Needed because a `.app` launch inherits a minimal PATH.
fn augmented_path() -> String {
    let mut dirs: Vec<String> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for rel in [".local/bin", ".claude/local"] {
            dirs.push(home.join(rel).to_string_lossy().into_owned());
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        dirs.push(dir.to_string());
    }
    let existing = std::env::var("PATH").unwrap_or_default();
    format!("{}:{existing}", dirs.join(":"))
}

/// Tools pre-approved for the headless review run. `-p` mode cannot answer
/// permission prompts (anything not allowed is auto-denied), so everything
/// the review skill and its subagents use must be listed here. Matches the
/// gh-review skill's `allowed-tools` plus the orchestration tools.
const REVIEW_ALLOWED_TOOLS: &str = "Bash,Glob,Grep,Read,Agent,Task,TodoWrite,Skill";

/// Model and reasoning effort every `claude` invocation this app makes is
/// pinned to, so review quality doesn't drift with whatever the user's own
/// global default happens to be at the time.
const REVIEW_MODEL: &str = "sonnet";
const REVIEW_EFFORT: &str = "xhigh";

/// The prompt sent to `claude` for one PR: the PR URL そのもの。レビューは
/// pr-review スキルが URL を受けて発火する。
fn review_prompt(pr_url: &str) -> String {
    pr_url.to_string()
}

/// The prompt sent when re-reviewing a PR that was re-requested after a
/// prior AI review. Sent as the next turn of that review's own session (via
/// `--resume`), so claude keeps the context of what it already flagged and
/// can focus on what changed since.
fn re_review_prompt(pr_url: &str) -> String {
    format!(
        "{pr_url} が再リクエストされました。前回のレビュー以降の差分に注目してレビューしてください。"
    )
}

/// herdr workspace that hosts the review tabs.
const HERDR_WORKSPACE_LABEL: &str = "gh-review";

/// The herdr binary, when installed.
fn find_herdr() -> Option<String> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin/herdr"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/herdr"));
    candidates.push(PathBuf::from("/usr/local/bin/herdr"));
    candidates
        .into_iter()
        .find(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
}

/// Run a herdr CLI command and return the JSON `result` payload. herdr
/// reports failures as an `error` object, which is surfaced as the error.
fn herdr_run(args: &[&str]) -> Result<Value> {
    const ATTEMPTS: u32 = 3;
    let mut last_err = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            // A transient hiccup (seen with dozens of concurrent review
            // panes open) — give herdr a moment before retrying.
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        match herdr_run_once(args) {
            Ok(value) => return Ok(value),
            Err((transient, err)) => {
                if !transient {
                    return Err(err);
                }
                last_err = Some(err);
            }
        }
    }
    Err(last_err.unwrap())
}

/// Run a herdr CLI command once. The bool distinguishes a transient failure
/// (process spawn hiccup, or truncated/non-JSON output — worth retrying,
/// this has been observed under heavy load even though the command actually
/// went through server-side) from a genuine `{"error": ...}` response from
/// herdr itself (a real failure; retrying it wouldn't help).
fn herdr_run_once(args: &[&str]) -> std::result::Result<Value, (bool, anyhow::Error)> {
    let herdr = find_herdr().ok_or_else(|| (false, anyhow!("herdr が見つかりませんでした。")))?;
    let output = Command::new(herdr)
        .env("PATH", augmented_path())
        .args(args)
        .output()
        .map_err(|err| (true, anyhow!("herdr を実行できませんでした: {err}")))?;
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let trimmed = stderr.trim();
        (
            true,
            anyhow!(if trimmed.is_empty() {
                format!("herdr {} がJSON応答を返しませんでした。", args.first().unwrap_or(&""))
            } else {
                trimmed.to_string()
            }),
        )
    })?;
    if let Some(error) = value.get("error") {
        let message = error["message"].as_str().unwrap_or("不明なエラー");
        return Err((false, anyhow!("herdr {}: {message}", args.first().unwrap_or(&""))));
    }
    Ok(value["result"].clone())
}

/// Find or create the dedicated review workspace, returning its id.
fn ensure_herdr_workspace(cwd: &str) -> Result<String> {
    let list = herdr_run(&["workspace", "list"])?;
    let existing = list["workspaces"].as_array().and_then(|workspaces| {
        workspaces
            .iter()
            .find(|ws| ws["label"].as_str() == Some(HERDR_WORKSPACE_LABEL))
            .and_then(|ws| ws["workspace_id"].as_str())
            .map(str::to_string)
    });
    if let Some(id) = existing {
        return Ok(id);
    }
    let created = herdr_run(&[
        "workspace", "create", "--label", HERDR_WORKSPACE_LABEL, "--no-focus", "--cwd", cwd,
    ])?;
    created["workspace"]["workspace_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("herdr workspace create の応答から id を取得できませんでした。"))
}

/// Start `argv` as a herdr agent in the dedicated review workspace, moved
/// into its own tab labeled `window_name` (one tab per PR). Returns the new
/// tab's terminal id, used to focus it later.
fn spawn_herdr_tab(window_name: &str, argv: &[&str]) -> Result<String> {
    let workspace = workspace_dir()?;
    std::fs::create_dir_all(&workspace)
        .context("workspace ディレクトリを作成できませんでした")?;
    let cwd = workspace.to_string_lossy().into_owned();
    let ws_id = ensure_herdr_workspace(&cwd)?;

    let mut args = vec![
        "agent", "start", window_name, "--workspace", &ws_id, "--cwd", &cwd, "--no-focus", "--",
    ];
    args.extend_from_slice(argv);
    let started = herdr_run(&args)?;
    let agent = &started["agent"];
    let terminal_id = agent["terminal_id"].as_str().unwrap_or("").to_string();
    let pane_id = agent["pane_id"].as_str().unwrap_or("").to_string();
    if terminal_id.is_empty() || pane_id.is_empty() {
        return Err(anyhow!("herdr agent start の応答から id を取得できませんでした。"));
    }
    // `agent start` splits into the workspace's active tab; give the run its
    // own labeled tab. The label doubles as the identity check used by the
    // liveness filter, so a failure here is a real error.
    herdr_run(&[
        "pane", "move", &pane_id, "--new-tab", "--workspace", &ws_id, "--label", window_name,
        "--no-focus",
    ])?;
    Ok(terminal_id)
}

/// Generate a UUID for a new claude session via macOS's `uuidgen`. Passed to
/// `claude --session-id` so the id is known up front — an interactive run
/// prints no machine-readable session line, so this is the only way to learn
/// it before (or without) tearing the tab down.
fn generate_session_id() -> Result<String> {
    let output = Command::new("uuidgen")
        .env("PATH", augmented_path())
        .output()
        .map_err(|err| anyhow!("uuidgen を実行できませんでした: {err}"))?;
    if !output.status.success() {
        return Err(anyhow!("uuidgen が失敗しました。"));
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_lowercase();
    if id.is_empty() {
        return Err(anyhow!("uuidgen が空の出力を返しました。"));
    }
    Ok(id)
}

/// The live review panes in the herdr workspace: terminal id -> label.
/// Empty when herdr (or its server) is not running — panes are dead then.
fn live_herdr_reviews() -> HashMap<String, String> {
    let Ok(list) = herdr_run(&["workspace", "list"]) else {
        return HashMap::new();
    };
    let Some(ws_id) = list["workspaces"].as_array().and_then(|workspaces| {
        workspaces
            .iter()
            .find(|ws| ws["label"].as_str() == Some(HERDR_WORKSPACE_LABEL))
            .and_then(|ws| ws["workspace_id"].as_str())
            .map(str::to_string)
    }) else {
        return HashMap::new();
    };
    let Ok(panes) = herdr_run(&["pane", "list", "--workspace", &ws_id]) else {
        return HashMap::new();
    };
    panes["panes"]
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter_map(|pane| {
                    let terminal_id = pane["terminal_id"].as_str()?;
                    let label = pane["label"].as_str()?;
                    Some((terminal_id.to_string(), label.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pids of attached herdr clients. The server has no controlling tty and
/// transient CLI invocations carry a subcommand, so a client is a bare
/// `herdr` (optionally with `--flags`) that has a tty.
fn herdr_client_pids() -> Vec<String> {
    let Ok(output) = Command::new("ps").args(["-axo", "pid=,tty=,args="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?;
            let tty = fields.next()?;
            let cmd = fields.next()?;
            let arg1 = fields.next();
            let is_client = tty != "??"
                && (cmd == "herdr" || cmd.ends_with("/herdr"))
                && arg1.is_none_or(|arg| arg.starts_with('-'));
            is_client.then(|| pid.to_string())
        })
        .collect()
}

/// A finished headless review: the session to resume and the saved report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiSessionRecord {
    pub session_id: String,
    /// RFC3339 timestamp of when the run finished.
    pub completed_at: String,
    /// Absolute path of the saved report, if the run produced text.
    pub report_path: Option<String>,
}

impl AiSessionRecord {
    fn to_value(&self) -> Value {
        serde_json::json!({
            "session_id": self.session_id,
            "completed_at": self.completed_at,
            "report_path": self.report_path,
        })
    }

    fn from_value(value: &Value) -> Option<Self> {
        let session_id = value["session_id"].as_str()?.to_string();
        if session_id.is_empty() {
            return None;
        }
        Some(Self {
            session_id,
            completed_at: value["completed_at"].as_str().unwrap_or("").to_string(),
            report_path: value["report_path"].as_str().map(str::to_string),
        })
    }
}

/// A review launched interactively as a herdr tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchedRecord {
    /// herdr terminal id (`term_…`), used to focus this PR's tab.
    pub pane_id: String,
    /// The tab name the run was created with (`repo#42`), double-checking
    /// identity in case ids are ever reused.
    pub window_name: String,
    /// claude session id, assigned up front via `--session-id` (an
    /// interactive run has no machine-readable way to report its id
    /// otherwise). Lets the conversation be resumed in a fresh tab if this
    /// one's herdr pane disappears, e.g. after a reboot restarts herdr.
    /// Empty for records written before this field existed.
    pub session_id: String,
}

impl LaunchedRecord {
    fn to_value(&self) -> Value {
        serde_json::json!({
            "pane_id": self.pane_id,
            "window_name": self.window_name,
            "session_id": self.session_id,
            "backend": "herdr",
        })
    }

    fn from_value(value: &Value) -> Option<Self> {
        // Records from the retired tmux backend carry no (or another)
        // backend tag and are dropped.
        if value["backend"].as_str() != Some("herdr") {
            return None;
        }
        let pane_id = value["pane_id"].as_str()?.to_string();
        if pane_id.is_empty() {
            return None;
        }
        Some(Self {
            pane_id,
            window_name: value["window_name"].as_str().unwrap_or("").to_string(),
            session_id: value["session_id"].as_str().unwrap_or("").to_string(),
        })
    }
}

/// Load persisted launched reviews. Returns `(record, is_live)` per PR URL:
/// `is_live` says whether the herdr pane is actually running right now.
/// Entries whose pane is dead AND have no session id (nothing to resume,
/// e.g. written before session-id tracking existed) are dropped and the
/// file is rewritten; everything else is kept indefinitely so a resumable
/// session survives a herdr restart until it's resumed or overwritten by a
/// fresh run.
pub fn load_launched_reviews() -> HashMap<String, (LaunchedRecord, bool)> {
    let all = read_launched();
    if all.is_empty() {
        return HashMap::new();
    }
    let live = live_herdr_reviews();
    let mut kept: HashMap<String, (LaunchedRecord, bool)> = HashMap::new();
    let mut pruned = false;
    for (url, record) in all {
        let is_live = live.get(&record.pane_id) == Some(&record.window_name);
        if is_live || !record.session_id.is_empty() {
            kept.insert(url, (record, is_live));
        } else {
            pruned = true;
        }
    }
    if pruned {
        let to_save: HashMap<String, LaunchedRecord> = kept
            .iter()
            .map(|(url, (record, _))| (url.clone(), record.clone()))
            .collect();
        save_launched_reviews(&to_save);
    }
    kept
}

fn read_launched() -> HashMap<String, LaunchedRecord> {
    let Some(path) = config::ai_launched_path() else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    let Some(map) = value.as_object() else {
        return HashMap::new();
    };
    map.iter()
        .filter_map(|(url, v)| LaunchedRecord::from_value(v).map(|r| (url.clone(), r)))
        .collect()
}

/// Persist launched reviews, keyed by PR URL.
pub fn save_launched_reviews(launched: &HashMap<String, LaunchedRecord>) {
    let Some(path) = config::ai_launched_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let map: serde_json::Map<String, Value> = launched
        .iter()
        .map(|(url, record)| (url.clone(), record.to_value()))
        .collect();
    if let Ok(json) = serde_json::to_string_pretty(&Value::Object(map)) {
        let _ = std::fs::write(path, json);
    }
}

/// Directory where the pr-review skill writes HTML reports, as
/// `<repo>/<PR番号>-<YYYYMMDD>-<HHMMSS>.html`.
fn review_logs_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude/pr-review-logs"))
}

/// A review report on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewReport {
    /// Full path of the HTML file.
    pub path: String,
    /// Short timestamp label for the button (e.g. `07/20 13:22`).
    pub label: String,
}

/// All review reports on disk, keyed by `repo#番号` (the GUI's short PR id).
/// A PR can have multiple reports; they are ordered newest first.
pub fn load_review_reports() -> HashMap<String, Vec<ReviewReport>> {
    let Some(root) = review_logs_dir() else {
        return HashMap::new();
    };
    let Ok(repos) = std::fs::read_dir(&root) else {
        return HashMap::new();
    };
    let mut map: HashMap<String, Vec<(String, ReviewReport)>> = HashMap::new();
    for repo_entry in repos.flatten() {
        let repo_dir = repo_entry.path();
        if !repo_dir.is_dir() {
            continue;
        }
        let repo = repo_entry.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(&repo_dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("html") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some((number, label)) = parse_report_stem(stem) else {
                continue;
            };
            map.entry(format!("{repo}#{number}")).or_default().push((
                stem.to_string(),
                ReviewReport {
                    path: path.to_string_lossy().into_owned(),
                    label,
                },
            ));
        }
    }
    map.into_iter()
        .map(|(key, mut reports)| {
            // The zero-padded timestamp makes filename order chronological.
            reports.sort_by(|a, b| b.0.cmp(&a.0));
            (key, reports.into_iter().map(|(_, report)| report).collect())
        })
        .collect()
}

/// `12358-20260720-132207` -> the PR number and a short timestamp label
/// (`07/20 13:22`). None when the stem doesn't start with a PR number.
fn parse_report_stem(stem: &str) -> Option<(&str, String)> {
    let (number, timestamp) = stem.split_once('-')?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let label = match (
        timestamp.get(4..6),
        timestamp.get(6..8),
        timestamp.get(9..11),
        timestamp.get(11..13),
    ) {
        (Some(month), Some(day), Some(hour), Some(minute)) => {
            format!("{month}/{day} {hour}:{minute}")
        }
        _ => timestamp.to_string(),
    };
    Some((number, label))
}

/// Load persisted review sessions, keyed by PR URL.
pub fn load_sessions() -> HashMap<String, AiSessionRecord> {
    let Some(path) = config::ai_sessions_path() else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    let Some(map) = value.as_object() else {
        return HashMap::new();
    };
    map.iter()
        .filter_map(|(url, v)| AiSessionRecord::from_value(v).map(|r| (url.clone(), r)))
        .collect()
}

/// Persist review sessions, keyed by PR URL. Only the headless flow produces
/// new records; kept alongside `run_review` for a possible switch back.
#[allow(dead_code)]
pub fn save_sessions(sessions: &HashMap<String, AiSessionRecord>) {
    let Some(path) = config::ai_sessions_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let map: serde_json::Map<String, Value> = sessions
        .iter()
        .map(|(url, record)| (url.clone(), record.to_value()))
        .collect();
    if let Ok(json) = serde_json::to_string_pretty(&Value::Object(map)) {
        let _ = std::fs::write(path, json);
    }
}

/// Open the saved review report as a tab in a dedicated "review window",
/// reusing the same window across calls (mirrors the herdr tab
/// consolidation used for reviews themselves) so reports pile up as tabs
/// instead of scattering across new windows.
pub fn open_report(path: &str) -> Result<()> {
    if let Some(browser_app) = report_browser_app() {
        if open_in_review_window(&browser_app, &file_url(path)) {
            return Ok(());
        }
    }
    // Fallback: the OS default handler for the file. Always works, but
    // doesn't control which window/tab it lands in.
    Command::new("open")
        .arg(path)
        .spawn()
        .context("レビュー結果を開けませんでした")?;
    Ok(())
}

/// The browser reports open in: Chrome specifically, regardless of the OS
/// default browser (the user runs Arc system-wide but wants reports kept
/// separate in Chrome). Falls back to the OS default when Chrome isn't
/// installed, since some AppleScript-drivable browser is still worth trying
/// before giving up to the plain `open` fallback.
fn report_browser_app() -> Option<String> {
    find_chrome().or_else(default_browser_app)
}

/// Locations commonly holding a Google Chrome install.
fn find_chrome() -> Option<String> {
    let mut candidates = vec![PathBuf::from("/Applications/Google Chrome.app")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/Google Chrome.app"));
    }
    candidates
        .into_iter()
        .find(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
}

/// The user's default web browser's `.app` bundle path — whatever `open
/// https://…` would launch — found via `NSWorkspace` since browsers vary
/// widely in what CLI flags or AppleScript they support for opening a URL.
/// Used only as a fallback when Chrome isn't installed (see
/// `report_browser_app`).
fn default_browser_app() -> Option<String> {
    let script = r#"use framework "AppKit"
use scripting additions
set theURL to current application's NSURL's URLWithString:"https://example.com"
set appURL to current application's NSWorkspace's sharedWorkspace's URLForApplicationToOpenURL:theURL
return appURL's |path| as text"#;
    let output = Command::new("osascript").arg("-e").arg(script).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then_some(path)
}

/// Identity of the dedicated review window, persisted so it survives this
/// app restarting (the window itself lives in the browser process, which
/// keeps running independently).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReviewWindowRecord {
    /// The default browser this window belongs to. A window id found under
    /// a different browser would be meaningless (ids aren't shared across
    /// apps) or could even collide with an unrelated window.
    browser_app: String,
    /// AppleScript window id — session-scoped, valid only as long as the
    /// browser process and that specific window are still alive.
    window_id: i64,
}

fn load_review_window() -> Option<ReviewWindowRecord> {
    let path = config::review_browser_window_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(ReviewWindowRecord {
        browser_app: value["browser_app"].as_str()?.to_string(),
        window_id: value["window_id"].as_i64()?,
    })
}

fn save_review_window(record: &ReviewWindowRecord) {
    let Some(path) = config::review_browser_window_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let value = serde_json::json!({
        "browser_app": record.browser_app,
        "window_id": record.window_id,
    });
    if let Ok(json) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(path, json);
    }
}

/// Open `url` as a tab in the dedicated review window for `browser_app`,
/// creating that window on first use (or after the previous one was closed,
/// or the default browser changed) and reusing it otherwise. Returns false
/// when the browser can't be driven this way at all (e.g. Arc has no
/// AppleScript window/tab dictionary — every attempt below fails the same
/// way), letting the caller fall back to the OS default handler.
fn open_in_review_window(browser_app: &str, url: &str) -> bool {
    ensure_browser_running(browser_app);

    if let Some(record) = load_review_window() {
        if record.browser_app == browser_app
            && run_applescript(&add_tab_script(browser_app, record.window_id, url))
        {
            return true;
        }
        // Either a different browser is now default, or the window is gone
        // (closed, or the browser itself restarted) — fall through to open
        // a fresh one below.
    }

    let Some(window_id) = create_window_and_get_id(browser_app, url) else {
        return false;
    };
    save_review_window(&ReviewWindowRecord {
        browser_app: browser_app.to_string(),
        window_id,
    });
    true
}

/// Launch `browser_app` if it isn't already running, and give it a moment to
/// finish starting. Chrome (and likely other Chromium forks) restores the
/// previous session into whatever window it creates first after a cold
/// start; starting it ourselves first, ahead of `create_window_and_get_id`,
/// keeps that restore out of our dedicated review window.
fn ensure_browser_running(browser_app: &str) {
    let script = format!("application \"{}\" is running", applescript_escape(browser_app));
    let already_running = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "true")
        .unwrap_or(false);
    if already_running {
        return;
    }
    let launch = format!(
        "tell application \"{}\" to launch",
        applescript_escape(browser_app)
    );
    let _ = Command::new("osascript").arg("-e").arg(launch).output();
    std::thread::sleep(std::time::Duration::from_millis(1200));
}

/// Run an AppleScript, discarding its output. Returns whether it succeeded.
fn run_applescript(script: &str) -> bool {
    Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Create a brand-new window in `browser_app` showing `url` and return its
/// AppleScript window id. Safari's `make new document` always creates a new
/// window; other browsers use the window/tab AppleScript dictionary
/// Chromium itself provides (Chrome, Brave, Edge, Vivaldi, Opera, … — any
/// fork that hasn't removed it). Browsers without that dictionary (e.g. Arc)
/// fail here, which the caller treats as "can't be driven this way".
fn create_window_and_get_id(browser_app: &str, url: &str) -> Option<i64> {
    let app = applescript_escape(browser_app);
    let url = applescript_escape(url);
    let script = if browser_app.ends_with("/Safari.app") {
        format!(
            "tell application \"{app}\"\n\
             set newDoc to make new document with properties {{URL:\"{url}\"}}\n\
             return id of window 1\n\
             end tell"
        )
    } else {
        format!(
            "tell application \"{app}\"\n\
             set newWin to make new window\n\
             set URL of active tab of newWin to \"{url}\"\n\
             return id of newWin\n\
             end tell"
        )
    };
    let output = Command::new("osascript").arg("-e").arg(script).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// AppleScript that opens `url` as a new tab in the given window id, makes
/// it the active tab, and raises that window and the browser to the front.
fn add_tab_script(browser_app: &str, window_id: i64, url: &str) -> String {
    let app = applescript_escape(browser_app);
    let url = applescript_escape(url);
    let activate_tab = if browser_app.ends_with("/Safari.app") {
        "set current tab to newTab"
    } else {
        "set active tab index to (count of tabs)"
    };
    format!(
        "tell application \"{app}\"\n\
         if not (exists window id {window_id}) then error \"gh-review-insight: window gone\"\n\
         tell window id {window_id}\n\
         set newTab to make new tab with properties {{URL:\"{url}\"}}\n\
         {activate_tab}\n\
         end tell\n\
         set index of window id {window_id} to 1\n\
         end tell\n\
         activate application \"{app}\""
    )
}

/// Percent-encode a filesystem path into a `file://` URL. Spaces and other
/// reserved/non-ASCII bytes need encoding; `/` stays literal as the path
/// separator.
fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                url.push(byte as char);
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// Thin wrapper around the `claude` CLI.
pub struct ClaudeClient {
    pub claude_path: String,
}

impl ClaudeClient {
    pub fn new(claude_path: impl Into<String>) -> Self {
        Self {
            claude_path: claude_path.into(),
        }
    }

    /// Start an interactive `claude` for one PR as a herdr agent in the
    /// dedicated review workspace, moved into its own labeled tab (one tab
    /// per PR). A session id is assigned up front (`--session-id`) so the
    /// conversation can be resumed later even if this tab disappears.
    /// Returns quickly with the run's identity (used to focus its tab
    /// later); the review itself keeps running in its tab.
    pub fn launch_review(&self, pr_url: &str, pr_key: &str) -> Result<LaunchedRecord> {
        let window_name = pr_key.rsplit('/').next().unwrap_or(pr_key).to_string();
        let claude = resolve_claude(&self.claude_path);
        let prompt = review_prompt(pr_url);
        let session_id = generate_session_id()?;

        let pane_id = spawn_herdr_tab(
            &window_name,
            &[
                &claude,
                "--model",
                REVIEW_MODEL,
                "--effort",
                REVIEW_EFFORT,
                "--session-id",
                &session_id,
                &prompt,
            ],
        )?;
        Ok(LaunchedRecord {
            pane_id,
            window_name,
            session_id,
        })
    }

    /// Reopen a review whose herdr tab is gone as a fresh tab running
    /// `claude --resume <session_id>`, continuing the same conversation
    /// (same session id; herdr restarting is what killed the old tab, not
    /// the underlying transcript, which claude keeps on disk regardless).
    /// No new message is sent — this just re-opens the existing conversation.
    pub fn resume_review(&self, record: &LaunchedRecord) -> Result<LaunchedRecord> {
        self.continue_review(record, None)
    }

    /// Re-review a PR that was re-requested after a prior AI review: resumes
    /// that review's session (same session id, new tab) and immediately
    /// sends a prompt asking claude to focus on what changed since, using
    /// its own memory of the earlier review as the baseline.
    pub fn launch_re_review(&self, pr_url: &str, record: &LaunchedRecord) -> Result<LaunchedRecord> {
        self.continue_review(record, Some(&re_review_prompt(pr_url)))
    }

    /// Shared implementation for `resume_review` / `launch_re_review`: opens
    /// a new tab running `claude --resume <session_id>`, optionally with an
    /// extra argument that claude processes as the next turn.
    fn continue_review(&self, record: &LaunchedRecord, prompt: Option<&str>) -> Result<LaunchedRecord> {
        let claude = resolve_claude(&self.claude_path);
        let mut argv = vec![
            claude.as_str(),
            "--model",
            REVIEW_MODEL,
            "--effort",
            REVIEW_EFFORT,
            "--resume",
            record.session_id.as_str(),
        ];
        if let Some(prompt) = prompt {
            argv.push(prompt);
        }
        let pane_id = spawn_herdr_tab(&record.window_name, &argv)?;
        Ok(LaunchedRecord {
            pane_id,
            window_name: record.window_name.clone(),
            session_id: record.session_id.clone(),
        })
    }

    /// Switch herdr to the tab of this run, then bring the terminal to the
    /// front (or open one). Selecting the tab is best effort: it fails when
    /// the pane is gone (e.g. claude was exited), in which case the
    /// workspace is shown as-is.
    pub fn focus_review_window(&self, record: &LaunchedRecord) -> Result<()> {
        let _ = herdr_run(&["agent", "focus", &record.pane_id]);
        self.show_review_terminal(false)
    }

    /// Show the herdr client in a terminal: raise the window hosting an
    /// attached client when there is one, otherwise open a terminal running
    /// `herdr` (which attaches to the persistent session). With
    /// `keep_existing`, an attached client that cannot be raised (e.g. it
    /// lives in an unknown terminal app) is left alone instead of opening a
    /// second, mirroring client; used right after launching reviews.
    pub fn show_review_terminal(&self, keep_existing: bool) -> Result<()> {
        let pids = herdr_client_pids();
        let pid_refs: Vec<&str> = pids.iter().map(String::as_str).collect();
        if raise_hosting_terminal(&pid_refs, keep_existing)? {
            return Ok(());
        }
        let herdr = find_herdr().ok_or_else(|| anyhow!("herdr が見つかりませんでした。"))?;
        open_shell_in_terminal(&sh_quote(&herdr))
    }

    /// Run the review headlessly (`claude -p`, JSON output). Blocks until the
    /// review finishes (minutes), so call this from a background thread. The
    /// final report text is saved as `<workspace>/reviews/<pr_key>.md`.
    ///
    /// 現在の GUI は herdr での対話実行を使うため未使用だが、完了検知・
    /// レポート保存・セッション永続化ができる実装として保持している。
    #[allow(dead_code)]
    pub fn run_review(&self, pr_url: &str, pr_key: &str) -> Result<AiSessionRecord> {
        let workspace = workspace_dir()?;
        let reviews_dir = workspace.join("reviews");
        std::fs::create_dir_all(&reviews_dir)
            .context("reviews ディレクトリを作成できませんでした")?;

        let mut cmd = Command::new(resolve_claude(&self.claude_path));
        cmd.env("PATH", augmented_path());
        cmd.current_dir(&workspace);
        cmd.arg("-p")
            .arg(review_prompt(pr_url))
            .arg("--model")
            .arg(REVIEW_MODEL)
            .arg("--effort")
            .arg(REVIEW_EFFORT)
            .arg("--output-format")
            .arg("json")
            .arg("--allowedTools")
            .arg(REVIEW_ALLOWED_TOOLS);

        let output = cmd.output().map_err(|err| {
            anyhow!("Claude Code CLI `claude` を実行できませんでした: {err}")
        })?;

        let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let trimmed = stderr.trim();
            return Err(anyhow!(if trimmed.is_empty() {
                "`claude` がJSON応答を返しませんでした。".to_string()
            } else {
                trimmed.to_string()
            }));
        };
        let (session_id, report) = parse_run_output(&value)?;

        let report_path = if report.trim().is_empty() {
            None
        } else {
            let path = reviews_dir.join(format!("{}.md", sanitize_file_stem(pr_key)));
            std::fs::write(&path, &report).context("レビュー結果を保存できませんでした")?;
            Some(path.to_string_lossy().into_owned())
        };

        Ok(AiSessionRecord {
            session_id,
            completed_at: chrono::Utc::now().to_rfc3339(),
            report_path,
        })
    }

    /// Open a terminal and resume the review session interactively. The
    /// shell cd's into the workspace first because Claude Code looks up
    /// sessions per project directory.
    pub fn open_resume_terminal(&self, session_id: &str) -> Result<()> {
        let workspace = workspace_dir()?;
        open_shell_in_terminal(&format!(
            "cd {} && {} --resume {}",
            sh_quote(&workspace.to_string_lossy()),
            sh_quote(&resolve_claude(&self.claude_path)),
            sh_quote(session_id),
        ))
    }
}

/// Path of the Ghostty app bundle, when installed.
fn ghostty_app() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications/Ghostty.app")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/Ghostty.app"));
    }
    candidates.into_iter().find(|path| path.exists())
}

/// Open a shell command in a new window of the user's terminal: Ghostty when
/// installed, Terminal.app otherwise.
fn open_shell_in_terminal(shell: &str) -> Result<()> {
    match ghostty_app() {
        Some(app) => open_in_ghostty(&app, shell),
        None => open_in_terminal(shell),
    }
}

/// Open a new Ghostty window running a shell command. macOS Ghostty has no
/// IPC to reach the running instance (`ghostty +new-window` is Linux-only);
/// `open -na` is the way its own CLI help recommends. The spawned instance
/// would restore the saved window state (duplicating the user's existing
/// windows), so state saving is disabled to keep the window standalone.
fn open_in_ghostty(app: &Path, shell: &str) -> Result<()> {
    Command::new("open")
        .arg("-na")
        .arg(app)
        .args(["--args", "--window-save-state=never", "-e", "/bin/zsh", "-lc"])
        .arg(shell)
        .spawn()
        .context("Ghostty を開けませんでした")?;
    Ok(())
}

/// Open Terminal.app and run a shell command in a new window.
fn open_in_terminal(shell: &str) -> Result<()> {
    let script = format!(
        "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
        applescript_escape(shell),
    );
    Command::new("osascript")
        .arg("-e")
        .arg(script)
        .spawn()
        .context("Terminal を開けませんでした")?;
    Ok(())
}

/// Bring an app (all of its windows) to the front.
fn activate_app(name: &str) -> Result<()> {
    let script = format!(
        "tell application \"{}\" to activate",
        applescript_escape(name),
    );
    Command::new("osascript")
        .arg("-e")
        .arg(script)
        .spawn()
        .with_context(|| format!("{name} を前面に表示できませんでした"))?;
    Ok(())
}

/// Raise the terminal window hosting one of the given client processes.
/// Returns `true` when the request is handled: a window was raised, or a
/// client exists and `keep_existing` says to leave it alone. Returns `false`
/// when the caller should open a new terminal instead.
///
/// Terminal.app tabs are matched by tty. A client's own tty can differ from
/// the tab's (shell wrappers allocate their own pty), so the ttys of the
/// client process and all its ancestors are collected, noting which terminal
/// app hosts the client along the way.
fn raise_hosting_terminal(client_pids: &[&str], keep_existing: bool) -> Result<bool> {
    if client_pids.is_empty() {
        return Ok(false);
    }
    let mut ttys: Vec<String> = Vec::new();
    let mut ghostty_hosted = false;
    for pid in client_pids {
        let info = process_tree_info(pid);
        ghostty_hosted |= info.ghostty;
        for tty in info.ttys {
            if !ttys.contains(&tty) {
                ttys.push(tty);
            }
        }
    }
    if ghostty_hosted {
        // Ghostty has no AppleScript dictionary, so the specific window
        // cannot be selected; raising the app is best effort.
        activate_app("Ghostty")?;
        return Ok(true);
    }
    let tty_refs: Vec<&str> = ttys.iter().map(String::as_str).collect();
    if !tty_refs.is_empty() && raise_terminal_window(&tty_refs)? {
        return Ok(true);
    }
    Ok(keep_existing)
}

/// What hosts a multiplexer client, learned by walking up its process
/// ancestry.
#[derive(Default)]
struct ProcessTreeInfo {
    /// ttys of the process and its ancestors (as `/dev/ttysNNN`). A shell
    /// wrapper can put the client on its own pty, so the hosting Terminal
    /// tab's tty may only appear further up the chain.
    ttys: Vec<String>,
    /// An ancestor lives in Ghostty.app (the client runs in a Ghostty window).
    ghostty: bool,
}

/// Walk a process's parent chain up to launchd, collecting ttys and the
/// hosting terminal app.
fn process_tree_info(pid: &str) -> ProcessTreeInfo {
    let mut info = ProcessTreeInfo::default();
    let mut pid: i64 = pid.trim().parse().unwrap_or(0);
    for _ in 0..16 {
        if pid <= 1 {
            break;
        }
        let Ok(output) = Command::new("ps")
            .args(["-o", "ppid=,tty=,comm=", "-p", &pid.to_string()])
            .output()
        else {
            break;
        };
        if !output.status.success() {
            break;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut fields = text.split_whitespace();
        let (Some(ppid), Some(tty)) = (fields.next(), fields.next()) else {
            break;
        };
        let comm = fields.collect::<Vec<_>>().join(" ");
        if comm.contains("Ghostty.app") {
            info.ghostty = true;
        }
        if tty != "??" {
            let tty = format!("/dev/{tty}");
            if !info.ttys.contains(&tty) {
                info.ttys.push(tty);
            }
        }
        pid = ppid.parse().unwrap_or(0);
    }
    info
}

/// Bring the Terminal.app window whose tab hosts one of `ttys` to the front.
/// Returns `false` when no Terminal tab matches, e.g. when the client is
/// attached from another terminal app.
fn raise_terminal_window(ttys: &[&str]) -> Result<bool> {
    let list = ttys
        .iter()
        .map(|tty| format!("\"{}\"", applescript_escape(tty)))
        .collect::<Vec<_>>()
        .join(", ");
    let script = r#"tell application "Terminal"
    set targetTtys to {TTYS}
    repeat with w in windows
        repeat with t in tabs of w
            if (tty of t) is in targetTtys then
                set index of w to 1
                set selected tab of w to t
                activate
                return "found"
            end if
        end repeat
    end repeat
end tell
return """#
        .replace("TTYS", &list);
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context("Terminal のウィンドウ検索に失敗しました")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "Terminal のウィンドウ検索に失敗しました: {}",
            stderr.trim(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "found")
}

fn workspace_dir() -> Result<PathBuf> {
    config::workspace_dir()
        .ok_or_else(|| anyhow!("HOME が取得できず作業ディレクトリを決められませんでした。"))
}

/// Extract `(session_id, result_text)` from a `claude -p --output-format json`
/// response. An `is_error` response surfaces its message as the error.
fn parse_run_output(value: &Value) -> Result<(String, String)> {
    if value["is_error"].as_bool().unwrap_or(false) {
        let message = match value["result"].as_str() {
            Some(text) if !text.trim().is_empty() => text.to_string(),
            _ => format!(
                "claude の実行がエラーになりました（subtype: {}）",
                value["subtype"].as_str().unwrap_or("unknown"),
            ),
        };
        return Err(anyhow!(message));
    }
    let session_id = value["session_id"].as_str().unwrap_or("").to_string();
    if session_id.is_empty() {
        return Err(anyhow!("claude の応答から session_id を取得できませんでした。"));
    }
    Ok((session_id, value["result"].as_str().unwrap_or("").to_string()))
}

/// `owner/repo#42` -> `owner-repo-42` (filesystem-safe stem).
fn sanitize_file_stem(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Quote a string for /bin/sh (single quotes, embedded quotes escaped).
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Escape a string for embedding in a double-quoted AppleScript literal.
fn applescript_escape(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_run_output_extracts_session_and_result() {
        let value = json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "# レビュー結果\n指摘は3件です。",
            "session_id": "abc-123",
        });
        let (session_id, report) = parse_run_output(&value).unwrap();
        assert_eq!(session_id, "abc-123");
        assert!(report.starts_with("# レビュー結果"));
    }

    #[test]
    fn parse_run_output_surfaces_error_responses() {
        let value = json!({
            "is_error": true,
            "subtype": "error_during_execution",
            "result": "boom",
            "session_id": "abc",
        });
        let err = parse_run_output(&value).unwrap_err();
        assert!(err.to_string().contains("boom"));

        // Error without a message falls back to the subtype.
        let value = json!({"is_error": true, "subtype": "error_max_turns"});
        let err = parse_run_output(&value).unwrap_err();
        assert!(err.to_string().contains("error_max_turns"));

        // A success shape without a session id is still an error.
        let value = json!({"is_error": false, "result": "ok"});
        assert!(parse_run_output(&value).is_err());
    }

    #[test]
    fn session_record_roundtrips_through_json() {
        let record = AiSessionRecord {
            session_id: "s-1".to_string(),
            completed_at: "2026-07-08T00:00:00+00:00".to_string(),
            report_path: Some("/tmp/r.md".to_string()),
        };
        assert_eq!(AiSessionRecord::from_value(&record.to_value()), Some(record));

        let no_report = AiSessionRecord {
            session_id: "s-2".to_string(),
            completed_at: "2026-07-08T00:00:00+00:00".to_string(),
            report_path: None,
        };
        assert_eq!(
            AiSessionRecord::from_value(&no_report.to_value()),
            Some(no_report)
        );

        // Records without a session id are dropped.
        assert_eq!(AiSessionRecord::from_value(&json!({"report_path": "x"})), None);
    }

    #[test]
    fn process_tree_info_handles_bogus_pids() {
        assert!(process_tree_info("0").ttys.is_empty());
        assert!(process_tree_info("not-a-pid").ttys.is_empty());
        // A real pid must not panic (tty presence depends on the environment).
        let _ = process_tree_info(&std::process::id().to_string());
    }

    #[test]
    fn review_prompt_is_the_pr_url() {
        assert_eq!(
            review_prompt("https://github.com/acme/widgets/pull/42"),
            "https://github.com/acme/widgets/pull/42"
        );
    }

    #[test]
    fn file_url_percent_encodes_unsafe_bytes_but_keeps_slashes() {
        assert_eq!(
            file_url("/Users/michika kurotaka/日報.html"),
            "file:///Users/michika%20kurotaka/%E6%97%A5%E5%A0%B1.html"
        );
    }

    #[test]
    fn add_tab_script_uses_the_right_activation_idiom_per_browser() {
        let safari = add_tab_script("/Applications/Safari.app", 42, "file:///a.html");
        assert!(safari.contains("set current tab to newTab"));

        let chrome = add_tab_script("/Applications/Google Chrome.app", 42, "file:///a.html");
        assert!(chrome.contains("set active tab index to (count of tabs)"));

        // Both check the window still exists before touching it, and raise
        // the window + app afterwards.
        for script in [safari, chrome] {
            assert!(script.contains("exists window id 42"));
            assert!(script.contains("set index of window id 42 to 1"));
            assert!(script.contains("activate application"));
        }
    }

    #[test]
    fn re_review_prompt_mentions_re_request_and_the_url() {
        let prompt = re_review_prompt("https://github.com/acme/widgets/pull/42");
        assert!(prompt.contains("https://github.com/acme/widgets/pull/42"));
        assert!(prompt.contains("再リクエスト"));
    }

    #[test]
    fn report_stem_yields_pr_number_and_label() {
        assert_eq!(
            parse_report_stem("12358-20260720-132207"),
            Some(("12358", "07/20 13:22".to_string()))
        );
        // Unexpected timestamp shapes fall back to the raw text.
        assert_eq!(parse_report_stem("7-x"), Some(("7", "x".to_string())));
        // Files not starting with a PR number are skipped.
        assert_eq!(parse_report_stem("_general"), None);
        assert_eq!(parse_report_stem("notes-20260720"), None);
    }

    #[test]
    fn launched_record_roundtrips_through_json() {
        let record = LaunchedRecord {
            pane_id: "term_abc123".to_string(),
            window_name: "widgets#42".to_string(),
            session_id: "8b654f7f-042e-413e-9232-b7e0c9eba66b".to_string(),
        };
        assert_eq!(LaunchedRecord::from_value(&record.to_value()), Some(record));
        // Records from the retired tmux backend (no backend tag) are dropped.
        let legacy = serde_json::json!({"pane_id": "%1", "window_name": "w"});
        assert_eq!(LaunchedRecord::from_value(&legacy), None);
        // Records without a pane id are dropped.
        assert_eq!(
            LaunchedRecord::from_value(&serde_json::json!({"window_name": "x", "backend": "herdr"})),
            None
        );
        // Records written before session-id tracking default to empty.
        let pre_feature = serde_json::json!({
            "pane_id": "term_x", "window_name": "w", "backend": "herdr",
        });
        assert_eq!(
            LaunchedRecord::from_value(&pre_feature).map(|r| r.session_id),
            Some(String::new())
        );
    }

    #[test]
    fn pr_key_becomes_a_safe_file_stem() {
        assert_eq!(
            sanitize_file_stem("everytv/delish-web2#4501"),
            "everytv-delish-web2-4501"
        );
    }

    #[test]
    fn shell_and_applescript_quoting() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(applescript_escape(r#"say "hi" \o/"#), r#"say \"hi\" \\o/"#);
    }

    #[test]
    fn resolve_prefers_explicit_path_then_candidates() {
        let exe = std::env::current_exe().unwrap();
        let path = exe.to_string_lossy().to_string();
        assert_eq!(
            resolve_claude_in(&path, &["/nonexistent/claude".to_string()]),
            path
        );
        assert_eq!(resolve_claude_in("claude", &[path.clone()]), path);
        assert_eq!(
            resolve_claude_in("claude", &["/nope/claude".to_string()]),
            "claude"
        );
    }
}
