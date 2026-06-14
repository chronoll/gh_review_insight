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

impl PullRequestSummary {
    /// The viewer's relationship to this PR: requested / reviewed / both / seen.
    pub fn self_status(&self) -> &'static str {
        match (self.requested_for_user, self.my_latest_review.is_some()) {
            (true, true) => "reviewed+requested",
            (true, false) => "requested",
            (false, true) => "reviewed",
            (false, false) => "seen",
        }
    }

    /// `owner/repo#number`, used as a stable display/key string.
    pub fn pr_key(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }
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
