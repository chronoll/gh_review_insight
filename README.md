# gh-review-insight

GitHub のレビューリクエストと自分のレビュー実績を確認するための小さなCLIです。

既存の `gh` CLI を認証とAPIアクセスに使うため、このツール自体は追加依存を持ちません。

## 位置づけ

調査した限り、近い既存手段はあります。

- `gh pr status`: 現在のリポジトリ内で自分にレビュー依頼されているPRを確認できる。
- `gh search prs --review-requested=@me --state=open`: GitHub全体を検索してレビュー依頼PRを確認できる。
- `gh-dash`: 設定可能なGitHub TUIで、PRやIssueのセクションを作れる。

一方で、「自分がすでにレビューしたか」「他の人の最新レビュー状態」「自分のレビュー数や割合」を同じ粒度で見る用途は薄いので、このCLIではその部分を補います。

## Requirements

- Python 3.10+
- GitHub CLI (`gh`)
- `gh auth login` 済みであること

## Usage

```bash
cd /Users/michika.kurotaka/Private/sandbox/gh-review-insight
python3 gh_review_insight.py status
```

実行ファイルとして使う場合:

```bash
chmod +x gh-review-insight
./gh-review-insight status
```

特定リポジトリやOrgに絞る:

```bash
./gh-review-insight --repo owner/repo status
./gh-review-insight --owner my-org status
```

JSON / CSV:

```bash
./gh-review-insight status --json
./gh-review-insight stats --days 90 --csv
```

レビュー実績:

```bash
./gh-review-insight stats --days 30
./gh-review-insight --owner my-org stats --since 2026-01-01 --until 2026-06-14
```

## Commands

### `status`

次のPRをまとめて表示します。

- `review-requested:@me` に一致するオープンPR
- `reviewed-by:<me>` に一致するオープンPR

列の意味:

- `status`: `requested`, `reviewed`, `reviewed+requested`
- `mine`: 自分の最新レビュー状態
- `others`: 他のレビュワーごとの最新レビュー状態
- `requested`: 現在残っている requested reviewers / teams

### `stats`

指定期間に自分がレビューしたPRを集計します。

- `reviews`: 自分のレビュー提出数
- `prs`: 自分がレビューしたユニークPR数
- `allReviews`: 自分が触ったPR上の同期間レビュー提出数
- `share`: `reviews / allReviews`
- `withOthers`: 同期間に他のレビュワーもレビューしていたPR数

## Limitations

- GitHub GraphQL のPRごとの `reviews(last: 100)` を使うため、1 PRに100件を超えるレビューがある場合は古いレビューを取りこぼす可能性があります。
- `review-requested` はGitHubの仕様上、レビュー提出後は検索結果から外れます。そのため `status` は `reviewed-by:<me>` も併用して、レビュー済みのオープンPRを補足します。
- チームレビュー依頼は、GitHub検索が自分の所属チーム分を返す場合に `requested` として扱います。
