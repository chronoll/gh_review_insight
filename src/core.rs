//! Fetching and aggregation. Builds GitHub search queries, runs them through
//! `gh`, and turns the raw JSON nodes into `PullRequestSummary` values.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::gh::{GhClient, GqlVar};
use crate::model::{PullRequestSummary, ReviewSummary, Stats};

const PR_SEARCH_QUERY: &str = r#"
query($searchQuery: String!, $first: Int!, $after: String) {
  search(query: $searchQuery, type: ISSUE, first: $first, after: $after) {
    pageInfo {
      hasNextPage
      endCursor
    }
    nodes {
      ... on PullRequest {
        author {
          login
        }
        createdAt
        isDraft
        number
        repository {
          nameWithOwner
        }
        reviewDecision
        reviewRequests(first: 100) {
          nodes {
            requestedReviewer {
              __typename
              ... on User {
                login
              }
              ... on Team {
                name
                slug
                organization {
                  login
                }
              }
            }
          }
        }
        reviews(last: 100) {
          nodes {
            author {
              login
            }
            state
            submittedAt
            url
          }
        }
        state
        title
        updatedAt
        url
      }
    }
  }
}
"#;

/// Options for the `status` query, mirroring the CLI flags.
#[derive(Clone, Debug)]
pub struct StatusOptions {
    pub user: String,
    pub repos: Vec<String>,
    pub owners: Vec<String>,
    pub include_drafts: bool,
    pub no_reviewed: bool,
    pub limit: usize,
}

impl Default for StatusOptions {
    fn default() -> Self {
        Self {
            user: "@me".to_string(),
            repos: Vec::new(),
            owners: Vec::new(),
            include_drafts: false,
            no_reviewed: false,
            limit: 50,
        }
    }
}

/// Resolve `@me` to the viewer login. Returns `(login, search_user)` where
/// `search_user` keeps `@me` for `review-requested:@me` style qualifiers.
pub fn normalize_login(value: &str, client: &GhClient) -> Result<(String, String)> {
    if value == "@me" {
        let login = client.viewer_login()?;
        Ok((login, "@me".to_string()))
    } else {
        let login = value.trim_start_matches('@').to_string();
        Ok((login.clone(), login))
    }
}

fn scope_qualifiers(repos: &[String], owners: &[String]) -> Vec<String> {
    let mut qualifiers = Vec::new();
    qualifiers.extend(repos.iter().map(|repo| format!("repo:{repo}")));
    qualifiers.extend(owners.iter().map(|owner| format!("user:{owner}")));
    qualifiers
}

/// Page through a GitHub search until `limit` nodes are collected.
pub fn search_pull_requests(
    client: &GhClient,
    search_query: &str,
    limit: usize,
) -> Result<Vec<Value>> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;

    while nodes.len() < limit {
        let first = std::cmp::min(100, limit - nodes.len()) as i64;
        let mut vars = vec![
            ("searchQuery", GqlVar::Str(search_query.to_string())),
            ("first", GqlVar::Int(first)),
        ];
        if let Some(after) = &cursor {
            vars.push(("after", GqlVar::Str(after.clone())));
        }

        let data = client.graphql(PR_SEARCH_QUERY, &vars)?;
        let search = &data["data"]["search"];
        if let Some(page_nodes) = search["nodes"].as_array() {
            nodes.extend(page_nodes.iter().filter(|n| !n.is_null()).cloned());
        }

        if !search["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false) {
            break;
        }
        match search["pageInfo"]["endCursor"].as_str() {
            Some(end) if !end.is_empty() => cursor = Some(end.to_string()),
            _ => break,
        }
    }

    Ok(nodes)
}

