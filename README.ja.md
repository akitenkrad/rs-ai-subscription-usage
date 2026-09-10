<p align="center"><img src="docs/assets/hero.svg" width="100%"></p>

[English](README.md) | **日本語**

`ai-subscription-usage` は，Claude Code と Codex CLI がローカルに残すセッションログを読み，それぞれのサブスクリプションの利用量を集計して，ダッシュボードが描画できる JSON として Obsidian vault に書き出す CLI です．2 つのプロバイダは最後まで分けたままにします — ディレクトリも，ファイルも別で，1 つの金額や 1 つの消化率に合算することはありません．Claude の 1 トークンと Codex の 1 トークンは意味が違い，利用制限の母数も別物だからです．利用制限の消化率をトークン数から推定することもありません．ログや提供元の API に含まれる観測値だけを正とします．

## インストール

```bash
cargo install --path .
```

バイナリ名は `ai-subscription-usage` です．実質 macOS 専用で，Claude の利用制限の取得はログインキーチェーンから `security` 経由で OAuth トークンを読み，定期実行は launchd で行います．

## クイックスタート

```bash
# 書き出し先．既定値は作者の vault なので必ず明示する．
export OBSIDIAN_VAULT="$HOME/path/to/vault"

# Claude は価格表が無いと動かない．詳細は docs/output.ja.md を参照．
mkdir -p "$OBSIDIAN_VAULT/_logs/_ai-subscription-usage/claude"
cat > "$OBSIDIAN_VAULT/_logs/_ai-subscription-usage/claude/pricing.json" <<'JSON'
{"models":{"claude-opus-5":{"input":5.0,"output":25.0,"cache_write_5m":6.25,"cache_write_1h":10.0,"cache_read":0.5}}}
JSON

# 何も書かずに走査だけする．フラグはサブコマンドの「前」に置く．
ai-subscription-usage claude --dry-run
ai-subscription-usage codex --dry-run

# 実際に集計し，続けて利用制限を記録する．
ai-subscription-usage claude
ai-subscription-usage claude limits
ai-subscription-usage codex
ai-subscription-usage codex limits
```

## ドキュメント

- [使いどころ](docs/usecases.ja.md) — 何に答えるための道具か．そして何にはあえて答えないか．
- [CLI リファレンス](docs/cli.ja.md) — 全サブコマンドとフラグ．フラグをサブコマンドの前に置く理由も．
- [出力ファイル](docs/output.ja.md) — vault と長期記録に何がどういう形で書かれるか．
- [アーキテクチャ](docs/architecture.ja.md) — common 層と provider 層の分離，増分走査，冪等性，トークンの意味．
- [運用](docs/operations.ja.md) — launchd ジョブの導入，バックアップ，トラブルシューティング．

## ライセンス

MIT — [LICENSE](LICENSE) を参照．
