//! The egui/eframe GUI.
//!
//! Two views, toggled in the toolbar:
//! - **status**: a table of pull requests. Clicking a PR (its id or title)
//!   opens the GitHub page in the browser.
//! - **stats**: aggregated review activity for the last N days.
//!
//! Fetching runs on a background thread and the result is delivered over a
//! channel, so the gh subprocess never blocks the UI thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::core::{StatsOptions, StatusOptions, collect_stats, collect_status};
use crate::gh::GhClient;
use crate::model::{PullRequestSummary, ReviewStatus, Stats, short_state};

/// macOS system fonts that include Japanese glyphs, in order of preference.
/// `.ttc` collections are loaded at face index 0 (egui passes the index to
/// ab_glyph). Read at startup so the repo doesn't bundle a font.
const JP_FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
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
}

impl App {
    pub fn new(gh_path: String) -> Self {
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
            colors: HashMap::new(),
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
                        self.state = ViewState::Ready(loaded);
                    }
                    Err(message) => self.state = ViewState::Error(message),
                }
                self.rx = None;
            }
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
            ui.separator();

            match self.mode {
                Mode::Status => {
                    ui.label("フィルタ:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("title / repo / author")
                            .desired_width(220.0),
                    );
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

    fn table(&self, ui: &mut egui::Ui, prs: &[PullRequestSummary]) {
        let needle = self.filter.to_lowercase();
        let rows: Vec<&PullRequestSummary> = prs
            .iter()
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
            .column(Column::auto()) // status
            .column(Column::auto()) // PR
            .column(Column::auto()) // author
            .column(Column::auto()) // mine
            .column(Column::auto()) // others
            .column(Column::auto()) // requested
            .column(Column::auto()) // updated
            .column(Column::remainder()) // title
            .header(20.0, |mut header| {
                for label in [
                    "status", "PR", "author", "mine", "others", "requested", "updated", "title",
                ] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|mut body| {
                for pr in &rows {
                    let tint = status_tint(pr.review_status(), dark);
                    body.row(22.0, |mut row| {
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.label(pr.review_status().label())
                                .on_hover_text(pr.review_status().description());
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.hyperlink_to(pr.pr_key(), pr.url.as_str())
                                .on_hover_text(detail_text(pr));
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            self.user_label(ui, &pr.author);
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.label(
                                pr.my_latest_review
                                    .as_ref()
                                    .map(|r| short_state(&r.state))
                                    .unwrap_or_else(|| "-".to_string()),
                            );
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            self.others_cell(ui, pr);
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.label(if pr.requested_reviewers.is_empty() {
                                "-".to_string()
                            } else {
                                pr.requested_reviewers.join(", ")
                            });
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.label(date10(&pr.updated_at));
                        });
                        row.col(|ui| {
                            fill_cell(ui, tint);
                            ui.hyperlink_to(pr.title.as_str(), pr.url.as_str())
                                .on_hover_text(detail_text(pr));
                        });
                    });
                }
            });
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

    /// The "others" column: each reviewer name colored individually.
    fn others_cell(&self, ui: &mut egui::Ui, pr: &PullRequestSummary) {
        if pr.other_latest_reviews.is_empty() {
            ui.label("-");
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for review in &pr.other_latest_reviews {
                let text = format!("{}:{}", review.author, short_state(&review.state));
                match self.user_color(&review.author) {
                    Some(color) => ui.colored_label(color, text),
                    None => ui.label(text),
                };
            }
        });
    }

    fn user_color(&self, user: &str) -> Option<egui::Color32> {
        self.colors
            .get(user)
            .map(|[r, g, b]| egui::Color32::from_rgb(*r, *g, *b))
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

        egui::CentralPanel::default().show(ctx, |ui| match &self.state {
            ViewState::Loading => {
                ui.label("取得しています…");
            }
            ViewState::Error(message) => {
                ui.colored_label(egui::Color32::RED, message);
                ui.label("`gh` が認証済みか（`gh auth status`）確認してください。");
            }
            ViewState::Ready(Loaded::Status(prs)) => self.table(ui, prs),
            ViewState::Ready(Loaded::Stats(stats)) => self.stats_view(ui, stats),
        });
    }
}

fn date10(value: &str) -> String {
    value.chars().take(10).collect()
}

/// Paint a full-cell background so an entire row reads as one color.
fn fill_cell(ui: &egui::Ui, tint: Option<egui::Color32>) {
    if let Some(color) = tint {
        ui.painter()
            .rect_filled(ui.available_rect_before_wrap(), 0.0, color);
    }
}

/// Faint row background per status (None = no tint). Tuned for both themes.
fn status_tint(status: ReviewStatus, dark: bool) -> Option<egui::Color32> {
    use egui::Color32;
    let color = match (status, dark) {
        (ReviewStatus::RequestedUntouched, true) => Color32::from_rgba_unmultiplied(190, 70, 70, 48),
        (ReviewStatus::RequestedUntouched, false) => {
            Color32::from_rgba_unmultiplied(220, 120, 120, 80)
        }
        (ReviewStatus::RequestedOthersReviewed, true) => {
            Color32::from_rgba_unmultiplied(190, 160, 60, 44)
        }
        (ReviewStatus::RequestedOthersReviewed, false) => {
            Color32::from_rgba_unmultiplied(230, 200, 120, 90)
        }
        (ReviewStatus::Reviewed, true) => Color32::from_rgba_unmultiplied(80, 150, 90, 40),
        (ReviewStatus::Reviewed, false) => Color32::from_rgba_unmultiplied(170, 220, 180, 90),
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
}
