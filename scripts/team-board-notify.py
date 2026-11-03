#!/usr/bin/env python3
"""Find priority / memo changes on the team review board since the last run.

Reads the board's `notes` documents (one JSON file per PR, as ArtifactData
`list ... out_dir` writes them: `<owner>@<repo>@<number>.json`), the
`board/snapshot` document (PR titles and URLs) and the previous
`board/notify` document, then prints one JSON object:

    {"date": "2026-11-02", "parent": "...", "reply": "..." | null,
     "notify": {"seen": {...}, "threads": {...}}}

`reply` is null when nothing changed. `notify` is the next `board/notify`
document; the caller adds the day's thread ts to `threads` after posting.

With `--post CHANNEL` the script posts itself through the Slack Web API
(`chat.postMessage`, bot token in `SLACK_BOT_TOKEN`): the day's parent
message the first time, then the changes as a reply in its thread. The
printed `notify.threads` then already holds the day's ts.

Usage:
    scripts/team-board-notify.py --notes-dir DIR --snapshot FILE \
        [--state FILE] --board-url URL [--post CHANNEL]
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

JST = timezone(timedelta(hours=9))
PRIO = {"high": "高", "mid": "中", "low": "低"}
KEEP_THREAD_DAYS = 7


def load_json(path: str | None) -> dict:
    if not path or not Path(path).exists():
        return {}
    data = json.loads(Path(path).read_text())
    # Tolerate a {"data": {...}} wrapper as well as the bare document.
    return data.get("data", data) if isinstance(data, dict) else {}


def load_notes(notes_dir: str) -> dict[str, dict]:
    notes = {}
    for f in sorted(Path(notes_dir).glob("*.json")):
        doc = load_json(str(f))
        notes[f.stem.replace("@", "~")] = {
            "priority": doc.get("priority") or None,
            "note": (doc.get("note") or "").strip(),
        }
    return notes


def slack_post(channel: str, text: str, thread_ts: str | None = None) -> str:
    body = {"channel": channel, "text": text, "unfurl_links": False}
    if thread_ts:
        body["thread_ts"] = thread_ts
    req = urllib.request.Request(
        "https://slack.com/api/chat.postMessage",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json; charset=utf-8",
            "Authorization": f"Bearer {os.environ['SLACK_BOT_TOKEN']}",
        },
    )
    with urllib.request.urlopen(req, timeout=30) as res:
        out = json.load(res)
    if not out.get("ok"):
        raise SystemExit(f"Slack chat.postMessage failed: {out.get('error')}")
    return out["ts"]


def link(url: str, text: str, mrkdwn: bool) -> str:
    return f"<{url}|{text}>" if mrkdwn else f"[{text}]({url})"


def bold(text: str, mrkdwn: bool) -> str:
    return f"*{text}*" if mrkdwn else f"**{text}**"


def slack_text(s: str, limit: int = 120) -> str:
    s = " ".join(s.split())
    return s if len(s) <= limit else s[: limit - 1] + "…"


def describe(key: str, old: dict | None, new: dict, prs: dict, names: dict, mrkdwn: bool) -> str | None:
    old = old or {"priority": None, "note": ""}
    lines = []
    if old["priority"] != new["priority"]:
        before = PRIO.get(old["priority"], "未設定")
        after = PRIO.get(new["priority"], "未設定")
        lines.append(f"  優先度：{before} → {bold(after, mrkdwn)}")
    if old["note"] != new["note"]:
        if not new["note"]:
            lines.append(f"  メモ：削除（旧「{slack_text(old['note'])}」）")
        elif not old["note"]:
            lines.append(f"  メモ：「{slack_text(new['note'])}」")
        else:
            lines.append(f"  メモ：「{slack_text(old['note'])}」→「{slack_text(new['note'])}」")
    if not lines:
        return None
    pr = prs[key]
    repo = pr["repo"].split("/", 1)[1]
    who = names.get(pr["author"], pr["author"])
    title = slack_text(pr["title"], 80)
    if mrkdwn:  # Slack mrkdwn treats these as control characters
        title = title.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    ref = f"{repo}#{pr['number']}"
    head = f"• {link(pr['url'], ref, mrkdwn)} {title}（{who}）"
    return "\n".join([head, *lines])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--notes-dir", required=True)
    ap.add_argument("--snapshot", required=True)
    ap.add_argument("--state", help="previous board/notify document (may be missing)")
    ap.add_argument("--config", help="board/config document, for member names")
    ap.add_argument("--board-url", required=True)
    ap.add_argument("--post", metavar="CHANNEL", help="post to this Slack channel with SLACK_BOT_TOKEN")
    args = ap.parse_args()
    mrkdwn = bool(args.post)

    now = datetime.now(JST)
    today = now.strftime("%Y-%m-%d")
    notes = load_notes(args.notes_dir)
    snapshot = load_json(args.snapshot)
    prs = {p["key"]: p for p in snapshot.get("prs", [])}
    config = load_json(args.config)
    names = {m["login"]: m.get("name") or m["login"] for m in config.get("members", [])}
    # Slack user ids from board/config to mention in the day's parent message.
    mentions = [u for u in config.get("notifyMentions", []) if isinstance(u, str) and u.isalnum()]
    state = load_json(args.state)
    seen = state.get("seen", {})

    changes = []
    for key, new in notes.items():
        if key not in prs:  # closed / merged PRs: remember, never announce
            continue
        if key not in seen and not new["priority"] and not new["note"]:
            continue
        line = describe(key, seen.get(key), new, prs, names, mrkdwn)
        if line:
            changes.append(line)

    cutoff = (now - timedelta(days=KEEP_THREAD_DAYS)).strftime("%Y-%m-%d")
    threads = {d: ts for d, ts in state.get("threads", {}).items() if d >= cutoff}

    mention = " ".join(f"<@{u}>" for u in mentions) + " " if mentions and mrkdwn else ""
    parent = (
        mention + bold(f"チームレビューボード：優先度・メモの更新（{now.month}/{now.day}）", mrkdwn) + "\n"
        f"今日の変更はこのスレッドに流れます。{link(args.board_url, 'ボードを開く', mrkdwn)}"
    )
    reply = (bold(f"{now:%H:%M} 時点の変更（{len(changes)}件）", mrkdwn) + "\n" + "\n".join(changes)) if changes else None

    if args.post and reply:
        ts = threads.get(today) or slack_post(args.post, parent)
        threads[today] = ts
        slack_post(args.post, reply, thread_ts=ts)

    out = {
        "date": today,
        "parent": parent,
        "reply": reply,
        "posted": bool(args.post and reply),
        "notify": {"seen": notes, "threads": threads},
    }
    json.dump(out, sys.stdout, ensure_ascii=False)
    return 0


if __name__ == "__main__":
    sys.exit(main())
