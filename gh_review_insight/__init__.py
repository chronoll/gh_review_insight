"""gh-review-insight: GitHub review requests and review activity from the terminal.

The package is split into a frontend-agnostic data layer (`core`) and frontends
(`cli`, and later `tui`). Commonly used names are re-exported here so that
`from gh_review_insight import ...` keeps working for tests and the wrapper.
"""

from __future__ import annotations

from .cli import main
from .core import (
    GhClient,
    GhError,
    PullRequestSummary,
    ReviewSummary,
    calculate_stats,
    collect_stats,
    collect_status,
    parse_date,
    parse_datetime,
    summarize_pull_request,
)

__all__ = [
    "GhClient",
    "GhError",
    "PullRequestSummary",
    "ReviewSummary",
    "calculate_stats",
    "collect_stats",
    "collect_status",
    "main",
    "parse_date",
    "parse_datetime",
    "summarize_pull_request",
]
