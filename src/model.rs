//! Data models returned by the `core` layer. Frontend-agnostic.

/// A single review submission on a pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    pub author: String,
    pub state: String,
    pub submitted_at: String,
    pub url: String,
}

/// A pull request enriched with the viewer's relationship to it.
#[derive(Clone, Debug)]
pub struct PullRequestSummary {
    pub repository: String,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub state: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub requested_reviewers: Vec<String>,
    pub requested_for_user: bool,
    pub sources: Vec<String>,
    pub reviews: Vec<ReviewSummary>,
    pub my_latest_review: Option<ReviewSummary>,
    pub other_latest_reviews: Vec<ReviewSummary>,
}

/// The viewer's actionable relationship to a PR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReviewStatus {
    /// Requested, but nobody has reviewed yet.
    RequestedUntouched,
    /// Requested, the viewer hasn't reviewed but someone else has.
    RequestedOthersReviewed,
    /// The viewer has reviewed it.
    Reviewed,
    /// Anything else (e.g. seen but not requested and not reviewed).
    Other,
}

impl ReviewStatus {
    /// Short label for the status column.
    pub fn label(self) -> &'static str {
        match self {
            ReviewStatus::RequestedUntouched => "要レビュー",
            ReviewStatus::RequestedOthersReviewed => "要レビュー(他者済)",
            ReviewStatus::Reviewed => "レビュー済",
            ReviewStatus::Other => "—",
        }
    }

    /// Full sentence used for tooltips.
    pub fn description(self) -> &'static str {
        match self {
            ReviewStatus::RequestedUntouched => "リクエストがあるが誰もレビューしていない",
            ReviewStatus::RequestedOthersReviewed => {
                "リクエストがあり自分はレビューしていないが、誰かがレビューした"
            }
            ReviewStatus::Reviewed => "リクエストがあり自分がレビューした（またはレビュー済み）",
            ReviewStatus::Other => "その他",
        }
    }
}

impl PullRequestSummary {
    /// Classify the viewer's relationship to this PR. Having reviewed it takes
    /// precedence; otherwise a standing request is split by whether anyone else
    /// has reviewed.
    pub fn review_status(&self) -> ReviewStatus {
        if self.my_latest_review.is_some() {
            ReviewStatus::Reviewed
        } else if self.requested_for_user {
            if self.other_latest_reviews.is_empty() {
                ReviewStatus::RequestedUntouched
            } else {
                ReviewStatus::RequestedOthersReviewed
            }
        } else {
            ReviewStatus::Other
        }
    }

    /// `owner/repo#number`, used as a stable display/key string.
    pub fn pr_key(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }
}

/// Aggregated review activity for a period.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub own_review_submissions: usize,
    pub unique_prs_reviewed: usize,
    pub reviews_on_touched_prs: usize,
    pub own_share: f64,
    pub prs_with_other_reviewers: usize,
    pub approved: usize,
    pub changes_requested: usize,
    pub commented: usize,
    pub dismissed: usize,
    pub candidate_prs: usize,
    /// RFC3339 window bounds (UTC).
    pub since: String,
    pub until: String,
}

/// Short, lower-case label for a GitHub review state.
pub fn short_state(state: &str) -> String {
    match state {
        "APPROVED" => "approved".to_string(),
        "CHANGES_REQUESTED" => "changes".to_string(),
        "COMMENTED" => "comment".to_string(),
        "DISMISSED" => "dismissed".to_string(),
        "PENDING" => "pending".to_string(),
        other => other.to_lowercase(),
    }
}