/// Open PRs requesting the user's review, plus open PRs the user has reviewed.
pub fn collect_status(
    client: &GhClient,
    opts: &StatusOptions,
) -> Result<(String, Vec<PullRequestSummary>)> {
    let (login, search_user) = normalize_login(&opts.user, client)?;

    let mut common = vec![
        "is:pr".to_string(),
        "archived:false".to_string(),
        "is:open".to_string(),
    ];
    common.extend(scope_qualifiers(&opts.repos, &opts.owners));

    let build = |extra: String| {
        let mut parts = common.clone();
        parts.push(extra);
        parts.join(" ")
    };

    let mut searches: Vec<(&str, String)> =
        vec![("requested", build(format!("review-requested:{search_user}")))];
    if !opts.no_reviewed {
        searches.push(("reviewed", build(format!("reviewed-by:{login}"))));
    }

    // Merge by URL, remembering which searches each PR came from. `order`
    // preserves first-seen order (the requested bucket first).
    let mut order: Vec<String> = Vec::new();
    let mut by_url: HashMap<String, (Value, BTreeSet<String>)> = HashMap::new();
    for (source, query) in &searches {
        for node in search_pull_requests(client, query, opts.limit)? {
            let url = node["url"].as_str().unwrap_or("").to_string();
            if url.is_empty() {
                continue;
            }
            let entry = by_url.entry(url.clone()).or_insert_with(|| {
                order.push(url.clone());
                (node.clone(), BTreeSet::new())
            });
            entry.1.insert((*source).to_string());
        }
    }

    let mut summaries: Vec<PullRequestSummary> = order
        .iter()
        .map(|url| {
            let (node, sources) = &by_url[url];
            let sources: Vec<String> = sources.iter().cloned().collect();
            summarize_pull_request(node, &login, &sources)
        })
        .collect();

    if !opts.include_drafts {
        summaries.retain(|summary| !summary.is_draft);
    }

    summaries.sort_by(|a, b| {
        status_priority(a)
            .cmp(&status_priority(b))
            // Newer updates first within the same priority bucket.
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });

    Ok((login, summaries))
}

fn status_priority(summary: &PullRequestSummary) -> i32 {
    match summary.self_status() {
        "requested" => 0,
        "reviewed+requested" => 1,
        "reviewed" => 2,
        "seen" => 3,
        _ => 9,
    }
}

/// Turn a raw search node into a `PullRequestSummary` for the given viewer.
pub fn summarize_pull_request(
    node: &Value,
    login: &str,
    sources: &[String],
) -> PullRequestSummary {
    let request_nodes = node["reviewRequests"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let requested_reviewers: Vec<String> = request_nodes
        .iter()
        .filter_map(format_requested_reviewer)
        .collect();

    let reviews = parse_reviews(node);
    let latest_by_author = latest_reviews_by_author(&reviews);
    let my_latest_review = latest_by_author.get(login).cloned();
    // BTreeMap iterates in author order, matching the Python `sorted(...)`.
    let other_latest_reviews: Vec<ReviewSummary> = latest_by_author
        .iter()
        .filter(|(author, _)| author.as_str() != login)
        .map(|(_, review)| review.clone())
        .collect();

    let requested_for_user = sources.iter().any(|s| s == "requested")
        || requested_reviewers.iter().any(|r| r == &format!("@{login}"));

    PullRequestSummary {
        repository: node["repository"]["nameWithOwner"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        number: node["number"].as_i64().unwrap_or(0),
        title: node["title"].as_str().unwrap_or("").to_string(),
        url: node["url"].as_str().unwrap_or("").to_string(),
        author: node["author"]["login"].as_str().unwrap_or("").to_string(),
        state: node["state"].as_str().unwrap_or("").to_string(),
        is_draft: node["isDraft"].as_bool().unwrap_or(false),
        review_decision: node["reviewDecision"].as_str().map(str::to_string),
        created_at: node["createdAt"].as_str().unwrap_or("").to_string(),
        updated_at: node["updatedAt"].as_str().unwrap_or("").to_string(),
        requested_reviewers,
        requested_for_user,
        sources: sources.to_vec(),
        reviews,
        my_latest_review,
        other_latest_reviews,
    }
}

fn format_requested_reviewer(node: &Value) -> Option<String> {
    let reviewer = &node["requestedReviewer"];
    match reviewer["__typename"].as_str() {
        Some("User") => reviewer["login"].as_str().map(|login| format!("@{login}")),
        Some("Team") => {
            let slug = reviewer["slug"].as_str()?;
            match reviewer["organization"]["login"].as_str() {
                Some(org) => Some(format!("@{org}/{slug}")),
                None => Some(format!("@{slug}")),
            }
        }
        _ => None,
    }
}

fn parse_reviews(node: &Value) -> Vec<ReviewSummary> {
    let mut reviews = Vec::new();
    if let Some(nodes) = node["reviews"]["nodes"].as_array() {
        for review in nodes {
            let author = review["author"]["login"].as_str();
            let submitted_at = review["submittedAt"].as_str();
            let state = review["state"].as_str();
            if let (Some(author), Some(submitted_at), Some(state)) = (author, submitted_at, state) {
                if author.is_empty() || submitted_at.is_empty() || state.is_empty() {
                    continue;
                }
                reviews.push(ReviewSummary {
                    author: author.to_string(),
                    state: state.to_string(),
                    submitted_at: submitted_at.to_string(),
                    url: review["url"].as_str().unwrap_or("").to_string(),
                });
            }
        }
    }
    reviews
}

/// Keep only the latest review per author. ISO-8601 UTC strings compare
/// correctly lexicographically, so no datetime parsing is needed here.
fn latest_reviews_by_author(reviews: &[ReviewSummary]) -> BTreeMap<String, ReviewSummary> {
    let mut latest: BTreeMap<String, ReviewSummary> = BTreeMap::new();
    for review in reviews {
        let replace = match latest.get(&review.author) {
            Some(current) => review.submitted_at > current.submitted_at,
            None => true,
        };
        if replace {
            latest.insert(review.author.clone(), review.clone());
        }
    }
    latest
}

/// Options for the `stats` query.
#[derive(Clone, Debug)]
pub struct StatsOptions {
    pub user: String,
    pub repos: Vec<String>,
    pub owners: Vec<String>,
    pub days: i64,
    pub limit: usize,
}

impl Default for StatsOptions {
    fn default() -> Self {
        Self {
            user: "@me".to_string(),
            repos: Vec::new(),
            owners: Vec::new(),
            days: 30,
            limit: 200,
        }
    }
}

/// Summarize the viewer's review activity over the last `opts.days` days.
pub fn collect_stats(client: &GhClient, opts: &StatsOptions) -> Result<(String, Stats)> {
    let (login, _search_user) = normalize_login(&opts.user, client)?;
    let until = Utc::now();
    let since = until - Duration::days(opts.days.max(0));

    let mut parts = vec![
        "is:pr".to_string(),
        "archived:false".to_string(),
        format!("reviewed-by:{login}"),
        format!("updated:>={}", since.format("%Y-%m-%d")),
    ];
    parts.extend(scope_qualifiers(&opts.repos, &opts.owners));

    let nodes = search_pull_requests(client, &parts.join(" "), opts.limit)?;
    let summaries: Vec<PullRequestSummary> = nodes
        .iter()
        .map(|node| summarize_pull_request(node, &login, &["reviewed".to_string()]))
        .collect();

    let mut stats = calculate_stats(&login, &summaries, since, until);
    stats.candidate_prs = summaries.len();
    Ok((login, stats))
}

/// Parse a GitHub ISO-8601 timestamp (e.g. `2026-06-03T00:00:00Z`) as UTC.
pub fn parse_datetime(value: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("日時を解析できません: {value}"))?;
    Ok(parsed.with_timezone(&Utc))
}

/// Parse `YYYY-MM-DD` (or a full ISO timestamp) as UTC. With `end_of_day`, a
/// date-only value is pushed to the last microsecond of that day.
///
/// Kept for parity with the original tool's explicit `--since/--until` window
/// (the GUI currently exposes only a day count); covered by unit tests.
#[allow(dead_code)]
pub fn parse_date(value: &str, end_of_day: bool) -> Result<DateTime<Utc>> {
    if value.len() == 10 {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .with_context(|| format!("日付を解析できません: {value}"))?;
        let start = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap());
        if end_of_day {
            return Ok(start + Duration::days(1) - Duration::microseconds(1));
        }
        return Ok(start);
    }
    parse_datetime(value)
}

