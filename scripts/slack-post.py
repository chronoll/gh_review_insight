#!/usr/bin/env python3
"""Post one message to Slack as the bot and print its ts.

The message text is read from stdin (Slack mrkdwn: `*bold*`, `<url|text>`,
`<@U…>` mentions). The bot token comes from `SLACK_BOT_TOKEN`; the bot must
be a member of the channel.

Usage:
    scripts/slack-post.py --channel C0123 [--thread-ts 1700000000.000100] < message.txt
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.request


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--channel", required=True)
    ap.add_argument("--thread-ts", help="reply in this message's thread")
    args = ap.parse_args()

    token = os.environ.get("SLACK_BOT_TOKEN")
    if not token:
        print("SLACK_BOT_TOKEN is not set", file=sys.stderr)
        return 2
    text = sys.stdin.read().strip()
    if not text:
        print("empty message", file=sys.stderr)
        return 2

    body = {"channel": args.channel, "text": text, "unfurl_links": False}
    if args.thread_ts:
        body["thread_ts"] = args.thread_ts
    req = urllib.request.Request(
        "https://slack.com/api/chat.postMessage",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json; charset=utf-8", "Authorization": f"Bearer {token}"},
    )
    with urllib.request.urlopen(req, timeout=30) as res:
        out = json.load(res)
    if not out.get("ok"):
        print(f"chat.postMessage failed: {out.get('error')}", file=sys.stderr)
        return 1
    print(out["ts"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
