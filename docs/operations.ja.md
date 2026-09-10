[English](operations.md) | **日本語**

# 運用

## 前提

- ビルドに Rust ツールチェーン（edition 2021）．
- macOS．`claude limits` は `security find-generic-password` でログインキーチェーンから OAuth トークンを読み，定期実行は launchd を使います．
- Claude Code / Codex CLI がインストールされ，`~/.claude/projects` と `~/.codex/sessions` にログを書いていること．
- 書き出し先の Obsidian vault（`OBSIDIAN_VAULT` で指定）．

## ビルドとテスト

```bash
cargo build --release
make test          # cargo test
```

## 定期実行ジョブの導入

```bash
make deploy
```

`deploy` は `cargo install --path .` でバイナリを入れ，4 つの launchd plist を `~/Library/LaunchAgents/` へコピーし，それぞれを入れ直します（`launchctl bootout` — 読み込まれていない場合の失敗は無視 — の後に `launchctl bootstrap`）．変更後に何度実行しても安全です．

| ジョブ | コマンド | 実行間隔 |
|---|---|---|
| `…ai-subscription-usage` | `claude` | 毎日 05:10 |
| `…ai-subscription-usage-codex` | `codex` | 毎日 05:15 |
| `…ai-subscription-usage-limits` | `claude limits` | 毎時（読み込み時にも実行） |
| `…ai-subscription-usage-codex-limits` | `codex limits` | 毎時（読み込み時にも実行） |

集計は 1 日 1 回，5 分ずらして回します．利用制限を毎時にしてあるのは，**後から取り直せない**からです．Anthropic のエンドポイントは «今この瞬間» しか返さず，Codex の観測値はセッションログが刈られた時点で消えます．折れ線の解像度は毎時の標本化がそのまま決めます．

各ジョブの stdout / stderr は `~/Library/Logs/obsidian-ai-subscription-usage*.log` と `*.err` に落ちます．

launchd は絶対パスを要求するので，同梱の plist にはインストール先のバイナリとログファイルの絶対パスが書かれており，かつ作者のアカウント向けになっています．**`make deploy` の前に，自分のユーザ用にパスを書き換えてください．**また plist は `OBSIDIAN_VAULT` を設定していないため，launchd から回すと組み込みの既定 vault パスへ書きます．vault が別の場所にあるなら `EnvironmentVariables` を足してください．

読み込まれているものの確認，あるいは 1 つを手で回す:

```bash
launchctl list | grep ai-subscription-usage
launchctl kickstart -k "gui/$(id -u)/com.akitenkrad.obsidian.ai-subscription-usage-codex-limits"
```

## バックアップ

`~/.local/share/ai-subscription-usage/` をバックアップしてください（両プロバイダの `state.json` と `limits.jsonl`）．

これらはキャッシュではありません．入力ログは 1 か月ほどで刈られるので，古い日次集計の唯一の写しは state ファイルにあり，これまでに取った利用制限の観測値の唯一の写しは JSONL にあります．vault 側の出力はこれらから作り直せますが，これら自身はどこからも作り直せません．

vault のファイル自体は，vault のバックアップに任せて構いません．

## トラブルシューティング

**`価格表を読めません`**．`<vault>/_logs/_ai-subscription-usage/claude/` に `pricing.json` がありません．作成してください（→ [出力ファイル](output.ja.md)）．これを意図的に致命エラーにしてあるのは，そうしないと «1 か月ぶんの `null` の金額» が «ファイルが無い» ではなく «価格表の穴» のように見えるためです．

**`transcript のディレクトリがありません` / `Codex sessions のディレクトリがありません`**．`~/.claude/projects` または `~/.codex/sessions` が存在しないか，`HOME` が想定外の場所を指しています．

**`キーチェーンから認証情報を読めません`**．`Claude Code-credentials` の項目が見つからないか，アクセスが拒否されました．Claude Code にログインしているか，macOS の許可ダイアログでキーチェーンへのアクセスを許可したかを確認してください．ダイアログはターミナルのセッション側に出るので，**`claude limits` の初回は launchd からではなく手で実行する**のが確実です．

**`Claudeの認証情報の有効期限が切れています`**．Claude Code 側で再ログインしてください．トークンの更新はそちらで行われます．

**`使用制限 API が HTTP 401 を返しました`**．トークンが拒否されました．たいていは再ログインで解消します．新しい版の Claude Code がキーチェーンの項目名を変えた場合は，そもそも読み取りの段階で失敗するので，キーチェーンのメッセージの方が出ます．

**`error: unexpected argument '--dry-run' found`**．フラグを操作の後ろに置いています．フラグはプロバイダに属するので，`codex limits --dry-run` ではなく `codex --dry-run limits` と書いてください（→ [CLI リファレンス](cli.ja.md)）．

**`[WARN] JSON として読めない行が N 行ありました`**．ログの一部の行が JSON として読めませんでした．その行は数えて読み飛ばし，残りは通常どおり集計しています．中断されたセッションの末尾が切れている場合がほとんどです．

**`_windows.json` が書かれない**．Claude の出力ディレクトリに `limit.json` があり，**かつ**観測点が 1 つ以上必要です．無ければ黙って飛ばします — これはエラーではありません．

**Codex の `_limits.json` が書かれない**．利用制限の観測値がまだ 1 件も集まっていません．`0%` のファイルは書かない方針です（«本当に使っていない» と区別できなくなるため）．Codex を一度使ってから `codex limits` を回し直してください．

**state の版が変わって全部読み直した**．より細かいデータを要する版上げの後に起きる想定どおりの挙動です．既に刈られた分は埋められないので，新しく足した次元に依存する数字を古い期間について語らないでください．
