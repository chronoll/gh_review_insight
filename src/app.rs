//! The egui/eframe GUI.
//!
//! Two views, toggled in the toolbar:
//! - **status**: a table of pull requests. Clicking a PR (its id or title)
//!   opens the GitHub page in the browser. Rows can be checked and reviewed
//!   in bulk by Claude Code: each selected PR gets an interactive `claude`
//!   as a herdr tab, so the run is visible and continuable like normal
//!   terminal usage.
//! - **stats**: aggregated review activity for the last N days.
//!
//! Fetching runs on a background thread and the result is delivered over a
//! channel, so the gh subprocess never blocks the UI thread.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::claude::{self, AiSessionRecord, ClaudeClient, LaunchedRecord, ReviewReport};
use crate::config::{colors_path, excludes_path, ignored_path, load_colors, load_excludes, load_ignored};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::core::{StatsOptions, StatusOptions, collect_stats, collect_status};
use crate::gh::GhClient;
use crate::model::{PullRequestSummary, ReviewStatus, Stats, short_state};

/// macOS system fonts that include Japanese glyphs, in order of preference.
/// `.ttc` collections are loaded at face index 0 (egui passes the index to
/// ab_glyph). Read at startup so the repo doesn't bundle a font.
const JP_FONT_CANDIDATES: &[&str] = &[
    // Prefer a heavier weight so all text (Latin + Japanese) reads as bold.
    "/System/Library/Fonts/ヒラギノ角ゴシック W6.ttc",
    "/System/Library/Fonts/ヒラギノ角ゴシック W5.ttc",
    "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/Library/Fonts/Arial Unicode.ttf",
];

/// Register a Japanese-capable font as the highest-priority face so labels
/// render instead of showing tofu (□). Falls back silently if none is found.
pub fn install_japanese_font(ctx: &egui::Context) {
    let Some(bytes) = JP_FONT_CANDIDATES.iter().find_map(|path| std::fs::read(path).ok()) else {
        eprintln!("warning: 日本語フォントが見つかりませんでした（日本語が□で表示されます）。");
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("japanese".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "japanese".to_owned());
    }
    ctx.set_fonts(fonts);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Status,
    Stats,
}

enum Loaded {
    Status(Vec<PullRequestSummary>),
    Stats(Stats),
}

type FetchResult = Result<(String, Loaded), String>;

enum ViewState {
    Loading,
    Ready(Loaded),
    Error(String),
}

/// Lifecycle of the Claude review for one PR (absent = not started).
enum AiReview {
    /// A launch/resume request is running on a background thread. herdr's
    /// CLI round-trip can legitimately take up to its retry ceiling (tens of
    /// seconds, e.g. while `agent_pane_busy` clears under load), so this
    /// state exists purely so the UI thread is never the one waiting on it.
    Pending,
    /// An interactive run as a herdr tab; holds the identity used to focus
    /// this PR's tab. Persisted and restored across app restarts while the
    /// pane is alive.
    Launched(LaunchedRecord),
    /// The herdr tab is gone (e.g. herdr restarted after a reboot), but the
    /// claude session id is known, so the conversation can be reopened in a
    /// new tab.
    Resumable(LaunchedRecord),
    /// A finished headless run (loaded from ai_sessions.json).
    Done(AiSessionRecord),
    Failed(String),
}

/// Outcome of a background launch/resume, delivered back to the UI thread
/// over a channel (see `App::ai_rx`) so herdr's occasionally-slow CLI calls
/// never run on the UI thread.
enum AiActionOutcome {
    Launched(LaunchedRecord),
    Failed(String),
}

/// Row interactions collected while drawing the table (which only has `&self`)
/// and applied to `self` afterwards.
#[derive(Default)]
struct TableActions {
    /// PR URLs whose selection checkbox was toggled.
    toggle: Vec<String>,
    /// Report file to open.
    open_report: Option<String>,
    /// Session id to resume in Terminal (headless flow).
    resume: Option<String>,
    /// Launched run whose tab should be focused and brought to the front.
    focus: Option<LaunchedRecord>,
    /// (PR url, record) whose herdr tab is gone and should be reopened.
    resume_herdr: Option<(String, LaunchedRecord)>,
}

pub struct App {
    gh_path: String,
    user: String,
    repos: Vec<String>,
    owners: Vec<String>,
    days: i64,
    mode: Mode,
    login: String,
    state: ViewState,
    rx: Option<Receiver<FetchResult>>,
    filter: String,
    started: bool,
    /// Per-user text colors (sRGB), keyed by GitHub login.
    colors: HashMap<String, [u8; 3]>,
    /// PR / repo URLs to hide from the status list.
    excludes: Vec<String>,
    /// Reviewer logins to ignore (e.g. bots) when counting reviews.
    ignored: Vec<String>,
    /// Settings window state.
    show_settings: bool,
    new_user: String,
    new_exclude: String,
    new_ignored: String,
    /// `claude` binary (resolved like `gh`; see claude.rs).
    claude_path: String,
    /// PR URLs checked for a batch AI review.
    selected: HashSet<String>,
    /// AI review state per PR URL. Persisted `Done` entries survive restarts.
    ai: HashMap<String, AiReview>,
    /// Review reports on disk, keyed by `repo#番号`, newest first. Rescanned
    /// on every status reload.
    reports: HashMap<String, Vec<ReviewReport>>,
    /// In-flight launch/resume background threads. Each yields `(pr_url,
    /// outcome)` pairs — one thread can drive several PRs in sequence (a
    /// batch launch), so results arrive one at a time rather than as a
    /// single value. herdr's CLI round-trip can legitimately take tens of
    /// seconds (e.g. `agent_pane_busy` retries under load), so these run off
    /// the UI thread; `poll` drains whichever have finished.
    ai_rx: Vec<Receiver<(String, AiActionOutcome)>>,
}

