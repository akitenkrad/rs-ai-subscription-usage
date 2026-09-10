[English](output.md) | **日本語**

# 出力ファイル

このツールが作るものは 2 か所に落ちます．ダッシュボードが読む Obsidian vault と，永続記録を置く `~/.local/share/ai-subscription-usage/` です．

```
<vault>/_logs/_ai-subscription-usage/
├── claude/
│   ├── pricing.json     (入力，必須)
│   ├── limit.json       (入力，任意)
│   ├── YYYY-MM.json     (出力)
│   ├── _limits.json     (出力)
│   └── _windows.json    (出力，limit.json がある場合のみ)
└── codex/
    ├── YYYY-MM.json     (出力)
    └── _limits.json     (出力)

~/.local/share/ai-subscription-usage/
├── claude/{state.json, limits.jsonl}
└── codex/{state.json, limits.jsonl}
```

vault のファイルはすべて，一時ファイルへ書いてから rename します．読んでいる最中のダッシュボードに半端な JSON を見せないためです．

旧レイアウト（`_logs/_claude-usage/`，`_logs/_codex-usage/`）が残っていれば，dry-run でない初回実行時に新しい provider ディレクトリへコピーします．新しい側に既にあるファイルは上書きしません．

## 入力

### `claude/pricing.json`（必須）

価格はバイナリにもダッシュボードにも焼き込まず，毎回このファイルから読みます．無い / 壊れている場合は，黙って全部の金額を `null` にするのではなく実行を止めます．

```json
{
  "aliases": {},
  "models": {
    "claude-opus-5": {"input": 5.0, "output": 25.0, "cache_write_5m": 6.25, "cache_write_1h": 10.0, "cache_read": 0.5},
    "claude-sonnet-5": {"input": 2.0, "output": 10.0, "cache_write_5m": 2.5, "cache_write_1h": 4.0, "cache_read": 0.2}
  }
}
```

単位は USD / 100 万トークンです．`aliases` は生のモデル名を正規名へ写す表で，適用はログを読むときではなく出力を組み立てるときに行います．したがって別名を直せば，再走査せずとも次回実行で過去の月の集計が直ります．`models` に無いモデルはその月の `unpriced_models` に並び，`cost_usd` は `null` になります．

### `claude/limit.json`（任意）

`_windows.json` を作るための，週の利用制限の基準です．リセットの曜日と時刻，API 換算額（USD）での基準上限，メーター（公式画面は «すべてのモデル» と «Fable» を別のバーで出すので，集計も分ける），一時的な倍率キャンペーン，そして公式画面から読み取った観測点を持ちます．このファイルが無い場合，あるいは観測点が 1 つも無い場合は，`_windows.json` を単に書きません．一方で壊れている / 版が違う場合はエラーにします — «基準が無い» のか «書き間違えた» のかを混同しないためです．

## 月次の集計

### `claude/YYYY-MM.json`

```json
{
  "version": 1,
  "provider": "claude",
  "month": "2026-08",
  "updated_at": "2026-08-31T05:10:00+09:00",
  "days": [
    {
      "date": "2026-08-22",
      "entries": [
        {
          "model": "claude-opus-5", "scope": "main", "requests": 12,
          "input": 240, "output": 3100,
          "cache_write_5m": 0, "cache_write_1h": 31533, "cache_read": 26479,
          "cost_usd": 0.401234
        }
      ]
    }
  ],
  "unpriced_models": []
}
```

`days` は日付の昇順，`entries` はモデル名の昇順 → scope（`main` → `subagent`）の順です．transcript のパスが `subagents/` を含むか，行が sidechain の印を持つ場合に `scope` が `subagent` になります．`cost_usd` は API 換算の参考額で，小数第 6 位に丸めてあります．価格の無いモデルでは `null` であって `0` ではありません．

### `codex/YYYY-MM.json`

```json
{
  "version": 1,
  "provider": "codex",
  "month": "2026-09",
  "updated_at": "2026-09-05T20:15:00.123456+00:00",
  "days": [
    {
      "date": "2026-09-05",
      "entries": [
        {
          "model": "gpt-5", "scope": "main", "requests": 1,
          "input": 100, "cached_input": 0, "non_cached_input": 100,
          "output": 50, "reasoning_output": 0, "normal_output": 50,
          "cache_write_input": 0
        }
      ]
    }
  ]
}
```

`cost_usd` はありません．Codex 側に価格表を持っていないためです．`non_cached_input` と `normal_output` は表示のために引き算で出した値で，原始値はその隣にそのまま残してあります — `cached_input` は `input` の部分集合，`reasoning_output` は `output` の部分集合だからです．**導出値と原始値を足すと二重に数えます．**メインの会話かサブスレッドかをログから判別できない場合，`scope` は `unknown` になります．

## 利用制限

### `claude/_limits.json`

`claude limits` を実行するたび，JSONL 全体から組み立て直します．

| 欄 | 意味 |
|---|---|
| `updated_at` | このファイルを書いた時刻． |
| `source` | 数字の出どころ（エンドポイント）． |
| `note` | 但し書きをデータ自身に持たせたもの．アカウント全体の値であること，Max 20x では金額の分母が `null` であること． |
| `history_days` | `history` を何日ぶんに絞ったか（60）． |
| `history_from` | `history` に入り得るいちばん古い時刻． |
| `latest` | 最新の生レスポンスをそのまま（`fetched_at` 付き）． |
| `history` | 古い順．`fetched_at`，`five_hour_pct`，`five_hour_resets_at`，`seven_day_pct`，`seven_day_resets_at`，`seven_day_opus_pct`，`seven_day_sonnet_pct`． |

