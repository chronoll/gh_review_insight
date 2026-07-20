# gh-review-insight

## ビルドと反映

- `cargo run` はソースから最新の debug ビルドを起動する。/Applications の `.app` は最後にインストールした release バイナリのままで、ソース変更は自動反映されない
- `.app`（Dock / Spotlight / ログイン項目からの起動）に変更を反映するには、必ず以下を実行する:

```bash
cargo build --release
./scripts/bundle-macos.sh --install
```
