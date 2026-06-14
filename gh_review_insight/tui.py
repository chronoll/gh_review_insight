"""Interactive TUI frontend for gh-review-insight.

Built on `textual`, which is an *optional* dependency: install it with
``pip install 'gh-review-insight[tui]'``. The CLI and core layers never import
this module, so they keep working with zero third-party dependencies.

The TUI reuses the exact same `core` fetching/aggregation and `cli` rendering
helpers as the command line; it adds no new network calls of its own, it only
makes the existing data browsable (move / detail / open / refresh / switch /
filter).
"""

from __future__ import annotations

import argparse
import webbrowser
from types import SimpleNamespace

from textual import work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal
from textual.widgets import DataTable, Footer, Header, Input, Static

from . import cli
from .core import GhClient, GhError, PullRequestSummary, collect_stats, collect_status


STATUS_COLUMNS = ("status", "pr", "mine", "others", "requested", "updated", "title")


def parse_global_args(argv: list[str]) -> argparse.Namespace:
    """Parse the global options shared with the CLI (everything but a subcommand)."""
    parser = argparse.ArgumentParser(prog="gh-review-insight")
    parser.add_argument("--user", default="@me")
    parser.add_argument("--repo", action="append", default=[])
    parser.add_argument("--owner", action="append", default=[])
    parser.add_argument("--gh", default="gh")
    return parser.parse_args(argv)


def _status_namespace(opts: argparse.Namespace) -> SimpleNamespace:
    return SimpleNamespace(
        user=opts.user,
        repo=opts.repo,
        owner=opts.owner,
        gh=opts.gh,
        command="status",
        limit=50,
        include_drafts=False,
        no_reviewed=False,
        json=False,
        csv=False,
    )


def _stats_namespace(opts: argparse.Namespace) -> SimpleNamespace:
    return SimpleNamespace(
        user=opts.user,
        repo=opts.repo,
        owner=opts.owner,
        gh=opts.gh,
        command="stats",
        limit=200,
        days=30,
        since=None,
        until=None,
        state="all",
        json=False,
        csv=False,
    )


def _detail_text(summary: PullRequestSummary) -> str:
    lines = [
        f"{summary.repository}#{summary.number}",
        summary.title or "(no title)",
        summary.url,
        "",
        f"author:  {summary.author}",
        f"state:   {summary.state}{' (draft)' if summary.is_draft else ''}",
    ]
    if summary.review_decision:
        lines.append(f"decision: {summary.review_decision}")
    if summary.requested_reviewers:
        lines.append("requested: " + ", ".join(summary.requested_reviewers))
    lines.append("")
    lines.append("reviews:")
    timeline = sorted(summary.reviews, key=lambda review: review.submitted_at)
    if not timeline:
        lines.append("  (none)")
    for review in timeline:
        lines.append(
            f"  {review.submitted_at[:10]}  {review.author:<18} {cli.short_state(review.state)}"
        )
    return "\n".join(lines)


