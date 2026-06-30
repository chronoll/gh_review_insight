mod app;
mod config;
mod core;
mod gh;
mod model;

use anyhow::Result;

use crate::core::{StatsOptions, StatusOptions, collect_stats, collect_status};
use crate::gh::GhClient;
use crate::model::{PullRequestSummary, ReviewStatus, Stats, short_state};

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // CLI mode: `gh-review-insight status [--json]`
    //           `gh-review-insight stats  [--json]`
    // No args → launch GUI as before.
    if args.len() >= 2 && matches!(args[1].as_str(), "status" | "stats") {
        if let Err(e) = run_cli(&args[1..]) {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_title("gh-review-insight"),
        ..Default::default()
    };

    eframe::run_native(
        "gh-review-insight",
        native_options,
        Box::new(|cc| {
            app::install_japanese_font(&cc.egui_ctx);
            Ok(Box::new(app::App::new("gh".to_string())))
        }),
    )
}

fn run_cli(args: &[String]) -> Result<()> {
    let subcommand = &args[0];
    let json_mode = args.contains(&"--json".to_string());

    let ignored = config::load_ignored();
    let excludes = config::load_excludes();
    let client = GhClient::new("gh".to_string());

    match subcommand.as_str() {
        "status" => {
            let (login, prs) = collect_status(
                &client,
                &StatusOptions {
                    user: "@me".to_string(),
                    repos: vec![],
                    owners: vec![],
                    include_drafts: false,
                    no_reviewed: false,
                    limit: 50,
                    ignored,
                },
            )?;
            let visible: Vec<PullRequestSummary> = prs
                .into_iter()
                .filter(|pr| !is_excluded(&pr.url, &excludes))
                .collect();

            if json_mode {
                print_status_json(&login, &visible);
            } else {
                print_status_text(&login, &visible);
            }
        }
        "stats" => {
            let (login, stats) = collect_stats(&client, &StatsOptions::default())?;
            if json_mode {
                print_stats_json(&login, &stats);
            } else {
                print_stats_text(&login, &stats);
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}

fn is_excluded(url: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|raw| {
        let needle = raw.trim().trim_end_matches('/');
        !needle.is_empty() && (url == needle || url.starts_with(&format!("{needle}/")))
    })
}

fn print_status_text(login: &str, prs: &[PullRequestSummary]) {
    println!("=== gh-review-insight status (login: {login}) ===\n");

    let groups: &[(ReviewStatus, &str, &str)] = &[
        (ReviewStatus::RequestedUntouched, "waiting", "要レビュー・誰もまだレビューしていない"),
        (ReviewStatus::RequestedOthersReviewed, "should", "要レビュー・誰かがレビュー済み"),
        (ReviewStatus::Reviewed, "reviewed", "レビュー済み"),
        (ReviewStatus::Other, "other", "その他"),
    ];

    let mut idx = 1usize;
    for (status, label, description) in groups {
        let group: Vec<&PullRequestSummary> =
            prs.iter().filter(|pr| pr.review_status() == *status).collect();
        if group.is_empty() {
            continue;
        }
        println!("[{label}] {description}: {}件\n", group.len());
        for pr in group {
            let others = if pr.other_latest_reviews.is_empty() {
                "(なし)".to_string()
            } else {
                pr.other_latest_reviews
                    .iter()
                    .map(|r| format!("{} ({})", r.author, short_state(&r.state)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mine = match &pr.my_latest_review {
                Some(r) => short_state(&r.state),
                None => "-".to_string(),
            };
            let requested = if pr.requested_reviewers.is_empty() {
                "(なし)".to_string()
            } else {
                pr.requested_reviewers
                    .iter()
                    .map(|r| r.trim_start_matches('@').to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!("  {idx}. {}  {}", pr.pr_key(), pr.title);
            println!("     URL: {}", pr.url);
            println!("     Author: {} | Updated: {}", pr.author, &pr.updated_at[..10]);
            println!("     Requested: {requested}");
            println!("     My review: {mine} | Others: {others}");
            println!();
            idx += 1;
        }
    }
}

fn print_status_json(login: &str, prs: &[PullRequestSummary]) {
    let items: Vec<serde_json::Value> = prs
        .iter()
        .map(|pr| {
            serde_json::json!({
                "repository": pr.repository,
                "number": pr.number,
                "title": pr.title,
                "url": pr.url,
                "author": pr.author,
                "status": pr.review_status().label(),
                "updated_at": pr.updated_at,
                "requested_reviewers": pr.requested_reviewers,
                "my_latest_review": pr.my_latest_review.as_ref().map(|r| serde_json::json!({
                    "state": short_state(&r.state),
                    "submitted_at": r.submitted_at,
                })),
                "other_reviews": pr.other_latest_reviews.iter().map(|r| serde_json::json!({
                    "author": r.author,
                    "state": short_state(&r.state),
                    "submitted_at": r.submitted_at,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    let output = serde_json::json!({
        "login": login,
        "pull_requests": items,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}

fn print_stats_text(login: &str, stats: &Stats) {
    println!("=== gh-review-insight stats (login: {login}) ===\n");
    println!("期間: {} 〜 {}", &stats.since[..10], &stats.until[..10]);
    println!("レビュー提出数: {}", stats.own_review_submissions);
    println!("レビューしたPR数: {}", stats.unique_prs_reviewed);
    println!("対象PR上の全レビュー数: {}", stats.reviews_on_touched_prs);
    println!("自分の割合: {:.1}%", stats.own_share * 100.0);
    println!("他レビュワーもいたPR数: {}", stats.prs_with_other_reviewers);
    println!(
        "状態内訳: approved {} / changes {} / comment {} / dismissed {}",
        stats.approved, stats.changes_requested, stats.commented, stats.dismissed,
    );
    println!("候補PR数: {}", stats.candidate_prs);
}

fn print_stats_json(login: &str, stats: &Stats) {
    let output = serde_json::json!({
        "login": login,
        "since": stats.since,
        "until": stats.until,
        "own_review_submissions": stats.own_review_submissions,
        "unique_prs_reviewed": stats.unique_prs_reviewed,
        "reviews_on_touched_prs": stats.reviews_on_touched_prs,
        "own_share": stats.own_share,
        "prs_with_other_reviewers": stats.prs_with_other_reviewers,
        "approved": stats.approved,
        "changes_requested": stats.changes_requested,
        "commented": stats.commented,
        "dismissed": stats.dismissed,
        "candidate_prs": stats.candidate_prs,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}
