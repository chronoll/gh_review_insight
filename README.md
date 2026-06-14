# gh-review-insight

GitHub のレビュー依頼状況と自分のレビュー実績を確認するための小さな GUI です（Rust + egui）。

認証と API アクセスは既存の `gh` CLI に委譲するため、トークンや HTTP 通信を自前で持ちません。

## できること

- **status**: レビュー依頼中 / レビュー済みのオープン PR を一覧表示。PR の id・タイトルをクリックすると GitHub ページがブラウザで開きます。
- **stats**: 直近 N 日のレビュー実績（提出数・ユニーク PR 数・割合・状態内訳）を集計表示。

## Requirements

- Rust 1.87+（ビルド用）
- GitHub CLI (`gh`)
- `gh auth login` 済みであること

## 使い方

```bash
cd gh-review-insight
cargo run            # GUI を起動
```

リリースビルド / インストール:

```bash
cargo build --release      # target/release/gh-review-insight
cargo install --path .     # PATH に gh-review-insight を入れる
```

## 画面と操作

- ツールバーの `status` / `stats` でビュー切替、`⟳ 更新` で再取得。
- status: フィルタ（title / repo / author）で絞り込み。PR 行の id・タイトルがリンクになっており、クリックで GitHub へ遷移。ホバーするとレビュー履歴などの詳細が出ます。
- stats: `days` を変更して `更新` を押すと集計期間が変わります。

## 構成

- `src/gh.rs` — `gh api graphql` を実行する薄いラッパー（認証は gh に委譲）
- `src/core.rs` — 検索・集計（`collect_status` / `collect_stats` / `summarize_pull_request` / `calculate_stats`）
- `src/model.rs` — データモデル（`PullRequestSummary` / `ReviewSummary` / `Stats`）
- `src/app.rs` — egui GUI
- `src/main.rs` — エントリポイント

## テスト

```bash
cargo test
```

## Limitations

- 1 PR あたり GraphQL の `reviews(last: 100)` を使うため、100件を超えるレビューがある PR では古いレビューを取りこぼす可能性があります。
- `review-requested` はGitHubの仕様上レビュー提出後に検索結果から外れるため、status では `reviewed-by` も併用してレビュー済みのオープン PR を補足します。
