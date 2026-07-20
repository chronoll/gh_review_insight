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
- **AI レビュー（Claude Code 連携）**: PR をチェックして一括で `claude` にレビューさせ、完了後はレポート表示とターミナルでの会話再開ができます（下記参照）。

## Requirements

- Rust 1.87+（ビルド用）
- GitHub CLI (`gh`)
- `gh auth login` 済みであること
- （AI レビューを使う場合）Claude Code CLI (`claude`) と `gh-review` プラグイン

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

## macOS アプリとして常駐させる

リリースビルドから `.app` を作り、Dock / Spotlight から起動したり、ログイン時に自動起動できます。

```bash
./scripts/bundle-macos.sh            # .app を生成（target/release/macos/ に出力）
./scripts/bundle-macos.sh --install  # /Applications にインストール
./scripts/bundle-macos.sh --login    # インストール + ログイン項目に登録（ログイン時に自動起動）
```

- 生成された `.app` は Dock / Launchpad / Spotlight から起動できます。
- ログイン項目は システム設定 > 一般 > ログイン項目 で確認・解除できます。
- ad-hoc 署名のため、初回起動時に「開発元を確認できません」と出た場合は `.app` を右クリック →「開く」で許可してください。
- GUI 起動（Dock など）では `PATH` が最小限になり `gh` が見つからないことがありますが、`/opt/homebrew/bin` などを自動探索するようにしてあります。
- アイコンを付けたい場合は `assets/icon.icns` を置いてから再ビルドしてください。

## 画面と操作

- ツールバーの `status` / `stats` でビュー切替、`⟳ 更新` で再取得、`⚙ 設定` で色設定を開閉。
- status: フィルタ（title / repo / author）で絞り込み。PR 行の id・タイトルがリンクになっており、クリックで GitHub へ遷移。ホバーするとレビュー履歴などの詳細が出ます。セルの背景色は状態（`waiting` / `should` / `finished`）を表します。`others` / `requested` 列は幅を制限し、全文はホバーで表示します。
- stats: `days` を変更して `更新` を押すと集計期間が変わります。
- 設定: `⚙ 設定` で (1) ユーザーごとの文字色（color picker）、(2) 一覧から除外する GitHub リンク（PR / リポジトリ URL）、(3) レビュー集計で無視するアカウント（bot 等。`finished` / `should` 判定や `others` に含めない）を設定できます。設定は `$HOME/.config/gh-review-insight/`（`colors.json` / `excludes.json` / `ignored.json`）に保存され、次回起動時に読み込まれます。

## AI レビュー（Claude Code 連携）

status ビューで PR を選択し、まとめて Claude Code にレビューさせられます。実行は [herdr](https://herdr.dev) の専用 workspace `gh-review` 内の**対話モード**（1 PR = 1タブ、タブ名は `repo#番号`）なので、進行の様子を通常利用と同じ画面で確認でき、完了後はそのままタブ内で会話を継続できます。

1. 行頭のチェックボックス（「選択」列）で任意の PR を選択します。ツールバーの「未対応を選択」で `waiting` / `should` の一括選択、「解除」で全解除もできます。実行中のレビュータブがある PR は二重起動防止のため選択できません（タブを閉じれば再び選択できます）。
2. 「AIレビュー実行 (N)」を押すと、PR ごとに対話モードの `claude` が herdr のタブとして起動します。herdr クライアントが未アタッチならターミナル（Ghostty 優先、なければ Terminal.app）で `herdr` を自動で開きます。
3. 起動済みの PR は AI 列に「herdr」ボタンが出ます。押すと**その PR のタブに切り替えたうえで**、レビューウィンドウを前面に表示します（なければ新しくターミナルを開きます）。Ghostty はウィンドウ単位の操作に対応していないため、前面表示はアプリごと行います。起動済み状態は `ai_launched.json` に永続化され、タブが生きている限りアプリを再起動してもボタンが復元されます（タブを閉じた分は起動時に自動で片付きます）。
4. 起動に失敗した場合は AI 列に「失敗」と表示されます（ホバーでエラー内容を確認し、再選択して実行し直せます）。
5. レビューエージェントが `$HOME/.claude/pr-review-logs/<repo>/<PR番号>-<日時>.html` に出力した HTML レポートは、**report 列**にタイムスタンプ（`MM/DD hh:mm`）のボタンとして表示されます。クリックでブラウザが開きます。同じ PR に複数レポートがある場合は新しい順にすべて並びます。一覧は「⟳ 更新」のたびに再スキャンされます。

仕組みと注意:

- herdr が必要です（`brew install herdr`）。
- 実行ディレクトリは `$HOME/.config/gh-review-insight/workspace/` 固定です。初回はこのフォルダの信頼確認が claude 側で表示されるので承認してください（一度だけ）。
- **検証モード**: 本来のレビュースキル（`/gh-review:review`）は subagent を大量に使いトークン消費が激しく、複数 PR の同時実行でレートリミットに達しやすいため、現在は呼び出し経路の検証用に即答する軽量プロンプトを送ります。本来のプロンプトに戻すには `src/claude.rs` の `review_prompt` のコメントを入れ替えてください。
- レビュータブはアプリを終了しても残ります（herdr サーバーが保持）。claude を exit すればタブは閉じます。
- 過去のヘッドレス実行で保存されたレポート・セッションがある場合は、AI 列の「結果」「続き」ボタンから引き続き利用できます。

## 構成

- `src/gh.rs` — `gh api graphql` を実行する薄いラッパー（認証は gh に委譲）
- `src/claude.rs` — Claude Code CLI 連携（ヘッドレスレビュー実行・セッション永続化・Terminal 引き継ぎ）
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
