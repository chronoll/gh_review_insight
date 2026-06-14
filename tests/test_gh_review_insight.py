import unittest
from datetime import datetime, timezone

from gh_review_insight import (
    calculate_stats,
    parse_date,
    parse_datetime,
    summarize_pull_request,
)


def fixture_pr():
    return {
        "author": {"login": "alice"},
        "createdAt": "2026-06-01T00:00:00Z",
        "isDraft": False,
        "number": 42,
        "repository": {"nameWithOwner": "acme/widgets"},
        "reviewDecision": "REVIEW_REQUIRED",
        "reviewRequests": {
            "nodes": [
                {"requestedReviewer": {"__typename": "User", "login": "bob"}},
                {
                    "requestedReviewer": {
                        "__typename": "Team",
                        "name": "Platform",
                        "slug": "platform",
                        "organization": {"login": "acme"},
                    }
                },
            ]
        },
        "reviews": {
            "nodes": [
                {
                    "author": {"login": "bob"},
                    "state": "COMMENTED",
                    "submittedAt": "2026-06-02T00:00:00Z",
                    "url": "https://github.com/acme/widgets/pull/42#review-1",
                },
                {
                    "author": {"login": "bob"},
                    "state": "APPROVED",
                    "submittedAt": "2026-06-03T00:00:00Z",
                    "url": "https://github.com/acme/widgets/pull/42#review-2",
                },
                {
                    "author": {"login": "carol"},
                    "state": "CHANGES_REQUESTED",
                    "submittedAt": "2026-06-03T12:00:00Z",
                    "url": "https://github.com/acme/widgets/pull/42#review-3",
                },
            ]
        },
        "state": "OPEN",
        "title": "Add widget filters",
        "updatedAt": "2026-06-04T00:00:00Z",
        "url": "https://github.com/acme/widgets/pull/42",
    }


class ReviewInsightTest(unittest.TestCase):
    def test_summarize_pull_request_marks_requested_and_latest_review(self):
        summary = summarize_pull_request(fixture_pr(), "bob", {"requested", "reviewed"})

        self.assertEqual(summary.self_status, "reviewed+requested")
        self.assertEqual(summary.my_latest_review.state, "APPROVED")
        self.assertEqual(summary.requested_reviewers, ("@bob", "@acme/platform"))
        self.assertEqual(len(summary.other_latest_reviews), 1)
        self.assertEqual(summary.other_latest_reviews[0].author, "carol")

    def test_calculate_stats_counts_reviews_in_window(self):
        summary = summarize_pull_request(fixture_pr(), "bob", {"reviewed"})
        stats = calculate_stats(
            "bob",
            [summary],
            datetime(2026, 6, 1, tzinfo=timezone.utc),
            datetime(2026, 6, 30, tzinfo=timezone.utc),
        )

        self.assertEqual(stats["ownReviewSubmissions"], 2)
        self.assertEqual(stats["uniquePullRequestsReviewed"], 1)
        self.assertEqual(stats["reviewSubmissionsOnTouchedPullRequests"], 3)
        self.assertEqual(stats["ownReviewShareOnTouchedPullRequests"], 0.6667)
        self.assertEqual(stats["pullRequestsWithOtherReviewers"], 1)
        self.assertEqual(stats["stateCounts"]["APPROVED"], 1)
        self.assertEqual(stats["stateCounts"]["COMMENTED"], 1)

    def test_parse_datetime_accepts_github_zulu_time(self):
        parsed = parse_datetime("2026-06-03T00:00:00Z")
        self.assertEqual(parsed.tzinfo, timezone.utc)

    def test_parse_date_can_include_full_until_day(self):
        parsed = parse_date("2026-06-14", end_of_day=True)

        self.assertEqual(parsed.isoformat(), "2026-06-14T23:59:59.999999+00:00")


if __name__ == "__main__":
    unittest.main()