impl App {
    pub fn new(gh_path: String) -> Self {
        let mut ai: HashMap<String, AiReview> = claude::load_sessions()
            .into_iter()
            .map(|(url, record)| (url, AiReview::Done(record)))
            .collect();
        // Live (or resumable) herdr runs win over older headless records
        // for the same PR.
        for (url, (record, is_live)) in claude::load_launched_reviews() {
            let state = if is_live {
                AiReview::Launched(record)
            } else {
                AiReview::Resumable(record)
            };
            ai.insert(url, state);
        }
        Self {
            gh_path,
            user: "@me".to_string(),
            repos: Vec::new(),
            owners: Vec::new(),
            days: 30,
            mode: Mode::Status,
            login: String::new(),
            state: ViewState::Loading,
            rx: None,
            filter: String::new(),
            started: false,
            colors: load_colors(),
            excludes: load_excludes(),
            ignored: load_ignored(),
            show_settings: false,
            new_user: String::new(),
            new_exclude: String::new(),
            new_ignored: String::new(),
            claude_path: "claude".to_string(),
            selected: HashSet::new(),
            ai,
            reports: claude::load_review_reports(),
            ai_rx: Vec::new(),
        }
    }

    fn start_fetch(&mut self, ctx: &egui::Context) {
        self.state = ViewState::Loading;
        let (tx, rx): (Sender<FetchResult>, Receiver<FetchResult>) = channel();
        self.rx = Some(rx);

        let gh_path = self.gh_path.clone();
        let mode = self.mode;
        let user = self.user.clone();
        let repos = self.repos.clone();
        let owners = self.owners.clone();
        let days = self.days;
        let ignored = self.ignored.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let client = GhClient::new(gh_path);
            let result = match mode {
                Mode::Status => collect_status(
                    &client,
                    &StatusOptions {
                        user,
                        repos,
                        owners,
                        include_drafts: false,
                        no_reviewed: false,
                        limit: 50,
                        ignored,
                    },
                )
                .map(|(login, prs)| (login, Loaded::Status(prs))),
                Mode::Stats => collect_stats(
                    &client,
                    &StatsOptions {
                        user,
                        repos,
                        owners,
                        days,
                        limit: 200,
                        ignored,
                    },
                )
                .map(|(login, stats)| (login, Loaded::Stats(stats))),
            }
            .map_err(|err| format!("{err:#}"));

            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok((login, loaded)) => {
                        self.login = login;
                        if matches!(loaded, Loaded::Status(_)) {
                            // Reports may have been written since the last
                            // reload; rescan alongside the PR data.
                            self.reports = claude::load_review_reports();
                        }
                        self.state = ViewState::Ready(loaded);
                    }
                    Err(message) => self.state = ViewState::Error(message),
                }
                self.rx = None;
            }
        }

        // Drain every finished (or partially finished, for a batch launch)
        // background thread. A thread is done with a given receiver once its
        // sender drops, reported as `Disconnected` — keep the receiver
        // around otherwise so later results from the same batch still land.
        //
        // Only a successful `Launched` outcome should trigger a save:
        // `save_launched` rewrites the whole file from `self.ai`, and a
        // `Failed` resume/launch has nothing worth persisting — but if it
        // did trigger a save, the PR's *prior* on-disk record (e.g. a
        // `Resumable` entry with the session id to retry from) would be
        // wiped out along with it, since `self.ai` now holds `Failed` for
        // that url instead. A failure here must leave the file untouched.
        let mut any_launched = false;
        let mut still_running = Vec::new();
        for rx in std::mem::take(&mut self.ai_rx) {
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok((url, outcome)) => match outcome {
                        AiActionOutcome::Launched(record) => {
                            self.ai.insert(url, AiReview::Launched(record));
                            any_launched = true;
                        }
                        AiActionOutcome::Failed(message) => {
                            self.ai.insert(url, AiReview::Failed(message));
                        }
                    },
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if !disconnected {
                still_running.push(rx);
            }
        }
        self.ai_rx = still_running;
        if any_launched {
            self.save_launched();
        }
    }

    /// Check every actionable (waiting / should) visible PR.
    fn select_actionable(&mut self) {
        let ViewState::Ready(Loaded::Status(prs)) = &self.state else {
            return;
        };
        let urls: Vec<String> = prs
            .iter()
            .filter(|pr| !self.is_excluded(&pr.url))
            .filter(|pr| {
                matches!(
                    pr.review_status(),
                    ReviewStatus::RequestedUntouched | ReviewStatus::RequestedOthersReviewed
                )
            })
            .filter(|pr| !self.blocks_selection(pr))
            .map(|pr| pr.url.clone())
            .collect();
        self.selected.extend(urls);
    }

    /// True while the PR has a live review tab (a duplicate run makes no
    /// sense).
    fn is_launched(&self, url: &str) -> bool {
        matches!(self.ai.get(url), Some(AiReview::Launched(_)))
    }

    /// The prior review to continue when re-reviewing this PR: `Some` only
    /// when it was re-requested for review (`RequestedUntouched`) and we
    /// have a session to resume from an earlier AI review (Launched or
    /// Resumable both carry one). Checked regardless of the current tab's
    /// liveness, since resuming — not the tab surviving — is what carries
    /// claude's memory of the earlier review forward.
    fn re_review_target(&self, pr: &PullRequestSummary) -> Option<LaunchedRecord> {
        if pr.review_status() != ReviewStatus::RequestedUntouched {
            return None;
        }
        match self.ai.get(&pr.url) {
            Some(AiReview::Launched(record)) | Some(AiReview::Resumable(record)) => {
                Some(record.clone())
            }
            _ => None,
        }
    }

    /// True when selecting this PR for a new AI review should be disallowed:
    /// a tab is already running AND it isn't a fresh re-request (re-requests
    /// always get to re-review, even while the old tab is still open).
    fn blocks_selection(&self, pr: &PullRequestSummary) -> bool {
        // A launch/resume already in flight for this PR — never start a
        // second one on top of it.
        if matches!(self.ai.get(&pr.url), Some(AiReview::Pending)) {
            return true;
        }
        self.is_launched(&pr.url) && self.re_review_target(pr).is_none()
    }

    /// Launch one interactive `claude` per selected PR as a herdr tab, then
    /// show the herdr client (unless one is already visible). PRs that were
    /// re-requested after a prior AI review resume that review's session
    /// with a re-request prompt instead of starting a brand-new one.
    ///
    /// herdr's CLI round-trip can legitimately take tens of seconds (e.g.
    /// `agent_pane_busy` retries while a freshly split pane's shell is still
    /// starting up under load), so this runs on a background thread rather
    /// than the UI thread — otherwise the whole app would appear to hang for
    /// however long that retry takes.
    fn start_ai_reviews(&mut self, ctx: &egui::Context) {
        let ViewState::Ready(Loaded::Status(prs)) = &self.state else {
            return;
        };
        let targets: Vec<(String, String, Option<LaunchedRecord>)> = prs
            .iter()
            .filter(|pr| self.selected.contains(&pr.url))
            .filter(|pr| !self.blocks_selection(pr))
            .map(|pr| (pr.url.clone(), pr.pr_key(), self.re_review_target(pr)))
            .collect();
        if targets.is_empty() {
            return;
        }

        for (url, ..) in &targets {
            self.selected.remove(url);
            self.ai.insert(url.clone(), AiReview::Pending);
        }
        // Without this, the `Pending` state just set above wouldn't paint
        // until whatever next redraws the UI (e.g. a mouse move) — egui
        // doesn't repaint on its own just because state changed mid-frame.
        ctx.request_repaint();

        let claude_path = self.claude_path.clone();
        let (tx, rx) = channel();
        self.ai_rx.push(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let client = ClaudeClient::new(claude_path);
            let mut launched_any = false;
            for (url, pr_key, prior) in targets {
                let result = match &prior {
                    Some(record) => client.launch_re_review(&url, record),
                    None => client.launch_review(&url, &pr_key),
                };
                let outcome = match result {
                    Ok(record) => {
                        launched_any = true;
                        AiActionOutcome::Launched(record)
                    }
                    Err(err) => AiActionOutcome::Failed(format!("{err:#}")),
                };
                let _ = tx.send((url, outcome));
                ctx.request_repaint();
            }
            if launched_any {
                if let Err(err) = client.show_review_terminal(true) {
                    eprintln!("warning: {err:#}");
                }
            }
        });
    }

    /// Persist the currently launched/resumable (herdr) reviews.
    fn save_launched(&self) {
        let launched: HashMap<String, LaunchedRecord> = self
            .ai
            .iter()
            .filter_map(|(url, state)| match state {
                AiReview::Launched(record) | AiReview::Resumable(record) => {
                    Some((url.clone(), record.clone()))
                }
                _ => None,
            })
            .collect();
        claude::save_launched_reviews(&launched);
    }

    fn apply_table_actions(&mut self, actions: TableActions, ctx: &egui::Context) {
        for url in actions.toggle {
            if !self.selected.remove(&url) {
                self.selected.insert(url);
            }
        }
        if let Some(path) = actions.open_report {
            if let Err(err) = claude::open_report(&path) {
                eprintln!("warning: {err:#}");
            }
        }
        if let Some(session_id) = actions.resume {
            let client = ClaudeClient::new(self.claude_path.clone());
            if let Err(err) = client.open_resume_terminal(&session_id) {
                eprintln!("warning: {err:#}");
            }
        }
        if let Some(record) = actions.focus {
            // Off the UI thread: `agent focus` round-trips through the same
            // herdr CLI as launch/resume, so it can be just as slow.
            let claude_path = self.claude_path.clone();
            std::thread::spawn(move || {
                let client = ClaudeClient::new(claude_path);
                if let Err(err) = client.focus_review_window(&record) {
                    eprintln!("warning: {err:#}");
                }
            });
        }
        if let Some((url, record)) = actions.resume_herdr {
            debug_log(&format!("resume_herdr: begin url={url}"));
            self.ai.insert(url.clone(), AiReview::Pending);
            ctx.request_repaint();
            let claude_path = self.claude_path.clone();
            let (tx, rx) = channel();
            self.ai_rx.push(rx);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let client = ClaudeClient::new(claude_path);
                debug_log("resume_herdr: thread started, calling resume_review");
                let resume_result = client.resume_review(&record);
                debug_log(&format!(
                    "resume_herdr: resume_review returned ok={}",
                    resume_result.is_ok()
                ));
                let outcome = match resume_result {
                    Ok(new_record) => {
                        if let Err(err) = client.show_review_terminal(false) {
                            eprintln!("warning: {err:#}");
                        }
                        AiActionOutcome::Launched(new_record)
                    }
                    Err(err) => {
                        debug_log(&format!("resume_herdr: error = {err:#}"));
                        AiActionOutcome::Failed(format!("{err:#}"))
                    }
                };
                let send_result = tx.send((url, outcome));
                debug_log(&format!(
                    "resume_herdr: tx.send ok={}, calling request_repaint",
                    send_result.is_ok()
                ));
                ctx.request_repaint();
                debug_log("resume_herdr: thread ending");
            });
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.heading("gh-review-insight");
            ui.separator();
            if ui.selectable_value(&mut self.mode, Mode::Status, "status").clicked() {
                self.start_fetch(ctx);
            }
            if ui.selectable_value(&mut self.mode, Mode::Stats, "stats").clicked() {
                self.start_fetch(ctx);
            }
            if ui.button("⟳ 更新").clicked() {
                self.start_fetch(ctx);
            }
            if ui.button("⚙ 設定").clicked() {
                self.show_settings = !self.show_settings;
            }
            ui.separator();

            match self.mode {
                Mode::Status => {
                    ui.label("フィルタ:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("title / repo / author")
                            .desired_width(220.0),
                    );
                    ui.separator();
                    if ui
                        .button("未対応を選択")
                        .on_hover_text("waiting / should の PR をまとめて選択（個別には行頭のチェックボックスで）")
                        .clicked()
                    {
                        self.select_actionable();
                    }
                    let count = self.selected.len();
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(format!("AIレビュー実行 ({count})")),
                        )
                        .on_hover_text("選択した PR ごとに herdr のタブとして claude を起動（実行の様子をターミナルで確認できます）")
                        .clicked()
                    {
                        self.start_ai_reviews(ctx);
                    }
                    if count > 0 && ui.button("解除").clicked() {
                        self.selected.clear();
                    }
                }
                Mode::Stats => {
                    ui.label("days:");
                    ui.add(egui::DragValue::new(&mut self.days).range(1..=365));
                    ui.label("（変更後に「更新」）");
                }
            }

            ui.separator();
            match &self.state {
                ViewState::Loading => {
                    ui.spinner();
                    ui.label("読み込み中…");
                }
                ViewState::Error(_) => {
                    ui.colored_label(egui::Color32::RED, "取得エラー");
                }
                ViewState::Ready(Loaded::Status(prs)) => {
                    ui.label(format!("{} 件 / {}", prs.len(), self.login));
                }
                ViewState::Ready(Loaded::Stats(_)) => {
                    ui.label(format!("/ {}", self.login));
                }
            }
        });
    }

    fn table(&self, ui: &mut egui::Ui, prs: &[PullRequestSummary], actions: &mut TableActions) {
        let needle = self.filter.to_lowercase();
        let rows: Vec<&PullRequestSummary> = prs
            .iter()
            .filter(|pr| !self.is_excluded(&pr.url))
            .filter(|pr| {
                needle.is_empty()
                    || format!("{} {} {}", pr.title, pr.repository, pr.author)
                        .to_lowercase()
                        .contains(&needle)
            })
            .collect();

        if rows.is_empty() {
            ui.label("表示できる PR がありません。");
            return;
        }

        let dark = ui.visuals().dark_mode;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(Column::auto()) // select
            .column(Column::auto()) // status
            .column(Column::auto()) // PR
            .column(Column::auto()) // author
            .column(Column::remainder().clip(true)) // title
            .column(Column::auto()) // mine
            .column(Column::auto().at_least(80.0).at_most(220.0).clip(true)) // others
            .column(Column::auto().at_least(80.0).at_most(220.0).clip(true)) // requested
            .column(Column::auto()) // updated
            .column(Column::auto()) // AI
            .column(Column::auto()) // report
            .header(20.0, |mut header| {
                for label in [
                    "選択", "status", "PR", "author", "title", "mine", "others", "requested",
                    "updated", "AI", "report",
                ] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|mut body| {
                for pr in &rows {
                    let tint = status_tint(pr.review_status(), dark);
                    body.row(24.0, |mut row| {
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            let blocked = self.blocks_selection(pr);
                            let mut checked = !blocked && self.selected.contains(&pr.url);
                            let response =
                                ui.add_enabled(!blocked, egui::Checkbox::without_text(&mut checked));
                            let response = if blocked {
                                response.on_disabled_hover_text(
                                    "実行中のレビュータブがあるため選択できません",
                                )
                            } else if self.re_review_target(pr).is_some() {
                                response.on_hover_text(
                                    "再リクエストされています。実行すると前回のセッションを継続し、差分に注目してレビューします",
                                )
                            } else {
                                response
                            };
                            if response.changed() {
                                actions.toggle.push(pr.url.clone());
                            }
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.label(pr.review_status().label())
                                .on_hover_text(pr.review_status().description());
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.hyperlink_to(pr_short(pr), pr.url.as_str())
                                .on_hover_text(detail_text(pr));
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            self.user_label(ui, &pr.author);
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.hyperlink_to(pr.title.as_str(), pr.url.as_str())
                                .on_hover_text(detail_text(pr));
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            match &pr.my_latest_review {
                                Some(review) => {
                                    let label = short_state(&review.state);
                                    match state_color(&review.state, dark) {
                                        Some(color) => ui.colored_label(color, label),
                                        None => ui.label(label),
                                    };
                                }
                                None => {
                                    ui.label("-");
                                }
                            }
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            self.others_cell(ui, pr);
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            let text = if pr.requested_reviewers.is_empty() {
                                "-".to_string()
                            } else {
                                pr.requested_reviewers
                                    .iter()
                                    .map(|r| r.trim_start_matches('@'))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            };
                            ui.add(egui::Label::new(text.as_str()).truncate())
                                .on_hover_text(text);
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.label(date10(&pr.updated_at));
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            self.ai_cell(ui, pr, actions);
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            self.reports_cell(ui, pr, actions);
                        });
                    });
                }
            });
    }

    /// The "report" column: one button per review report found on disk for
    /// this PR (newest first), opening the HTML in the browser.
    fn reports_cell(&self, ui: &mut egui::Ui, pr: &PullRequestSummary, actions: &mut TableActions) {
        let reports = self.reports.get(&pr_short(pr));
        match reports {
            Some(reports) if !reports.is_empty() => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for report in reports {
                        if ui
                            .small_button(&report.label)
                            .on_hover_text(format!("レビューレポートを開く\n{}", report.path))
                            .clicked()
                        {
                            actions.open_report = Some(report.path.clone());
                        }
                    }
                });
            }
            _ => {
                ui.label("-");
            }
        }
    }

    /// The "AI" column: current review state, with the report / resume
    /// hand-off once a review has finished.
    fn ai_cell(&self, ui: &mut egui::Ui, pr: &PullRequestSummary, actions: &mut TableActions) {
        match self.ai.get(&pr.url) {
            None => {
                ui.label("-");
            }
            Some(AiReview::Pending) => {
                ui.add_enabled(false, egui::Button::new("…"))
                    .on_hover_text("herdr とやり取り中です（数秒〜数十秒かかることがあります）");
            }
            Some(AiReview::Launched(record)) => {
                if ui
                    .small_button("herdr")
                    .on_hover_text("実行中。クリックでこの PR のタブを前面に表示")
                    .clicked()
                {
                    actions.focus = Some(record.clone());
                }
            }
            Some(AiReview::Resumable(record)) => {
                if ui
                    .small_button("続き")
                    .on_hover_text(
                        "herdr の再起動などでタブが消えましたが、会話は保存されているので新しいタブで再開できます",
                    )
                    .clicked()
                {
                    actions.resume_herdr = Some((pr.url.clone(), record.clone()));
                }
            }
            Some(AiReview::Done(record)) => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if let Some(path) = &record.report_path {
                        if ui
                            .small_button("結果")
                            .on_hover_text("保存済みのレビューレポートを開く")
                            .clicked()
                        {
                            actions.open_report = Some(path.clone());
                        }
                    }
                    if ui
                        .small_button("続き")
                        .on_hover_text(format!(
                            "ターミナルでこのレビューの会話を再開（実行日: {}）",
                            date10(&record.completed_at),
                        ))
                        .clicked()
                    {
                        actions.resume = Some(record.session_id.clone());
                    }
                });
            }
            Some(AiReview::Failed(message)) => {
                ui.colored_label(egui::Color32::RED, "失敗")
                    .on_hover_text(format!("{message}\n（再選択して実行し直せます）"));
            }
        }
    }

    /// A user name, colored if the user has a custom color assigned.
    fn user_label(&self, ui: &mut egui::Ui, user: &str) {
        match self.user_color(user) {
            Some(color) => {
                ui.colored_label(color, user);
            }
            None => {
                ui.label(user);
            }
        }
    }

    /// The "others" column: each reviewer name colored individually on a single
    /// line (the column clips; the full list is shown on hover).
    fn others_cell(&self, ui: &mut egui::Ui, pr: &PullRequestSummary) {
        if pr.other_latest_reviews.is_empty() {
            ui.label("-");
            return;
        }
        let full = pr
            .other_latest_reviews
            .iter()
            .map(|r| format!("{}:{}", r.author, short_state(&r.state)))
            .collect::<Vec<_>>()
            .join(", ");
        let dark = ui.visuals().dark_mode;
        let response = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for review in &pr.other_latest_reviews {
                    let text = format!("{}:{}", review.author, short_state(&review.state));
                    // approved -> green; otherwise fall back to the user color.
                    let color = state_color(&review.state, dark)
                        .or_else(|| self.user_color(&review.author));
                    match color {
                        Some(color) => ui.colored_label(color, text),
                        None => ui.label(text),
                    };
                }
            })
            .response;
        response.on_hover_text(full);
    }

    fn user_color(&self, user: &str) -> Option<egui::Color32> {
        self.colors
            .get(user)
            .map(|[r, g, b]| egui::Color32::from_rgb(*r, *g, *b))
    }

    /// Logins seen in the current data, plus any already-colored ones.
    fn known_users(&self) -> Vec<String> {
        let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        if !self.login.is_empty() {
            set.insert(self.login.clone());
        }
        if let ViewState::Ready(Loaded::Status(prs)) = &self.state {
            for pr in prs {
                if !pr.author.is_empty() {
                    set.insert(pr.author.clone());
                }
                if let Some(review) = &pr.my_latest_review {
                    set.insert(review.author.clone());
                }
                for review in &pr.other_latest_reviews {
                    set.insert(review.author.clone());
                }
                for requested in &pr.requested_reviewers {
                    let name = requested.trim_start_matches('@');
                    // Skip teams (e.g. @org/platform).
                    if !name.is_empty() && !name.contains('/') {
                        set.insert(name.to_string());
                    }
                }
            }
        }
        for user in self.colors.keys() {
            set.insert(user.clone());
        }
        set.into_iter().collect()
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }

        let users = self.known_users();
        let snapshot = self.colors.clone();
        let excludes_snapshot = self.excludes.clone();
        let ignored_snapshot = self.ignored.clone();
        let mut open = self.show_settings;
        // Collected outside the closures, then applied to `self` afterwards, so
        // we never borrow `self` across nested egui closures.
        let mut edits: Vec<(String, Option<[u8; 3]>)> = Vec::new();
        let mut add_user: Option<String> = None;
        let mut add_exclude: Option<String> = None;
        let mut remove_exclude: Option<String> = None;
        let mut add_ignored: Option<String> = None;
        let mut remove_ignored: Option<String> = None;
        let new_user = &mut self.new_user;
        let new_exclude = &mut self.new_exclude;
        let new_ignored = &mut self.new_ignored;

        // Keep the window within the screen and scroll its whole body when the
        // lists (colors / excludes / ignored) grow tall.
        let max_height = (ctx.screen_rect().height() - 80.0).max(200.0);
        egui::Window::new("ユーザー色設定")
            .open(&mut open)
            .resizable(true)
            .vscroll(true)
            .max_height(max_height)
            .show(ctx, |ui| {
                ui.label("ユーザー名ごとに文字色を設定できます（author と reviewer 名に反映）。");
                ui.separator();
                egui::Grid::new("user_colors")
                    .num_columns(3)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        for user in &users {
                            ui.label(user);
                            let mut rgb = snapshot.get(user).copied().unwrap_or([220, 220, 220]);
                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                edits.push((user.clone(), Some(rgb)));
                            }
                            if snapshot.contains_key(user) {
                                if ui.button("解除").clicked() {
                                    edits.push((user.clone(), None));
                                }
                            } else {
                                ui.label("");
                            }
                            ui.end_row();
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("ユーザー追加:");
                    ui.text_edit_singleline(new_user);
                    if ui.button("追加").clicked() {
                        let trimmed = new_user.trim();
                        if !trimmed.is_empty() {
                            add_user = Some(trimmed.to_string());
                        }
                    }
                });

                ui.separator();
                ui.label("一覧から除外する GitHub リンク（PR またはリポジトリの URL）:");
                for url in &excludes_snapshot {
                    ui.horizontal(|ui| {
                        if ui.button("解除").clicked() {
                            remove_exclude = Some(url.clone());
                        }
                        ui.label(url);
                    });
                }
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(new_exclude);
                    if ui.button("除外に追加").clicked() {
                        let trimmed = new_exclude.trim();
                        if !trimmed.is_empty() {
                            add_exclude = Some(trimmed.to_string());
                        }
                    }
                });

                ui.separator();
                ui.label("レビュー集計で無視するアカウント（bot 等。他者レビュー扱いにしない）:");
                for user in &ignored_snapshot {
                    ui.horizontal(|ui| {
                        if ui.button("解除").clicked() {
                            remove_ignored = Some(user.clone());
                        }
                        ui.label(user);
                    });
                }
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(new_ignored);
                    if ui.button("無視に追加").clicked() {
                        let trimmed = new_ignored.trim().trim_start_matches('@');
                        if !trimmed.is_empty() {
                            add_ignored = Some(trimmed.to_string());
                        }
                    }
                });
            });

        let mut changed = false;
        for (user, color) in edits {
            match color {
                Some(rgb) => {
                    self.colors.insert(user, rgb);
                }
                None => {
                    self.colors.remove(&user);
                }
            }
            changed = true;
        }
        if let Some(user) = add_user {
            self.colors.entry(user).or_insert([255, 255, 255]);
            self.new_user.clear();
            changed = true;
        }
        if changed {
            self.save_colors();
        }

        let mut excludes_changed = false;
        if let Some(url) = add_exclude {
            if !self.excludes.iter().any(|e| e == &url) {
                self.excludes.push(url);
            }
            self.new_exclude.clear();
            excludes_changed = true;
        }
        if let Some(url) = remove_exclude {
            self.excludes.retain(|e| e != &url);
            excludes_changed = true;
        }
        if excludes_changed {
            self.save_excludes();
        }

        let mut ignored_changed = false;
        if let Some(user) = add_ignored {
            if !self.ignored.iter().any(|i| i == &user) {
                self.ignored.push(user);
            }
            self.new_ignored.clear();
            ignored_changed = true;
        }
        if let Some(user) = remove_ignored {
            self.ignored.retain(|i| i != &user);
            ignored_changed = true;
        }
        if ignored_changed {
            self.save_ignored();
            // Ignoring affects how reviews are counted, so reload the data.
            self.start_fetch(ctx);
        }

        self.show_settings = open;
    }

    fn save_colors(&self) {
        let Some(path) = colors_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.colors) {
            let _ = std::fs::write(path, json);
        }
    }

    /// True when a PR should be hidden. An entry matches the exact PR URL or,
    /// when it's a repo URL, any PR under it.
    fn is_excluded(&self, url: &str) -> bool {
        self.excludes.iter().any(|raw| {
            let needle = raw.trim().trim_end_matches('/');
            !needle.is_empty() && (url == needle || url.starts_with(&format!("{needle}/")))
        })
    }

    fn save_excludes(&self) {
        let Some(path) = excludes_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.excludes) {
            let _ = std::fs::write(path, json);
        }
    }

    fn save_ignored(&self) {
        let Some(path) = ignored_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.ignored) {
            let _ = std::fs::write(path, json);
        }
    }

    fn stats_view(&self, ui: &mut egui::Ui, stats: &Stats) {
        egui::Grid::new("stats_grid")
            .num_columns(2)
            .spacing([24.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                ui.strong("期間");
                ui.label(format!("{} 〜 {}", date10(&stats.since), date10(&stats.until)));
                ui.end_row();

                ui.strong("レビュー提出数");
                ui.label(stats.own_review_submissions.to_string());
                ui.end_row();

                ui.strong("レビューしたPR数");
                ui.label(stats.unique_prs_reviewed.to_string());
                ui.end_row();

                ui.strong("対象PR上の全レビュー数");
                ui.label(stats.reviews_on_touched_prs.to_string());
                ui.end_row();

                ui.strong("自分の割合");
                ui.label(format!("{:.1}%", stats.own_share * 100.0));
                ui.end_row();

                ui.strong("他レビュワーもいたPR数");
                ui.label(stats.prs_with_other_reviewers.to_string());
                ui.end_row();

                ui.strong("状態内訳");
                ui.label(format!(
                    "approved {} / changes {} / comment {} / dismissed {}",
                    stats.approved, stats.changes_requested, stats.commented, stats.dismissed,
                ));
                ui.end_row();

                ui.strong("候補PR数");
                ui.label(stats.candidate_prs.to_string());
                ui.end_row();
            });
    }
}