`latest` は «ファイルの最後の行» ではなく «`fetched_at` が最大のレコード» です．行の順序が入れ替わっても最新が後戻りしないようにするためです．API の応答は構造体に写さず丸ごと保存します — 用途の分からない鍵が含まれており，いま落とすと後から意味が分かっても復元できないからです．

### `codex/_limits.json`

| 欄 | 意味 |
|---|---|
| `version`，`provider` | 形式の版と `"codex"`． |
| `updated_at` | このファイルを書いた時刻． |
| `observed_at` | 最新の観測の時刻．画面に «いつ時点の数字か» として出すのは `updated_at` ではなくこちら． |
| `source`，`note` | 数字の出どころと但し書き． |
| `limit_id`，`plan_type`，`credits` | 最新の観測に付随していた値． |
| `rate_limits` | 最新の `primary` / `secondary`．いずれも `{used_percent, window_minutes, resets_at}`． |
| `history_days`，`history` | 60 日ぶんを，1 日 1 点に間引いたもの． |

`history` に残すのは **その日いちばん高かった点**です．利用制限について知りたいのは «その日どこまで迫ったか» であって，平均でも最後の値でもありません（最後の値を採ると，枠がリセットされた直後に観測した日が `0%` に見えます）．そのうえで**最新の観測はその日の最大でなくても必ず残します** — 折れ線の右端と，その隣に出ている «いま何 %» が食い違わないようにするためです．日の区切りは JST です．

間引くのは，Codex が**応答ごとに**観測値を残すからです．3 週間で 985 点貯まり（2026-09-10 実測），そのまま載せると `_limits.json` は 262 KB になりました — 画面を開くたびに読むファイルとしては重く，しかも折れ線としては 1 日の中の点が潰れて読めません．裏の JSONL は間引きません．

観測が 1 件も無い場合，`_limits.json` は書きません．`0%` のファイルを置くと «まだ観測できていない» と «使っていない» が区別できなくなるためです．

## 落とし穴: `primary` / `secondary` は «席» であって «枠の長さ» ではない

Codex は利用制限を `primary` と `secondary` という 2 つの枠で返します．`primary` を «5 時間枠»，`secondary` を «週次枠» と読みたくなりますが，**その対応は固定ではありません．** 実ログ 985 件を数えると（2026-09-10），3 通りの形が現れます．

| `plan_type` | `primary.window_minutes` | `secondary` |
|---|---|---|
| plus | 10080（週次） | null |
| plus | 300（5 時間） | 10080（週次） |
| prolite | 10080（週次） | null |

同じプラン名でも時期によって並びが変わり，`prolite` では **5 時間枠がそもそも存在しません**．「primary は 5 時間のバー」と決め打つと，まさに気づきにくいプランで，週次の値を 5 時間枠のラベルの下に描いてしまいます — バーに入っている数字は，見出しが言うより 7 日遅れてリセットされる値です．

そこで **CLI は枠を解釈しません．** `window_minutes` を観測値のまま記録し，2 つの枠をそのまま通します．どちらのバーかを決めるのは画面側の仕事であり，判定は枠の名前ではなく**長さ**で行う必要があります．

```js
// 枠は長さで選ぶ．«primary» / «secondary» で選ばない．
const windows = [limits.rate_limits.primary, limits.rate_limits.secondary].filter(Boolean);
const fiveHour = windows.find(w => w.window_minutes <= 24 * 60);   // 無いこともある
const weekly   = windows.find(w => w.window_minutes >  24 * 60);
```

`history` も同じ方針で，各行は消化率の隣に `primary_window_minutes` と `secondary_window_minutes` を持ちます．2 本の固定された系列に畳みません．

`resets_at` は観測されたまま Unix 秒で持ち，観測されなければ `null` のままです．`null` は «分からない» の意味であり，`0` や現在時刻で埋めることはありません．

## 長期記録

### `state.json`

プロバイダごとの増分状態です．入力ファイルごとに `mtime`・サイズ・そこから得た集計を持ちます．これは**キャッシュではありません**．Claude Code は約 30 日で transcript を削除するので，それより古い集計は `state.json` にしか残っていません．元のファイルが消えたエントリは捨てずに `missing` の印を付けて保持し，月次の合算に含め続けます．鍵がファイルのパスなので，消えたファイルが復活して二重計上されることはありません．

### `limits.jsonl`

追記のみ．回転もしませんし，日付で切ることもしません．

- `claude/limits.jsonl` — 1 行が生の API 応答 1 件（`fetched_at` 付き）．
- `codex/limits.jsonl` — 1 行が観測値 1 件．`rate_limits` オブジェクトを `raw` として丸ごと含みます．CLI が型に写していない欄（`limit_name`，`individual_limit`，`spend_control_reached`，`rate_limit_reached_type`）はこの写しにしか残らず，とくに «実際に上限に当たったか» を示す欄は，何か月か後に «あの週なぜ止まったのか» を復元する唯一の手がかりになります．

Codex の観測値は追記時に `observed_at` で重複を落とすので，同じログを何度走査しても（`--all` の全走査を含めて）行は増えません．Claude の利用制限 API は «今この瞬間» しか返さず，過去に遡って取り直す手段はありません．**これらのファイルを消すと，そこまでに記録した点は永久に戻りません．**`state.json` と一緒にバックアップの対象に含めてください．
