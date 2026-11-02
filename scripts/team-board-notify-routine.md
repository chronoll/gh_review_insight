チームレビューボード（https://claude.ai/artifact/755vcTdVJ3iUyz4sd5svzF）で、前回からの「優先度・メモ」の変更を Slack チャンネル CDS9NNP0S の日付スレッドに通知する定期ジョブです。以下の手順だけを行い、コミット・PR 作成・ユーザーへの質問・指定以外の Slack 投稿はしないでください。

D をスクラッチパッド内の空ディレクトリとする。

1. リポジトリ chronoll/gh_review_insight の作業ツリーで `git fetch origin claude/tender-lovelace-2i3zje && git checkout -q origin/claude/tender-lovelace-2i3zje -- scripts/team-board-notify.py` を実行して最新のスクリプトを取る。
2. ArtifactData で読む（url は上記）：
   - list：collection "notes"、out_dir "D/in"（D/in/notes/*.json ができる）
   - get：collection "board"、doc_id "snapshot"、out_dir "D/in"
   - get：collection "board"、doc_id "config"、out_dir "D/in"
   - get：collection "board"、doc_id "notify"、out_dir "D/in"（結果の version を控える）
3. `python3 scripts/team-board-notify.py --notes-dir D/in/notes --snapshot D/in/board/snapshot.json --config D/in/board/config.json --state D/in/board/notify.json --board-url https://claude.ai/artifact/755vcTdVJ3iUyz4sd5svzF > D/out.json`
4. D/out.json の reply が null なら、何も投稿せず、board/notify も更新せずに「変更なし」と1行返して終了。
5. reply があるとき：
   a. 親メッセージの ts を決める。D/in/board/notify.json の threads[date]（date は out.json の date）があればそれを使う。なければ Slack の slack_send_message（channel_id "CDS9NNP0S"、message は out.json の parent）で親を投稿し、返ってきた ts を使う。
   b. slack_send_message（channel_id "CDS9NNP0S"、thread_ts は a の ts、message は out.json の reply）でスレッドに返信する。reply_broadcast は付けない。
   c. out.json の notify を土台に threads[date] = a の ts を加えた JSON を D/next.json に書き、ArtifactData set（collection "board"、doc_id "notify"、file_path "D/next.json"、if_version は手順2の version）で保存する。version 不一致なら notify を get し直し、その version で同じ内容を set（1回まで）。
6. 最後に「通知 N 件」または失敗理由を1行で返す。notes コレクション、data/users 以下、board/snapshot、board/config には書き込まないこと。