/// TEMPORARY debug instrumentation for the resume-herdr-stuck investigation.
/// Appends to `~/.config/gh-review-insight/debug.log`; remove once resolved.
fn debug_log(msg: &str) {
    use std::io::Write;
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = std::path::PathBuf::from(home).join(".config/gh-review-insight/debug.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "[{now}] {msg}");
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.started {
            self.started = true;
            self.start_fetch(ctx);
        }
        self.poll();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            self.toolbar(ui, ctx);
        });

        let mut actions = TableActions::default();
        egui::CentralPanel::default().show(ctx, |ui| match &self.state {
            ViewState::Loading => {
                ui.label("取得しています…");
            }
            ViewState::Error(message) => {
                ui.colored_label(egui::Color32::RED, message);
                ui.label("`gh` が認証済みか（`gh auth status`）確認してください。");
            }
            ViewState::Ready(Loaded::Status(prs)) => self.table(ui, prs, &mut actions),
            ViewState::Ready(Loaded::Stats(stats)) => self.stats_view(ui, stats),
        });
        self.apply_table_actions(actions, ctx);

        self.settings_window(ctx);
    }
}

fn date10(value: &str) -> String {
    value.chars().take(10).collect()
}

/// PR identifier without the organization: `owner/repo#42` -> `repo#42`.
fn pr_short(pr: &PullRequestSummary) -> String {
    let repo = pr.repository.rsplit('/').next().unwrap_or(&pr.repository);
    format!("{repo}#{}", pr.number)
}

