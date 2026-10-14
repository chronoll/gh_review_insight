#!/usr/bin/env python3
"""Collect a team's open PRs for the shared review board (Claude artifact).

Lists each repository's open PRs with `gh api` (REST, repository-scoped),
keeps the members' ones, and prints one JSON object that the board stores
as its `board/snapshot` document:

    {"syncedAt", "members", "repos", "prs": [...]}

Each PR carries what the board needs to classify review status per viewer:
requested reviewers, and each reviewer's latest review state.

Usage:
    scripts/team-board-sync.py --repo everytv/delish-server \
        --member chronoll --member m-book > snapshot.json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone

DECISIVE = {"APPROVED", "CHANGES_REQUESTED"}


def gh(path: str, *params: str) -> object:
    cmd = ["gh", "api", "-X", "GET", path]
    for p in params:
        cmd += ["-f", p]
    out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def open_prs_by(repo: str, members: set[str]) -> list[dict]:
    # Repository-scoped listing: the search API is unavailable where sessions
    # are bound to specific repositories.
    found: list[dict] = []
    page = 1
    while True:
        batch = gh(f"repos/{repo}/pulls", "state=open", "per_page=100", f"page={page}")
        found += [p for p in batch if p["user"]["login"] in members]
        if len(batch) < 100:
            return found
        page += 1


def is_ignored(login: str, ignored: set[str]) -> bool:
    return login.endswith("[bot]") or login in ignored


def summarize_reviews(reviews: list[dict], author: str, ignored: set[str]) -> dict:
    """Latest state per reviewer; a later COMMENTED does not hide an earlier
    APPROVED / CHANGES_REQUESTED, matching how GitHub shows review status."""
    out: dict[str, dict] = {}
    for r in sorted(reviews, key=lambda r: r.get("submitted_at") or ""):
        login = (r.get("user") or {}).get("login") or ""
        state = r.get("state") or ""
        at = r.get("submitted_at") or ""
        if not login or not at or login == author or is_ignored(login, ignored):
            continue
        if state == "PENDING":
            continue
        prev = out.get(login)
        if prev and state == "COMMENTED" and prev["state"] in DECISIVE:
            state = prev["state"]
        out[login] = {"state": state, "at": at}
    return out


def collect(repo: str, number: int, ignored: set[str]) -> dict:
    pr = gh(f"repos/{repo}/pulls/{number}")
    reviews = gh(f"repos/{repo}/pulls/{number}/reviews", "per_page=100")
    author = pr["user"]["login"]
    return {
        "key": f"{repo.replace('/', '~')}~{number}",
        "repo": repo,
        "number": number,
        "title": pr["title"],
        "url": pr["html_url"],
        "author": author,
        "draft": bool(pr.get("draft")),
        "labels": [{"name": l["name"], "color": l.get("color") or ""} for l in pr.get("labels", [])],
        "createdAt": pr["created_at"],
        "updatedAt": pr["updated_at"],
        "additions": pr.get("additions", 0),
        "deletions": pr.get("deletions", 0),
        "changedFiles": pr.get("changed_files", 0),
        "requested": [
            u["login"] for u in pr.get("requested_reviewers", []) if not is_ignored(u["login"], ignored)
        ],
        "requestedTeams": [t["slug"] for t in pr.get("requested_teams", [])],
        "reviews": summarize_reviews(reviews, author, ignored),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", action="append", required=True, help="owner/name (repeatable)")
    ap.add_argument("--member", action="append", required=True, help="GitHub login (repeatable)")
    ap.add_argument("--ignore", action="append", default=[], help="reviewer login to ignore")
    args = ap.parse_args()

    ignored = set(args.ignore)
    members = set(args.member)
    targets = [(repo, p["number"]) for repo in args.repo for p in open_prs_by(repo, members)]
    with ThreadPoolExecutor(max_workers=6) as pool:
        prs = list(pool.map(lambda t: collect(t[0], t[1], ignored), targets))
    prs.sort(key=lambda p: p["updatedAt"], reverse=True)

    json.dump(
        {
            "syncedAt": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "members": args.member,
            "repos": args.repo,
            "prs": prs,
        },
        sys.stdout,
        ensure_ascii=False,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
