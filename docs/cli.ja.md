[English](cli.md) | **日本語**

# CLI リファレンス

```
Usage: ai-subscription-usage <COMMAND>

Commands:
  claude
  codex
  all
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

プロバイダの指定は必須です．引数無しで起動すると usage を出して失敗します．

## フラグはサブコマンドの前に置く

各プロバイダはオプションと（省略可能な）操作の両方を取りますが，オプションは**操作ではなくプロバイダ側**に定義されています．したがって，これは通り，

```bash
ai-subscription-usage codex --dry-run limits
```

これは通りません．

```bash
ai-subscription-usage codex limits --dry-run
# error: unexpected argument '--dry-run' found
```

`limits` は `codex` のサブコマンドなので，パーサがそこへ降りた後は `limits` 自身のオプションしか見えません（そして `limits` にオプションはありません）．プロバイダに効かせたいものは，操作名より前に書く必要があります．本ドキュメントの例はすべてこの順です．

## `claude`

```bash
ai-subscription-usage claude [OPTIONS] [limits]
```

操作を付けない場合は `~/.claude/projects/**/*.jsonl` を走査し，各応答を（日付・時・モデル・scope）で束ねて，月ごとに `YYYY-MM.json` を vault へ書きます．出力先に `limit.json` があり，かつ観測点が 1 つ以上ある場合に限り `_windows.json` も書きます．

価格表は必須です．`pricing.json` が無い / 壊れている場合は，全部の金額を黙って `null` にするのではなく実行を失敗させます．

### オプション

| オプション | 効果 |
|---|---|
| `--all` | `mtime` と `size` による新鮮さの判定を無視し，実在する transcript をすべて読み直す．**既に消えた transcript の集計は捨てない．** |
| `--month <YYYY-MM>` | その月のファイルだけを書く．コンソールの要約には全月が出る．データが無い月ならその旨を表示する． |
| `--dry-run` | 走査して要約を出すだけ．月次 JSON も state も旧レイアウトの移行も，一切書かない． |
| `--forget-missing` | 実在しなくなった transcript の集計を永久に捨てる．**元の transcript が無いので復元できない．** |

### `claude limits`

```bash
ai-subscription-usage claude limits
```

ログインキーチェーン（項目 `Claude Code-credentials`．`security find-generic-password` で読む）からアクセストークンを読み，`GET https://api.anthropic.com/api/oauth/usage` を 1 回叩き，応答を長期記録の JSONL へ追記したうえで，JSONL 全体から `_limits.json` を作り直します．アクセストークンが失効していても，リフレッシュトークンが非空で，その失効時刻が未来または不明なら，一時的な失効として扱います．stdout に失効時刻（JST の RFC3339，秒まで）と Claude Code の起動で更新される旨を表示し，API 取得を見送って正常終了します（終了コード `0`）．JSONL と前回の `_limits.json` は変更しません．リフレッシュトークンが無い・空・失効済みの場合は，失効時刻と `/login` による再ログインの案内を stderr に出してエラー終了します（終了コード `1`）．

返ってくる % は **アカウント全体**（claude.ai / Cowork を含む）の値で，Claude Code だけの分ではありません．この但し書きは出力ファイル自身にも書き込んであり，ダッシュボードだけを見た人が取り違えないようにしてあります．

この操作に固有のオプションはありません．

## `codex`

```bash
ai-subscription-usage codex [OPTIONS] [limits]
```

操作を付けない場合は `~/.codex/sessions/**/rollout-*.jsonl` を走査し，課金対象のトークン量を集計して月次 JSON を書き，**同じ 1 回の走査で**利用制限の観測値をすべて長期記録へ落として `_limits.json` を作り直します．両方を 1 回の走査でやるのは意図的で，2 種類のデータが同じ行の並びから取れるため，利用制限の収集に追加の走査費用がかかりません．

素の `codex` 実行中に利用制限の更新が失敗した場合は警告に留め，月次 JSON は書き切ります．`codex limits` では同じ失敗がエラーになります — そちらは利用制限こそが仕事だからです．

月次ファイルは暦月ごとに 1 つずつ `YYYY-MM.json` として書かれ，各ファイルにはその月に属する日だけが入ります．`--month` は **どの月を書くか**の絞り込みであって，ファイル名や束ね方を変えるものではありません．

### オプション

`claude` と同じ 4 つですが，プロバイダ固有の意味があります．

| オプション | 効果 |
|---|---|
| `--all` | `mtime` と `size` が変わったものだけでなく，全セッションログを読み直す． |
| `--month <YYYY-MM>` | 書き出す月をその月だけに絞る． |
| `--dry-run` | 何を書く予定だったか（追記される観測値の件数を含む）を表示するだけで，何も書かない． |
| `--forget-missing` | 実在しなくなったセッションログの state を捨てる．その集計も失われる． |

### `codex limits`

```bash
ai-subscription-usage codex limits
```

走査の仕方は同じですが，更新するのは利用制限だけです（長期記録の JSONL と `_limits.json`）．月次 JSON には触れません．毎時のジョブはこれを回します．

Codex の利用制限は **ログに残った観測値**であって API への問い合わせではないので，Codex が新しい行を書くより高い頻度で回しても新しい点は増えません．

## `all`

予約されていますが未実装です．実行するとその旨のエラーで終了します．`claude` と `codex` を別々に使ってください．

## 環境変数

| 変数 | 意味 | 既定 |
|---|---|---|
| `OBSIDIAN_VAULT` | vault のルート．出力はすべて `<vault>/_logs/_ai-subscription-usage/` の下． | 作者の vault のパス．**必ず明示すること．** |
| `HOME` | 入力ログと長期記録の場所（`~/.claude/projects`，`~/.codex/sessions`，`~/.local/share/ai-subscription-usage/`）． | 未設定なら `/` |

## 終了状態と診断

- `0` — 正常終了．書いたファイルは 1 つずつ `wrote <path>` として表示される．
- `1` — 実行時エラー．stderr に `[ERROR] <メッセージ>` を出す（価格表が無い，入力ディレクトリが無い，アクセストークンが失効してリフレッシュトークンも使えない，HTTP 失敗，`limit.json` が壊れている等）．
- `2` — clap がコマンドラインを解釈できなかった（操作の後ろにフラグを置いた場合など）．

警告は stderr に `[WARN]` として出し，実行は止めません．JSON として読めない行は数えて読み飛ばし，版の分からない state は空から始める旨を告げ，集計中の利用制限の更新の失敗は記録したうえで月次 JSON は書き切ります．
