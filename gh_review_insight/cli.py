"""CLI frontend for gh-review-insight.

Owns argument parsing and the table/JSON/CSV rendering. This is the stable,
machine-friendly surface intended for scripts and AI agents. It depends only on
the standard library and `core`; no TUI dependency leaks in here.
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from io import StringIO
from typing import Any

from .core import (
    GhClient,
    GhError,
    PullRequestSummary,
    collect_stats,
    collect_status,
)


# Subcommands exposed by the CLI. Used both here and by the entry-point dispatch
# in __main__ to decide between CLI and TUI mode.
COMMANDS = ("status", "stats")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="gh-review-insight",
        description="GitHub review requests and review activity from the terminal.",
    )
    parser.add_argument(
        "--user",
        default="@me",
        help="GitHub login to inspect. Defaults to @me.",
    )
    parser.add_argument(
        "--repo",
        action="append",
        default=[],
        metavar="OWNER/REPO",
        help="Limit search to a repository. Repeatable.",
    )
    parser.add_argument(
        "--owner",
        action="append",
        default=[],
        metavar="OWNER",
        help="Limit search to an owner or org. Repeatable.",
    )
    parser.add_argument(
        "--gh",
        default="gh",
        help="Path to gh executable. Defaults to gh.",
    )

    subcommands = parser.add_subparsers(dest="command", required=True)

    status = subcommands.add_parser(
        "status",
        help="Show open PRs requesting your review and open PRs you reviewed.",
    )
    status.add_argument("--limit", type=int, default=50, help="Maximum PRs per search bucket.")
    status.add_argument("--include-drafts", action="store_true", help="Include draft pull requests.")
    status.add_argument(
        "--no-reviewed",
        action="store_true",
        help="Only show currently requested reviews, not open PRs you already reviewed.",
    )
    add_output_flags(status)

    stats = subcommands.add_parser(
        "stats",
        help="Summarize review activity for a period.",
    )
    stats.add_argument("--limit", type=int, default=200, help="Maximum candidate PRs to inspect.")
    stats.add_argument("--days", type=int, default=30, help="Look back this many days. Defaults to 30.")
    stats.add_argument("--since", help="Start date, YYYY-MM-DD. Overrides --days.")
    stats.add_argument("--until", help="End date, YYYY-MM-DD. Defaults to now.")
    stats.add_argument(
        "--state",
        choices=("all", "open", "closed", "merged"),
        default="all",
        help="PR state filter for candidates.",
    )
    add_output_flags(stats)

    return parser


def add_output_flags(parser: argparse.ArgumentParser) -> None:
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--json", action="store_true", help="Emit JSON.")
    group.add_argument("--csv", action="store_true", help="Emit CSV.")


def truncate(value: str, width: int) -> str:
    if len(value) <= width:
        return value
    if width <= 1:
        return value[:width]
    return value[: width - 1] + "…"


def render_status(login: str, summaries: list[PullRequestSummary], args: argparse.Namespace) -> str:
    rows = [status_row(summary) for summary in summaries]
    if args.json:
        return json.dumps({"user": login, "pullRequests": rows}, ensure_ascii=False, indent=2)
    if args.csv:
        return to_csv(rows)
    if not rows:
        return f"{login} 宛ての対象PRはありません。"
    columns = ["status", "pr", "mine", "others", "requested", "updated", "title"]
    return format_table(rows, columns)


def status_row(summary: PullRequestSummary) -> dict[str, Any]:
    other_reviews = ", ".join(
        f"{review.author}:{short_state(review.state)}" for review in summary.other_latest_reviews
    )
    return {
        "status": summary.self_status,
        "pr": f"{summary.repository}#{summary.number}",
        "mine": short_state(summary.my_latest_review.state) if summary.my_latest_review else "-",
        "others": other_reviews or "-",
        "requested": ", ".join(summary.requested_reviewers) or "-",
        "updated": summary.updated_at[:10],
        "title": summary.title,
        "url": summary.url,
        "reviewDecision": summary.review_decision or "",
        "draft": summary.is_draft,
    }


def short_state(state: str) -> str:
    return {
        "APPROVED": "approved",
        "CHANGES_REQUESTED": "changes",
        "COMMENTED": "comment",
        "DISMISSED": "dismissed",
        "PENDING": "pending",
    }.get(state, state.lower())


def format_table(rows: list[dict[str, Any]], columns: list[str]) -> str:
    display_rows = [
        {key: truncate(str(row.get(key, "")), width_for_column(key)) for key in columns}
        for row in rows
    ]
    widths = {
        column: max(len(column), *(len(row[column]) for row in display_rows))
        for column in columns
    }
    lines = [
        "  ".join(column.ljust(widths[column]) for column in columns),
        "  ".join("-" * widths[column] for column in columns),
    ]
    for row in display_rows:
        lines.append("  ".join(row[column].ljust(widths[column]) for column in columns))
    return "\n".join(lines)


def width_for_column(column: str) -> int:
    return {
        "status": 18,
        "pr": 32,
        "mine": 12,
        "others": 32,
        "requested": 32,
        "updated": 10,
        "title": 72,
    }.get(column, 40)


def to_csv(rows: list[dict[str, Any]]) -> str:
    if not rows:
        return ""
    output = StringIO()
    writer = csv.DictWriter(output, fieldnames=list(rows[0].keys()))
    writer.writeheader()
    writer.writerows(rows)
    return output.getvalue().rstrip("\n")


def render_stats(login: str, stats: dict[str, Any], args: argparse.Namespace) -> str:
    if args.json:
        return json.dumps({"user": login, **stats}, ensure_ascii=False, indent=2)
    row = {
        "user": login,
        "since": stats["period"]["since"][:10],
        "until": stats["period"]["until"][:10],
        "reviews": stats["ownReviewSubmissions"],
        "prs": stats["uniquePullRequestsReviewed"],
        "allReviews": stats["reviewSubmissionsOnTouchedPullRequests"],
        "share": f"{stats['ownReviewShareOnTouchedPullRequests'] * 100:.1f}%",
        "withOthers": stats["pullRequestsWithOtherReviewers"],
        "approved": stats["stateCounts"]["APPROVED"],
        "changes": stats["stateCounts"]["CHANGES_REQUESTED"],
        "commented": stats["stateCounts"]["COMMENTED"],
    }
    if args.csv:
        return to_csv([row])
    return format_table([row], list(row.keys()))


def run(args: argparse.Namespace) -> str:
    client = GhClient(args.gh)
    if args.command == "status":
        login, summaries = collect_status(args, client)
        return render_status(login, summaries, args)
    if args.command == "stats":
        login, stats = collect_stats(args, client)
        return render_stats(login, stats, args)
    raise GhError(f"Unknown command: {args.command}")


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        print(run(args))
    except GhError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
