チームレビューボード（https://claude.ai/artifact/755vcTdVJ3iUyz4sd5svzF）で、前回からの「優先度・メモ」の変更を Slack チャンネル CDS9NNP0S の日付スレッドに通知する定期タスクです。以下の手順だけを行い、コミット・push・PR 作成・ユーザーへの質問・指定以外の Slack 投稿はしないでください。

REPO = /path/to/gh_review_insight （手元の clone のパスに書き換える）
D = 一時ディレクトリ（`mktemp -d` で毎回新しく作る）

1. 最新のスクリプトを取る：`git -C "$REPO" fetch -q origin claude/tender-lovelace-2i3zje && git -C "$REPO" checkout -q origin/claude/tender-lovelace-2i3zje -- scripts/team-board-notify.py`
   （作業中の変更があるブランチでも、このファイルだけを最新にする。失敗したら手元の scripts/team-board-notify.py をそのまま使う）
2. ArtifactData でボードのデータを読む（url は上記）：
   - list：collection "notes"、out_dir "$D/in"（$D/in/notes/*.json ができる）
   - get：collection "board"、doc_id "snapshot"、out_dir "$D/in"
   - get：collection "board"、doc_id "config"、out_dir "$D/in"
   - get：collection "board"、doc_id "notify"、out_dir "$D/in"（結果の version を控える）
   ArtifactData が使えない場合は、何も投稿せずに「ArtifactData が使えないため中止」と1行返して終了する。
3. 差分を出す：
   `python3 "$REPO/scripts/team-board-notify.py" --notes-dir "$D/in/notes" --snapshot "$D/in/board/snapshot.json" --config "$D/in/board/config.json" --state "$D/in/board/notify.json" --board-url https://claude.ai/artifact/755vcTdVJ3iUyz4sd5svzF > "$D/out.json"`
4. $D/out.json の reply が null なら、何も投稿せず、board/notify も更新せずに「変更なし」と1行返して終了する。
5. reply があるとき：
   a. 親メッセージの ts を決める。$D/in/board/notify.json の threads[date]（date は out.json の date）があればそれを使う。なければ Slack の slack_send_message（channel_id "CDS9NNP0S"、message は out.json の parent）で親を投稿し、返ってきた ts を使う。
   b. slack_send_message（channel_id "CDS9NNP0S"、thread_ts は a の ts、message は out.json の reply）でスレッドに返信する。reply_broadcast は付けない。
   c. out.json の notify を土台に threads[date] = a の ts を加えた JSON を $D/next.json に書き、ArtifactData の set（collection "board"、doc_id "notify"、file_path "$D/next.json"、if_version は手順2の version）で保存する。version 不一致なら notify を get し直し、その version で同じ内容を set する（1回まで）。
6. 最後に「通知 N 件」または失敗理由を1行で返す。notes コレクション、data/users 以下、board/snapshot、board/config には書き込まないこと。