/// Highlight color for a review state worth calling out. APPROVED -> green
/// (tuned per theme); everything else has no special color.
fn state_color(state: &str, dark: bool) -> Option<egui::Color32> {
    if state == "APPROVED" {
        Some(if dark {
            egui::Color32::from_rgb(120, 205, 130)
        } else {
            egui::Color32::from_rgb(30, 140, 60)
        })
    } else {
        None
    }
}

/// Paint the whole cell background (full width and height) with a faint tint.
fn fill_cell(ui: &egui::Ui, tint: Option<egui::Color32>) {
    if let Some(color) = tint {
        ui.painter().rect_filled(ui.max_rect(), 0.0, color);
    }
}

/// Very faint per-cell background per status (None = no tint). Subtle on
/// purpose, tuned for both themes.
fn status_tint(status: ReviewStatus, dark: bool) -> Option<egui::Color32> {
    use egui::Color32;
    let color = match (status, dark) {
        (ReviewStatus::RequestedUntouched, true) => Color32::from_rgba_unmultiplied(200, 80, 80, 22),
        (ReviewStatus::RequestedUntouched, false) => {
            Color32::from_rgba_unmultiplied(220, 110, 110, 30)
        }
        (ReviewStatus::RequestedOthersReviewed, true) => {
            Color32::from_rgba_unmultiplied(205, 170, 70, 20)
        }
        (ReviewStatus::RequestedOthersReviewed, false) => {
            Color32::from_rgba_unmultiplied(225, 190, 110, 30)
        }
        (ReviewStatus::Reviewed, true) => Color32::from_rgba_unmultiplied(90, 165, 100, 18),
        (ReviewStatus::Reviewed, false) => Color32::from_rgba_unmultiplied(150, 205, 160, 28),
        (ReviewStatus::Other, _) => return None,
    };
    Some(color)
}

