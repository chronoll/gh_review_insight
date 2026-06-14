"""Entry-point dispatch for gh-review-insight.

Routing rules:
- A known subcommand (``status`` / ``stats``) or a help flag → CLI. This is the
  stable, machine-friendly surface for scripts and AI agents.
- No subcommand → TUI, but only on an interactive terminal. Piped or otherwise
  non-interactive invocations never launch the full-screen app; they get a clear
  error pointing at the subcommands instead.

The TUI import is deferred into ``_launch_tui`` so the CLI path keeps working
with zero third-party dependencies even when ``textual`` is not installed.
"""

from __future__ import annotations

import sys

from .cli import COMMANDS
from .cli import main as cli_main
from .core import GhError


def _wants_cli(argv: list[str]) -> bool:
    """Return True when argv selects the CLI: a known subcommand or help flag."""
    if any(arg in ("-h", "--help") for arg in argv):
        return True
    return any(arg in COMMANDS for arg in argv)


def _launch_tui(argv: list[str]) -> int:
    try:
        from .tui import run_tui
    except ModuleNotFoundError as exc:
        missing = exc.name or ""
        if missing == "textual" or missing.startswith("textual.") or missing.endswith(".tui"):
            raise GhError(
                "TUI を利用できません。`pip install 'gh-review-insight[tui]'` で textual を導入するか、"
                "`status` / `stats` サブコマンドを使ってください。"
            ) from exc
        raise
    return run_tui(argv)


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)

    if _wants_cli(argv):
        return cli_main(argv)

    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        print(
            "error: TUI には対話端末が必要です。`status` などのサブコマンドを使ってください。",
            file=sys.stderr,
        )
        return 2

    try:
        return _launch_tui(argv)
    except GhError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
