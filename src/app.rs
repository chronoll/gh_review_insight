//! The egui/eframe GUI. Shows the `status` view as a table of pull requests;
//! clicking a PR (its id or title) opens the GitHub page in the browser.
//!
//! Fetching runs on a background thread and the result is delivered over a
//! channel, so the gh subprocess never blocks the UI thread.

use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::core::{StatusOptions, collect_status};
use crate::gh::GhClient;
use crate::model::{PullRequestSummary, short_state};

type FetchResult = Result<(String, Vec<PullRequestSummary>), String>;

enum ViewState {
    Loading,
    Ready(Vec<PullRequestSummary>),
    Error(String),
}

pub struct App {
    gh_path: String,
    opts: StatusOptions,
    login: String,
    state: ViewState,
    rx: Option<Receiver<FetchResult>>,
    filter: String,
    started: bool,
}

impl App {
    pub fn new(opts: StatusOptions, gh_path: String) -> Self {
        Self {
            gh_path,
            opts,
            login: String::new(),
            state: ViewState::Loading,
            rx: None,
            filter: String::new(),
            started: false,
        }
    }

    fn start_fetch(&mut self, ctx: &egui::Context) {
        self.state = ViewState::Loading;
        let (tx, rx): (Sender<FetchResult>, Receiver<FetchResult>) = channel();
        self.rx = Some(rx);

        let gh_path = self.gh_path.clone();
        let opts = self.opts.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let client = GhClient::new(gh_path);
            let result = collect_status(&client, &opts).map_err(|err| format!("{err:#}"));
            let _ = tx.send(result);
            // Wake the UI so it picks up the result on the next frame.
            ctx.request_repaint();
        });
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok((login, prs)) => {
                        self.login = login;
                        self.state = ViewState::Ready(prs);
                    }
                    Err(message) => self.state = ViewState::Error(message),
                }
                self.rx = None;
            }
        }
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

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(Column::auto()) // status
            .column(Column::auto()) // PR
            .column(Column::auto()) // mine
            .column(Column::auto()) // others
            .column(Column::auto()) // requested
            .column(Column::auto()) // updated
            .column(Column::remainder()) // title
            .header(20.0, |mut header| {
                for label in ["status", "PR", "mine", "others", "requested", "updated", "title"] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|mut body| {
                for pr in &rows {
                    body.row(22.0, |mut row| {
                        row.col(|ui| {
                            ui.label(pr.self_status());
                        });
                        row.col(|ui| {
                            ui.hyperlink_to(pr.pr_key(), pr.url.as_str())
                                .on_hover_text(detail_text(pr));
                        });
                        row.col(|ui| {
                            ui.label(
                                pr.my_latest_review
                                    .as_ref()
                                    .map(|r| short_state(&r.state))
                                    .unwrap_or_else(|| "-".to_string()),
                            );
                        });
                        row.col(|ui| {
                            ui.label(others_text(pr));
                        });
                        row.col(|ui| {
                            ui.label(if pr.requested_reviewers.is_empty() {
                                "-".to_string()
                            } else {
                                pr.requested_reviewers.join(", ")
                            });
                        });
                        row.col(|ui| {
                            ui.label(pr.updated_at.chars().take(10).collect::<String>());
                        });
                        row.col(|ui| {
                            ui.hyperlink_to(pr.title.as_str(), pr.url.as_str())
                                .on_hover_text(detail_text(pr));
                        });
                    });
                }
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
            ui.horizontal(|ui| {
                ui.heading("gh-review-insight");
                if ui.button("⟳ 更新").clicked() {
                    self.start_fetch(ctx);
                }
                ui.separator();
                ui.label("フィルタ:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("title / repo / author")
                        .desired_width(220.0),
                );
                ui.separator();
                match &self.state {
                    ViewState::Loading => {
                        ui.spinner();
                        ui.label("読み込み中…");
                    }
                    ViewState::Error(_) => {
                        ui.colored_label(egui::Color32::RED, "取得エラー");
                    }
                    ViewState::Ready(prs) => {
                        ui.label(format!("{} 件 / {}", prs.len(), self.login));
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match &self.state {
            ViewState::Loading => {
                ui.label("PR を取得しています…");
            }
            ViewState::Error(message) => {
                ui.colored_label(egui::Color32::RED, message);
                ui.label("`gh` が認証済みか（`gh auth status`）確認してください。");
            }
            ViewState::Ready(prs) => self.table(ui, prs),
        });
    }
}

fn others_text(pr: &PullRequestSummary) -> String {
    if pr.other_latest_reviews.is_empty() {
        return "-".to_string();
    }
    pr.other_latest_reviews
        .iter()
        .map(|review| format!("{}:{}", review.author, short_state(&review.state)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn detail_text(pr: &PullRequestSummary) -> String {
    let mut lines = vec![
        format!("{}  ({})", pr.pr_key(), pr.state),
        pr.title.clone(),
        format!("author: {}{}", pr.author, if pr.is_draft { "  (draft)" } else { "" }),
    ];
    if let Some(decision) = &pr.review_decision {
        lines.push(format!("decision: {decision}"));
    }
    lines.push(format!(
        "created: {}   updated: {}",
        pr.created_at.chars().take(10).collect::<String>(),
        pr.updated_at.chars().take(10).collect::<String>(),
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
                review.submitted_at.chars().take(10).collect::<String>(),
                review.author,
                short_state(&review.state),
            ));
        }
    }
    lines.join("\n")
}
