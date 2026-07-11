//! Claude Code CLI integration.
//!
//! The GUI launches one interactive `claude` per pull request inside a shared
//! tmux session, so each run is visible (and continuable) exactly like normal
//! terminal usage. A headless variant (`claude -p`, JSON output, saved report)
//! is kept below for a possible future switch back.

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

/// tmux session that hosts the interactive review runs (one pane per PR).
const TMUX_SESSION: &str = "gh-review";

/// The prompt sent to `claude` for one PR.
///
/// 検証モード: 本来は `/gh-review:review <URL>` を送るが、レビュースキルは
/// subagent を大量に起動しトークン消費が激しい（複数 PR 同時実行でレート
/// リミットに達した実績あり）ため、呼び出し経路の検証が済むまで即答する
/// 軽量プロンプトに差し替えている。戻すときは下の行を入れ替える。
fn review_prompt(pr_url: &str) -> String {
    format!("接続テストです。ツールを使わず「{pr_url} のレビュー依頼を受け付けました」とだけ一行で返答してください。")
    // 本来: format!("/gh-review:review {pr_url}")
}

/// Locations commonly holding `tmux` (Homebrew installs are missing from the
/// minimal PATH a GUI launch inherits).
fn tmux_candidates() -> Vec<String> {
    ["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/usr/bin/tmux"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn resolve_tmux() -> String {
    resolve_claude_in("tmux", &tmux_candidates())
}

/// Run a tmux command, returning stdout on success.
fn tmux_run(tmux: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(tmux)
        .env("PATH", augmented_path())
        .args(args)
        .output()
        .map_err(|err| {
            anyhow!("tmux を実行できませんでした（`brew install tmux` が必要です）: {err}")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux {} が失敗しました: {}",
            args.first().unwrap_or(&""),
            stderr.trim(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn tmux_has_session(tmux: &str) -> bool {
    Command::new(tmux)
        .env("PATH", augmented_path())
        .args(["has-session", "-t", TMUX_SESSION])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
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

/// Open the saved review report with the default application.
pub fn open_report(path: &str) -> Result<()> {
    Command::new("open")
        .arg(path)
        .spawn()
        .context("レビュー結果を開けませんでした")?;
    Ok(())
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

    /// Start an interactive `claude` for one PR inside the shared tmux
    /// session (created on first use), so the run is visible and continuable
    /// like normal terminal usage. Each PR gets its own tmux window (a tab
    /// in the status bar) named after the PR. Returns quickly with the new
    /// pane's id (used to focus this PR's tab later); the review itself
    /// keeps running in its window.
    pub fn launch_review_in_tmux(&self, pr_url: &str, pr_key: &str) -> Result<String> {
        let workspace = workspace_dir()?;
        std::fs::create_dir_all(&workspace)
            .context("workspace ディレクトリを作成できませんでした")?;
        let cwd = workspace.to_string_lossy().into_owned();
        let tmux = resolve_tmux();
        let command = format!(
            "{} {}",
            sh_quote(&resolve_claude(&self.claude_path)),
            sh_quote(&review_prompt(pr_url)),
        );
        // Tab label: `owner/repo#42` -> `repo#42` (matches the GUI's PR column).
        let window_name = pr_key.rsplit('/').next().unwrap_or(pr_key);

        let pane_id = if tmux_has_session(&tmux) {
            let target = format!("{TMUX_SESSION}:");
            tmux_run(
                &tmux,
                &["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", &target, "-n", window_name, "-c", &cwd, &command],
            )?
        } else {
            tmux_run(
                &tmux,
                &["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", TMUX_SESSION, "-n", window_name, "-c", &cwd, &command],
            )?
        };

        let pane_id = pane_id.trim().to_string();
        if !pane_id.is_empty() {
            // Keep the window name fixed on the PR: no rename from the
            // running command (automatic-rename) nor from the application's
            // title escape sequences (allow-rename).
            let _ = tmux_run(&tmux, &["set-option", "-w", "-t", &pane_id, "automatic-rename", "off"]);
            let _ = tmux_run(&tmux, &["set-option", "-w", "-t", &pane_id, "allow-rename", "off"]);
        }
        // Mouse support, applied on every launch so it also reaches sessions
        // created by older builds: clicking a window name in the status bar
        // switches tabs, and wheel scrolling works. (Native text selection
        // needs Shift while the mouse is captured by tmux.)
        let _ = tmux_run(&tmux, &["set-option", "-t", TMUX_SESSION, "mouse", "on"]);
        Ok(pane_id)
    }

    /// Switch the review session to the tab hosting `pane_id`, then bring
    /// the terminal to the front (or open one). Selecting the window is best
    /// effort: it fails when the pane is gone (e.g. claude was exited), in
    /// which case the session is shown as-is.
    pub fn focus_review_window(&self, pane_id: &str) -> Result<()> {
        let tmux = resolve_tmux();
        if !pane_id.is_empty() && tmux_has_session(&tmux) {
            let _ = tmux_run(&tmux, &["select-window", "-t", pane_id]);
        }
        self.show_tmux_terminal(false)
    }

    /// Show the review tmux session in Terminal.app. When a client is
    /// already attached, its window is raised instead of attaching again
    /// (a second attach would just mirror the session). When nothing is
    /// attached, a new attached Terminal window is opened. With
    /// `keep_existing`, an attached client that cannot be raised (e.g. it
    /// lives in another terminal app) is left alone instead of opening a
    /// mirror; used right after launching reviews.
    pub fn show_tmux_terminal(&self, keep_existing: bool) -> Result<()> {
        let tmux = resolve_tmux();
        if !tmux_has_session(&tmux) {
            return Err(anyhow!("tmux セッション {TMUX_SESSION} がありません。"));
        }
        let clients = tmux_run(&tmux, &["list-clients", "-t", TMUX_SESSION, "-F", "#{client_pid}"])?;
        let pids: Vec<&str> = clients
            .lines()
            .map(str::trim)
            .filter(|pid| !pid.is_empty())
            .collect();
        if !pids.is_empty() {
            // Terminal tabs are matched by tty. A client's own tty can differ
            // from the tab's (shell wrappers allocate their own pty), so
            // collect the ttys of the client process and all its ancestors,
            // noting which terminal app hosts the client along the way.
            let mut ttys: Vec<String> = Vec::new();
            let mut ghostty_hosted = false;
            for pid in &pids {
                let info = process_tree_info(pid);
                ghostty_hosted |= info.ghostty;
                for tty in info.ttys {
                    if !ttys.contains(&tty) {
                        ttys.push(tty);
                    }
                }
            }
            if ghostty_hosted {
                // Ghostty has no AppleScript dictionary, so the specific
                // window cannot be selected; raising the app is best effort.
                return activate_app("Ghostty");
            }
            let tty_refs: Vec<&str> = ttys.iter().map(String::as_str).collect();
            if (!tty_refs.is_empty() && raise_terminal_window(&tty_refs)?) || keep_existing {
                return Ok(());
            }
        }
        open_shell_in_terminal(&format!(
            "{} attach -t {}",
            sh_quote(&tmux),
            sh_quote(TMUX_SESSION),
        ))
    }

    /// Run the review headlessly (`claude -p`, JSON output). Blocks until the
    /// review finishes (minutes), so call this from a background thread. The
    /// final report text is saved as `<workspace>/reviews/<pr_key>.md`.
    ///
    /// 現在の GUI は tmux での対話実行を使うため未使用だが、完了検知・
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

/// What hosts a tmux client, learned by walking up its process ancestry.
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
/// Returns `false` when no Terminal tab matches, e.g. when the tmux client
/// is attached from another terminal app.
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
    fn review_prompt_embeds_the_pr_url() {
        let prompt = review_prompt("https://github.com/acme/widgets/pull/42");
        assert!(prompt.contains("https://github.com/acme/widgets/pull/42"));
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
