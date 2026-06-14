"""Core data layer for gh-review-insight.

Frontend-agnostic. Owns the `gh` transport (`GhClient`), the GraphQL queries,
fetching/aggregation logic, and the data models returned to frontends.

This module intentionally uses only the Python standard library and delegates
GitHub authentication/API transport to the official `gh` CLI.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Any, Iterable


PR_SEARCH_QUERY = """
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
"""


VIEWER_QUERY = """
query {
  viewer {
    login
  }
}
"""


REVIEW_STATES = ("APPROVED", "CHANGES_REQUESTED", "COMMENTED", "DISMISSED")


class GhError(RuntimeError):
    pass


@dataclass(frozen=True)
class ReviewSummary:
    author: str
    state: str
    submitted_at: str
    url: str


@dataclass(frozen=True)
class PullRequestSummary:
    repository: str
    number: int
    title: str
    url: str
    author: str
    state: str
    is_draft: bool
    review_decision: str | None
    created_at: str
    updated_at: str
    requested_reviewers: tuple[str, ...]
    requested_for_user: bool
    sources: tuple[str, ...]
    reviews: tuple[ReviewSummary, ...]
    my_latest_review: ReviewSummary | None
    other_latest_reviews: tuple[ReviewSummary, ...]

    @property
    def self_status(self) -> str:
        if self.requested_for_user and self.my_latest_review:
            return "reviewed+requested"
        if self.requested_for_user:
            return "requested"
        if self.my_latest_review:
            return "reviewed"
        return "seen"


class GhClient:
    def __init__(self, gh_path: str = "gh") -> None:
        self.gh_path = gh_path

    def graphql(self, query: str, variables: dict[str, Any] | None = None) -> dict[str, Any]:
        if shutil.which(self.gh_path) is None:
            raise GhError("GitHub CLI `gh` が見つかりません。先に `gh auth login` まで済ませてください。")

        args = [self.gh_path, "api", "graphql", "-f", f"query={query}"]
        for key, value in (variables or {}).items():
            if value is None:
                continue
            flag = "-F" if isinstance(value, (int, bool)) else "-f"
            args.extend([flag, f"{key}={value}"])

        completed = subprocess.run(args, capture_output=True, text=True)
        if completed.returncode != 0:
            stderr = completed.stderr.strip()
            raise GhError(stderr or "`gh api graphql` の実行に失敗しました。")

        try:
            return json.loads(completed.stdout)
        except json.JSONDecodeError as exc:
            raise GhError(f"`gh` のJSON応答を解析できませんでした: {exc}") from exc

    def viewer_login(self) -> str:
        data = self.graphql(VIEWER_QUERY)
        login = data.get("data", {}).get("viewer", {}).get("login")
        if not login:
            raise GhError("GitHub viewer login を取得できませんでした。")
        return login


def normalize_login(value: str, client: GhClient) -> tuple[str, str]:
    if value == "@me":
        login = client.viewer_login()
        return login, "@me"
    login = value.removeprefix("@")
    return login, login


def scope_qualifiers(repos: Iterable[str], owners: Iterable[str]) -> list[str]:
    qualifiers: list[str] = []
    qualifiers.extend(f"repo:{repo}" for repo in repos)
    qualifiers.extend(f"user:{owner}" for owner in owners)
    return qualifiers


def state_qualifier(state: str) -> list[str]:
    if state == "open":
        return ["is:open"]
    if state == "closed":
        return ["is:closed"]
    if state == "merged":
        return ["is:merged"]
    return []


def search_pull_requests(client: GhClient, search_query: str, limit: int) -> list[dict[str, Any]]:
    nodes: list[dict[str, Any]] = []
    cursor: str | None = None

    while len(nodes) < limit:
        first = min(100, limit - len(nodes))
        data = client.graphql(
            PR_SEARCH_QUERY,
            {"searchQuery": search_query, "first": first, "after": cursor},
        )
        search = data.get("data", {}).get("search", {})
        page_nodes = [node for node in search.get("nodes", []) if node]
        nodes.extend(page_nodes)

        page_info = search.get("pageInfo", {})
        if not page_info.get("hasNextPage"):
            break
        cursor = page_info.get("endCursor")
        if not cursor:
            break

    return nodes


def collect_status(args: argparse.Namespace, client: GhClient) -> tuple[str, list[PullRequestSummary]]:
    login, search_user = normalize_login(args.user, client)
    common = ["is:pr", "archived:false", "is:open", *scope_qualifiers(args.repo, args.owner)]
    searches = [
        ("requested", " ".join([*common, f"review-requested:{search_user}"])),
    ]
    if not args.no_reviewed:
        searches.append(("reviewed", " ".join([*common, f"reviewed-by:{login}"])))

    by_url: dict[str, tuple[dict[str, Any], set[str]]] = {}
    for source, query in searches:
        for node in search_pull_requests(client, query, args.limit):
            url = node.get("url")
            if not url:
                continue
            if url not in by_url:
                by_url[url] = (node, set())
            by_url[url][1].add(source)

    summaries = [
        summarize_pull_request(node, login, sources)
        for node, sources in by_url.values()
    ]
    if not args.include_drafts:
        summaries = [summary for summary in summaries if not summary.is_draft]

    summaries.sort(key=status_sort_key)
    return login, summaries


def status_sort_key(summary: PullRequestSummary) -> tuple[int, str]:
    priority = {
        "requested": 0,
        "reviewed+requested": 1,
        "reviewed": 2,
        "seen": 3,
    }.get(summary.self_status, 9)
    return (priority, reverse_timestamp(summary.updated_at))


def reverse_timestamp(value: str) -> str:
    # Strings are ISO 8601, so a simple inversion keeps recent rows first while
    # retaining a stable tuple sort key.
    return "".join(chr(255 - ord(ch)) for ch in value)


def summarize_pull_request(
    node: dict[str, Any], login: str, sources: Iterable[str]
) -> PullRequestSummary:
    requested_reviewers = tuple(filter(None, map(format_requested_reviewer, review_request_nodes(node))))
    reviews = parse_reviews(node)
    latest_by_author = latest_reviews_by_author(reviews)
    my_latest = latest_by_author.get(login)
    other_latest = tuple(
        review
        for author, review in sorted(latest_by_author.items())
        if author != login
    )
    requested_for_user = "requested" in sources or f"@{login}" in requested_reviewers

    return PullRequestSummary(
        repository=node.get("repository", {}).get("nameWithOwner", ""),
        number=int(node.get("number", 0)),
        title=node.get("title", ""),
        url=node.get("url", ""),
        author=node.get("author", {}).get("login", ""),
        state=node.get("state", ""),
        is_draft=bool(node.get("isDraft")),
        review_decision=node.get("reviewDecision"),
        created_at=node.get("createdAt", ""),
        updated_at=node.get("updatedAt", ""),
        requested_reviewers=requested_reviewers,
        requested_for_user=requested_for_user,
        sources=tuple(sorted(sources)),
        reviews=tuple(reviews),
        my_latest_review=my_latest,
        other_latest_reviews=other_latest,
    )


def review_request_nodes(node: dict[str, Any]) -> list[dict[str, Any]]:
    return node.get("reviewRequests", {}).get("nodes", []) or []


def format_requested_reviewer(node: dict[str, Any]) -> str | None:
    reviewer = node.get("requestedReviewer") or {}
    typename = reviewer.get("__typename")
    if typename == "User" and reviewer.get("login"):
        return f"@{reviewer['login']}"
    if typename == "Team" and reviewer.get("slug"):
        org = reviewer.get("organization", {}).get("login")
        if org:
            return f"@{org}/{reviewer['slug']}"
        return f"@{reviewer['slug']}"
    return None


def parse_reviews(node: dict[str, Any]) -> list[ReviewSummary]:
    reviews: list[ReviewSummary] = []
    for review in node.get("reviews", {}).get("nodes", []) or []:
        author = review.get("author") or {}
        login = author.get("login")
        submitted_at = review.get("submittedAt")
        state = review.get("state")
        if not login or not submitted_at or not state:
            continue
        reviews.append(
            ReviewSummary(
                author=login,
                state=state,
                submitted_at=submitted_at,
                url=review.get("url", ""),
            )
        )
    return reviews


def latest_reviews_by_author(reviews: Iterable[ReviewSummary]) -> dict[str, ReviewSummary]:
    latest: dict[str, ReviewSummary] = {}
    for review in reviews:
        current = latest.get(review.author)
        if current is None or parse_datetime(review.submitted_at) > parse_datetime(current.submitted_at):
            latest[review.author] = review
    return latest


def parse_datetime(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def collect_stats(args: argparse.Namespace, client: GhClient) -> tuple[str, dict[str, Any]]:
    login, _search_user = normalize_login(args.user, client)
    since, until = stats_window(args)
    common = [
        "is:pr",
        "archived:false",
        f"reviewed-by:{login}",
        f"updated:>={since.date().isoformat()}",
        *state_qualifier(args.state),
        *scope_qualifiers(args.repo, args.owner),
    ]
    nodes = search_pull_requests(client, " ".join(common), args.limit)
    summaries = [summarize_pull_request(node, login, {"reviewed"}) for node in nodes]
    stats = calculate_stats(login, summaries, since, until)
    stats["period"] = {
        "since": since.isoformat(),
        "until": until.isoformat(),
    }
    stats["candidatePullRequests"] = len(summaries)
    return login, stats


def stats_window(args: argparse.Namespace) -> tuple[datetime, datetime]:
    until = parse_date(args.until, end_of_day=True) if args.until else datetime.now(timezone.utc)
    if args.since:
        since = parse_date(args.since)
    else:
        since = until - timedelta(days=args.days)
    return since, until


def parse_date(value: str, end_of_day: bool = False) -> datetime:
    parsed = datetime.fromisoformat(value)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    else:
        parsed = parsed.astimezone(timezone.utc)
    if end_of_day and len(value) == 10:
        parsed = parsed + timedelta(days=1) - timedelta(microseconds=1)
    return parsed


def calculate_stats(
    login: str,
    summaries: Iterable[PullRequestSummary],
    since: datetime,
    until: datetime,
) -> dict[str, Any]:
    own_reviews: list[ReviewSummary] = []
    all_reviews_on_touched_prs: list[ReviewSummary] = []
    unique_prs: set[str] = set()
    prs_with_other_reviewers: set[str] = set()
    state_counts = {state: 0 for state in REVIEW_STATES}

    for summary in summaries:
        pr_key = f"{summary.repository}#{summary.number}"
        own_in_window = [
            review
            for review in summary.reviews
            if review.author == login and since <= parse_datetime(review.submitted_at) <= until
        ]
        if not own_in_window:
            continue

        unique_prs.add(pr_key)
        own_reviews.extend(own_in_window)
        for review in own_in_window:
            if review.state in state_counts:
                state_counts[review.state] += 1

        reviews_in_window = [
            review
            for review in summary.reviews
            if since <= parse_datetime(review.submitted_at) <= until
        ]
        all_reviews_on_touched_prs.extend(reviews_in_window)
        if any(review.author != login for review in reviews_in_window):
            prs_with_other_reviewers.add(pr_key)

    total_peer_reviews = len(all_reviews_on_touched_prs)
    own_review_count = len(own_reviews)
    share = own_review_count / total_peer_reviews if total_peer_reviews else 0

    return {
        "ownReviewSubmissions": own_review_count,
        "uniquePullRequestsReviewed": len(unique_prs),
        "reviewSubmissionsOnTouchedPullRequests": total_peer_reviews,
        "ownReviewShareOnTouchedPullRequests": round(share, 4),
        "pullRequestsWithOtherReviewers": len(prs_with_other_reviewers),
        "stateCounts": state_counts,
    }