fn detail_text(pr: &PullRequestSummary) -> String {
    let mut lines = vec![
        format!("{}  ({})", pr.pr_key(), pr.state),
        pr.title.clone(),
        format!(
            "author: {}{}",
            pr.author,
            if pr.is_draft { "  (draft)" } else { "" }
        ),
    ];
    if let Some(decision) = &pr.review_decision {
        lines.push(format!("decision: {decision}"));
    }
    lines.push(format!(
        "created: {}   updated: {}",
        date10(&pr.created_at),
        date10(&pr.updated_at),
    ));
    if !pr.sources.is_empty() {
        lines.push(format!("via: {}", pr.sources.join(", ")));
    }
    if !pr.reviews.is_empty() {
        lines.push(String::new());
        lines.push("reviews:".to_string());
        let mut timeline: Vec<&crate::model::ReviewSummary> = pr.reviews.iter().collect();
        timeline.sort_by(|a, b| a.submitted_at.cmp(&b.submitted_at));
        for review in timeline {
            lines.push(format!(
                "  {}  {:<16} {}",
                date10(&review.submitted_at),
                review.author,
                short_state(&review.state),
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::JP_FONT_CANDIDATES;
    use ab_glyph::{Font, FontVec};

    #[test]
    fn a_japanese_font_with_kana_and_kanji_is_available() {
        let bytes = JP_FONT_CANDIDATES
            .iter()
            .find_map(|path| std::fs::read(path).ok())
            .expect("日本語フォント候補が一つも見つかりませんでした");
        // index 0 mirrors how egui loads .ttc collections.
        let font = FontVec::try_from_vec_and_index(bytes, 0).expect("フォントを解析できませんでした");
        // glyph_id returns id 0 (.notdef) when the character is missing.
        for ch in ['あ', 'ア', '漢', 'レ'] {
            assert_ne!(font.glyph_id(ch).0, 0, "'{ch}' のグリフがありません");
        }
    }

    #[test]
    fn approved_state_gets_a_color_others_do_not() {
        assert!(super::state_color("APPROVED", true).is_some());
        assert!(super::state_color("APPROVED", false).is_some());
        assert!(super::state_color("COMMENTED", true).is_none());
        assert!(super::state_color("CHANGES_REQUESTED", false).is_none());
    }

    #[test]
    fn user_colors_serialize_roundtrip() {
        // The persistence format (HashMap<String, [u8; 3]> via serde_json).
        let mut map: std::collections::HashMap<String, [u8; 3]> = std::collections::HashMap::new();
        map.insert("alice".to_string(), [10, 20, 30]);
        let json = serde_json::to_string(&map).expect("serialize");
        let back: std::collections::HashMap<String, [u8; 3]> =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.get("alice"), Some(&[10, 20, 30]));
    }
}