class ReviewInsightApp(App):
    """Browse review requests (status) and review activity (stats)."""

    CSS = """
    #body { height: 1fr; }
    #table { width: 2fr; }
    #detail { width: 1fr; border: round $panel; padding: 0 1; }
    #statsview { padding: 1 2; }
    #statusbar { height: 1; background: $boost; color: $text; padding: 0 1; }
    #filter { display: none; }
    #filter.visible { display: block; }
    .hidden { display: none; }
    """

    BINDINGS = [
        Binding("q", "quit", "Quit"),
        Binding("r", "refresh", "Refresh"),
        Binding("s", "switch", "status/stats"),
        Binding("o", "open", "Open in browser"),
        Binding("slash", "filter", "Filter"),
        Binding("escape", "clear_filter", "Clear filter", show=False),
    ]

    def __init__(self, opts: argparse.Namespace) -> None:
        super().__init__()
        self._opts = opts
        self._client = GhClient(opts.gh)
        self.mode = "status"
        self._login = ""
        self._summaries: list[PullRequestSummary] = []
        self._by_key: dict[str, PullRequestSummary] = {}
        self._filter = ""

    def compose(self) -> ComposeResult:
        yield Header(show_clock=False)
        yield Static("", id="statusbar")
        with Horizontal(id="body"):
            table = DataTable(id="table")
            table.cursor_type = "row"
            yield table
            yield Static("", id="detail")
        yield Static("", id="statsview", classes="hidden")
        yield Input(placeholder="filter (title / repo / author)…", id="filter")
        yield Footer()

    def on_mount(self) -> None:
        table = self.query_one("#table", DataTable)
        table.add_columns(*STATUS_COLUMNS)
        # Keep keyboard focus on the table; the hidden filter Input must not
        # steal initial focus, or bindings like "s"/"/" would type into it.
        table.focus()
        self._load()

    # --- data loading -----------------------------------------------------

    @work(thread=True, exclusive=True)
    def _load(self) -> None:
        self.call_from_thread(self._set_status, f"{self.mode}: 読み込み中…")
        try:
            if self.mode == "status":
                login, summaries = collect_status(_status_namespace(self._opts), self._client)
                self.call_from_thread(self._on_status_loaded, login, summaries)
            else:
                login, stats = collect_stats(_stats_namespace(self._opts), self._client)
                self.call_from_thread(self._on_stats_loaded, login, stats)
        except GhError as exc:
            self.call_from_thread(self._set_status, f"error: {exc}")

    def _on_status_loaded(self, login: str, summaries: list[PullRequestSummary]) -> None:
        self._login = login
        self._summaries = summaries
        self._show_status_view()
        self._populate_table()

    def _on_stats_loaded(self, login: str, stats: dict) -> None:
        self._login = login
        namespace = SimpleNamespace(json=False, csv=False)
        rendered = cli.render_stats(login, stats, namespace)
        self.query_one("#statsview", Static).update(rendered)
        self._show_stats_view()
        self._set_status(f"stats: {login}")

    # --- view toggling ----------------------------------------------------

    def _show_status_view(self) -> None:
        self.query_one("#body").remove_class("hidden")
        self.query_one("#statsview", Static).add_class("hidden")
        self.query_one("#table", DataTable).focus()

    def _show_stats_view(self) -> None:
        self.query_one("#body").add_class("hidden")
        self.query_one("#statsview", Static).remove_class("hidden")

    def _populate_table(self) -> None:
        table = self.query_one("#table", DataTable)
        table.clear()
        self._by_key.clear()
        needle = self._filter.lower()
        shown = 0
        for summary in self._summaries:
            if needle and needle not in self._haystack(summary):
                continue
            row = cli.status_row(summary)
            table.add_row(*(row[column] for column in STATUS_COLUMNS), key=summary.url)
            self._by_key[summary.url] = summary
            shown += 1
        suffix = f"  filter='{self._filter}'" if self._filter else ""
        self._set_status(f"status: {self._login}  ({shown}/{len(self._summaries)} PRs){suffix}")
        if shown:
            self._update_detail_for_cursor()
        else:
            self.query_one("#detail", Static).update("対象PRはありません。")

    @staticmethod
    def _haystack(summary: PullRequestSummary) -> str:
        return f"{summary.title} {summary.repository} {summary.author}".lower()

    def _set_status(self, text: str) -> None:
        self.query_one("#statusbar", Static).update(text)

    # --- cursor / detail --------------------------------------------------

    def _current_summary(self) -> PullRequestSummary | None:
        if self.mode != "status":
            return None
        table = self.query_one("#table", DataTable)
        if table.row_count == 0:
            return None
        try:
            row_key = table.coordinate_to_cell_key(table.cursor_coordinate).row_key
        except Exception:
            return None
        return self._by_key.get(row_key.value)

    def _update_detail_for_cursor(self) -> None:
        summary = self._current_summary()
        if summary is not None:
            self.query_one("#detail", Static).update(_detail_text(summary))

    def on_data_table_row_highlighted(self, event: DataTable.RowHighlighted) -> None:
        summary = self._by_key.get(event.row_key.value)
        if summary is not None:
            self.query_one("#detail", Static).update(_detail_text(summary))

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        summary = self._by_key.get(event.row_key.value)
        if summary is not None and summary.url:
            webbrowser.open(summary.url)

    # --- actions ----------------------------------------------------------

    def action_refresh(self) -> None:
        self._load()

    def action_switch(self) -> None:
        self.mode = "stats" if self.mode == "status" else "status"
        self._load()

    def action_open(self) -> None:
        summary = self._current_summary()
        if summary is not None and summary.url:
            webbrowser.open(summary.url)

    def action_filter(self) -> None:
        if self.mode != "status":
            return
        filter_input = self.query_one("#filter", Input)
        filter_input.add_class("visible")
        filter_input.value = self._filter
        filter_input.focus()

    def action_clear_filter(self) -> None:
        filter_input = self.query_one("#filter", Input)
        filter_input.remove_class("visible")
        if self._filter:
            self._filter = ""
            self._populate_table()
        self.query_one("#table", DataTable).focus()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self._filter = event.value.strip()
        event.input.remove_class("visible")
        self._populate_table()
        self.query_one("#table", DataTable).focus()


def run_tui(argv: list[str]) -> int:
    opts = parse_global_args(argv)
    ReviewInsightApp(opts).run()
    return 0