/// Aggregate review statistics over `[since, until]`.
pub fn calculate_stats(
    login: &str,
    summaries: &[PullRequestSummary],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Stats {
    let in_window = |value: &str| match parse_datetime(value) {
        Ok(dt) => since <= dt && dt <= until,
        Err(_) => false,
    };

    let mut own_reviews = 0usize;
    let mut all_reviews = 0usize;
    let mut unique: BTreeSet<String> = BTreeSet::new();
    let mut with_others: BTreeSet<String> = BTreeSet::new();
    let (mut approved, mut changes, mut commented, mut dismissed) = (0usize, 0usize, 0usize, 0usize);

    for summary in summaries {
        let key = summary.pr_key();
        let own_in_window: Vec<&ReviewSummary> = summary
            .reviews
            .iter()
            .filter(|r| r.author == login && in_window(&r.submitted_at))
            .collect();
        if own_in_window.is_empty() {
            continue;
        }

        unique.insert(key.clone());
        own_reviews += own_in_window.len();
        for review in &own_in_window {
            match review.state.as_str() {
                "APPROVED" => approved += 1,
                "CHANGES_REQUESTED" => changes += 1,
                "COMMENTED" => commented += 1,
                "DISMISSED" => dismissed += 1,
                _ => {}
            }
        }

        let reviews_in_window: Vec<&ReviewSummary> = summary
            .reviews
            .iter()
            .filter(|r| in_window(&r.submitted_at))
            .collect();
        all_reviews += reviews_in_window.len();
        if reviews_in_window.iter().any(|r| r.author != login) {
            with_others.insert(key);
        }
    }

    let share = if all_reviews > 0 {
        own_reviews as f64 / all_reviews as f64
    } else {
        0.0
    };

    Stats {
        own_review_submissions: own_reviews,
        unique_prs_reviewed: unique.len(),
        reviews_on_touched_prs: all_reviews,
        own_share: (share * 10000.0).round() / 10000.0,
        prs_with_other_reviewers: with_others.len(),
        approved,
        changes_requested: changes,
        commented,
        dismissed,
        candidate_prs: 0,
        since: since.to_rfc3339(),
        until: until.to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_pr() -> Value {
        json!({
            "author": {"login": "alice"},
            "createdAt": "2026-06-01T00:00:00Z",
            "isDraft": false,
            "number": 42,
            "repository": {"nameWithOwner": "acme/widgets"},
            "reviewDecision": "REVIEW_REQUIRED",
            "reviewRequests": {
                "nodes": [
                    {"requestedReviewer": {"__typename": "User", "login": "bob"}},
                    {"requestedReviewer": {
                        "__typename": "Team",
                        "name": "Platform",
                        "slug": "platform",
                        "organization": {"login": "acme"}
                    }}
                ]
            },
            "reviews": {
                "nodes": [
                    {"author": {"login": "bob"}, "state": "COMMENTED",
                     "submittedAt": "2026-06-02T00:00:00Z",
                     "url": "https://github.com/acme/widgets/pull/42#review-1"},
                    {"author": {"login": "bob"}, "state": "APPROVED",
                     "submittedAt": "2026-06-03T00:00:00Z",
                     "url": "https://github.com/acme/widgets/pull/42#review-2"},
                    {"author": {"login": "carol"}, "state": "CHANGES_REQUESTED",
                     "submittedAt": "2026-06-03T12:00:00Z",
                     "url": "https://github.com/acme/widgets/pull/42#review-3"}
                ]
            },
            "state": "OPEN",
            "title": "Add widget filters",
            "updatedAt": "2026-06-04T00:00:00Z",
            "url": "https://github.com/acme/widgets/pull/42"
        })
    }

    #[test]
    fn summarize_marks_requested_and_latest_review() {
        let summary = summarize_pull_request(
            &fixture_pr(),
            "bob",
            &["requested".to_string(), "reviewed".to_string()],
        );

        assert_eq!(summary.self_status(), "reviewed+requested");
        assert_eq!(summary.my_latest_review.as_ref().unwrap().state, "APPROVED");
        assert_eq!(
            summary.requested_reviewers,
            vec!["@bob".to_string(), "@acme/platform".to_string()]
        );
        assert_eq!(summary.other_latest_reviews.len(), 1);
        assert_eq!(summary.other_latest_reviews[0].author, "carol");
    }

    #[test]
    fn summarize_for_non_reviewer_marks_seen_when_not_requested() {
        let summary = summarize_pull_request(&fixture_pr(), "dave", &["reviewed".to_string()]);
        // dave neither requested nor has a review -> "seen".
        assert_eq!(summary.self_status(), "seen");
        assert!(summary.my_latest_review.is_none());
        assert_eq!(summary.other_latest_reviews.len(), 2);
    }

    #[test]
    fn calculate_stats_counts_reviews_in_window() {
        let summary = summarize_pull_request(&fixture_pr(), "bob", &["reviewed".to_string()]);
        let since = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let until = Utc.with_ymd_and_hms(2026, 6, 30, 0, 0, 0).unwrap();

        let stats = calculate_stats("bob", &[summary], since, until);

        assert_eq!(stats.own_review_submissions, 2);
        assert_eq!(stats.unique_prs_reviewed, 1);
        assert_eq!(stats.reviews_on_touched_prs, 3);
        assert!((stats.own_share - 0.6667).abs() < 1e-9);
        assert_eq!(stats.prs_with_other_reviewers, 1);
        assert_eq!(stats.approved, 1);
        assert_eq!(stats.commented, 1);
    }

    #[test]
    fn parse_datetime_accepts_github_zulu_time() {
        let parsed = parse_datetime("2026-06-03T00:00:00Z").unwrap();
        assert_eq!(parsed, Utc.with_ymd_and_hms(2026, 6, 3, 0, 0, 0).unwrap());
    }

    #[test]
    fn parse_date_can_include_full_until_day() {
        let parsed = parse_date("2026-06-14", true).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-06-14T23:59:59.999999+00:00");
    }
}
