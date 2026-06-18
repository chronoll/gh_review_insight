# gh-review-insight

GitHub のレビュー依頼状況と自分のレビュー実績を確認するための小さな GUI です（Rust + egui）。

認証と API アクセスは既存の `gh` CLI に委譲するため、トークンや HTTP 通信を自前で持ちません。

## できること

- **status**: オープン PR を一覧表示。PR の id・タイトルをクリックすると GitHub ページがブラウザで開きます。
  - 各 PR を3つの状態に分類し、**セルを薄く色分け**します:
    - `waiting`（赤系）= 自分の対応が必要（誰も未レビュー、または一度レビュー後に再リクエストされた）
    - `should`（橙系）= リクエストがあり自分は未レビュー、誰かがレビュー済み
    - `finished`（緑系）= 自分がレビュー済みで、再リクエストはされていない
  - 作成者（author）列で「誰の PR か」がわかります。
- **stats**: 直近 N 日のレビュー実績（提出数・ユニーク PR 数・割合・状態内訳）を集計表示。
- **ユーザー別の文字色**: 設定画面でユーザーごとに色を割り当てると、author や reviewer 名がその色で表示されます（設定は保存されます）。

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

- ツールバーの `status` / `stats` でビュー切替、`⟳ 更新` で再取得、`⚙ 設定` で色設定を開閉。
- status: フィルタ（title / repo / author）で絞り込み。PR 行の id・タイトルがリンクになっており、クリックで GitHub へ遷移。ホバーするとレビュー履歴などの詳細が出ます。セルの背景色は状態（`waiting` / `should` / `finished`）を表します。`others` / `requested` 列は幅を制限し、全文はホバーで表示します。
- stats: `days` を変更して `更新` を押すと集計期間が変わります。
- 設定: `⚙ 設定` で (1) ユーザーごとの文字色（color picker）、(2) 一覧から除外する GitHub リンク（PR / リポジトリ URL）、(3) レビュー集計で無視するアカウント（bot 等。`finished` / `should` 判定や `others` に含めない）を設定できます。設定は `$HOME/.config/gh-review-insight/`（`colors.json` / `excludes.json` / `ignored.json`）に保存され、次回起動時に読み込まれます。

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
