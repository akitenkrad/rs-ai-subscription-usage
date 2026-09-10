**English** | [日本語](output.ja.md)

# Output files

Everything the tool produces lands in one of two places: the Obsidian vault, which holds what a dashboard reads, and `~/.local/share/ai-subscription-usage/`, which holds the permanent record.

```
<vault>/_logs/_ai-subscription-usage/
├── claude/
│   ├── pricing.json     (input,  required)
│   ├── limit.json       (input,  optional)
│   ├── YYYY-MM.json     (output)
│   ├── _limits.json     (output)
│   └── _windows.json    (output, only when limit.json is present)
└── codex/
    ├── YYYY-MM.json     (output)
    └── _limits.json     (output)

~/.local/share/ai-subscription-usage/
├── claude/{state.json, limits.jsonl}
└── codex/{state.json, limits.jsonl}
```

Every file in the vault is written to a temporary sibling and then renamed, so a dashboard reading concurrently never sees half a JSON document.

If a previous layout (`_logs/_claude-usage/`, `_logs/_codex-usage/`) is still present, its files are copied into the new provider directory on the first non-dry run. Files that already exist at the new path are never overwritten.

## Inputs

### `claude/pricing.json` (required)

Prices are not compiled into the binary and not hard-coded in the dashboard; they are read from this file on every run. A missing or malformed table aborts the run rather than quietly turning every cost into `null`.

```json
{
  "aliases": {},
  "models": {
    "claude-opus-5": {"input": 5.0, "output": 25.0, "cache_write_5m": 6.25, "cache_write_1h": 10.0, "cache_read": 0.5},
    "claude-sonnet-5": {"input": 2.0, "output": 10.0, "cache_write_5m": 2.5, "cache_write_1h": 4.0, "cache_read": 0.2}
  }
}
```

Units are USD per million tokens. `aliases` maps a raw model name to a canonical one; the mapping is applied when the output is built, not when the log is read, so correcting an alias fixes past months on the next run without re-reading anything. Models absent from `models` are listed in the month's `unpriced_models` and their `cost_usd` is `null`.

### `claude/limit.json` (optional)

Describes the weekly usage limit so `_windows.json` can be built: the reset weekday and time, the base limit in API-equivalent USD, the meters (the official screen draws separate bars for "all models" and for Fable, so they are counted separately), any temporary multiplier campaigns, and the observation points read off the official screen. Without this file, or with a file carrying no observations, `_windows.json` is simply not written; a malformed one or a version mismatch is an error, so that "no baseline" and "typo" cannot be confused.

## Monthly aggregates

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

Days ascend by date; entries within a day sort by model, then by scope (`main` before `subagent`). `scope` is `subagent` when the transcript sits under a `subagents/` path or the line is marked as a sidechain. `cost_usd` is the API-equivalent reference amount, rounded to six decimals, and is `null` — never `0` — for a model with no price.

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

No `cost_usd`: there is no price table on the Codex side. `non_cached_input` and `normal_output` are conveniences for display, derived by subtraction — the raw values are kept untouched beside them, because `cached_input` is a subset of `input` and `reasoning_output` is a subset of `output`. Adding the derived and the raw figures together double-counts. `scope` is `unknown` when the log carries nothing to distinguish a main thread from a sub-thread.

## Usage limits

### `claude/_limits.json`

Built from the whole limits JSONL after each `claude limits` run.

| Field | Meaning |
|---|---|
| `updated_at` | When this file was written. |
| `source` | The endpoint the numbers came from. |
| `note` | The caveat, carried inside the data: these are whole-account figures, and the dollar denominators are `null` on Max 20x. |
| `history_days` | How many days `history` was cut to (60). |
| `history_from` | The oldest instant `history` may contain. |
| `latest` | The most recent raw response verbatim, plus `fetched_at`. |
| `history` | Oldest first: `fetched_at`, `five_hour_pct`, `five_hour_resets_at`, `seven_day_pct`, `seven_day_resets_at`, `seven_day_opus_pct`, `seven_day_sonnet_pct`. |

`latest` is the record with the greatest `fetched_at`, not the last line in the file, so a reordered append cannot make the newest reading move backwards. The API response is stored whole rather than mapped onto a struct: it contains keys whose purpose is not yet known, and dropping them now would mean they can never be recovered later.

### `codex/_limits.json`

| Field | Meaning |
|---|---|
| `version`, `provider` | Format version and `"codex"`. |
| `updated_at` | When this file was written. |
| `observed_at` | When the newest observation was made. This — not `updated_at` — is the "as of" time to show. |
| `source`, `note` | Where the numbers came from, and the caveats. |
| `limit_id`, `plan_type`, `credits` | As reported alongside the newest observation. |
| `rate_limits` | The newest `primary` / `secondary` windows, each `{used_percent, window_minutes, resets_at}`. |
| `history_days`, `history` | 60 days, thinned to one point per day. |

`history` keeps **the highest point of each day**, because what matters about a limit is how close you came, not the average and not the last reading — taking the last reading would show `0%` for any day observed just after a reset. The newest observation is always kept as well, even when it is not that day's peak, so the right-hand end of the chart matches the current figure shown beside it. Days are cut at JST midnight.

Thinning exists because Codex records an observation on *every response*: 985 points accumulated in three weeks (measured 2026-09-10), which would have made `_limits.json` 262 KB — heavy for a file a dashboard reads on every open, and illegible as a chart. The JSONL behind it is not thinned.

If nothing has ever been observed, `_limits.json` is not written at all. A file reading `0%` would conflate "not measured yet" with "not used yet".

## The pitfall: `primary` and `secondary` are seats, not window lengths

Codex reports its rate limits in two slots named `primary` and `secondary`. It is tempting to read `primary` as "the 5-hour window" and `secondary` as "the weekly window". **That mapping is not stable.** Counting 985 real observations (2026-09-10) turns up three different shapes:

| `plan_type` | `primary.window_minutes` | `secondary` |
|---|---|---|
| plus | 10080 (weekly) | null |
| plus | 300 (5 hours) | 10080 (weekly) |
| prolite | 10080 (weekly) | null |

The same plan name yields different arrangements at different times, and on `prolite` **there is no 5-hour window at all**. Hard-coding "primary is the 5-hour bar" would therefore render a weekly figure under a 5-hour label on exactly the plans where the mistake is hardest to notice — the bar would be filled with a number that resets seven days later than the caption claims.

So the CLI does not interpret the slots. It records `window_minutes` as observed and passes both slots through unchanged; deciding which bar is which is the display's job, and it must decide by **length**, not by slot name:

```js
// Pick the window by its length, never by "primary" / "secondary".
const windows = [limits.rate_limits.primary, limits.rate_limits.secondary].filter(Boolean);
const fiveHour = windows.find(w => w.window_minutes <= 24 * 60);   // may be undefined
const weekly   = windows.find(w => w.window_minutes >  24 * 60);
```

`history` follows the same rule: each row carries `primary_window_minutes` and `secondary_window_minutes` next to the percentages, rather than folding them into two fixed series.

`resets_at` stays a Unix timestamp exactly as observed, and stays `null` when it was not observed. `null` means "unknown" — it is never filled in with `0` or with the current time.

## The long-term record

### `state.json`

The per-provider incremental state: for each source file, its `mtime`, its size, and the totals derived from it. It is not a cache. Claude Code deletes transcripts after roughly 30 days, so for anything older than that, `state.json` is the *only* remaining copy of those totals. Entries whose source file has disappeared are kept and flagged `missing`, and keep contributing to the monthly totals; because the key is the file path, a deleted file cannot reappear and be counted twice.

### `limits.jsonl`

Append-only, never rotated, never trimmed by date.

- `claude/limits.jsonl` — one raw API response per line, each with a `fetched_at`.
- `codex/limits.jsonl` — one observation per line, including the whole `rate_limits` object as `raw`. Fields the tool does not model (`limit_name`, `individual_limit`, `spend_control_reached`, `rate_limit_reached_type`) survive only in that copy — and the field saying whether a limit was actually hit is often the only way to reconstruct, months later, why a given week stopped.

Codex observations are deduplicated by `observed_at` when appending, so re-scanning the same logs — including a full `--all` rescan — adds no rows. Claude's usage endpoint reports only the present moment; there is no way to fetch a past reading. **Delete these files and the points recorded so far are gone permanently.** Back them up together with `state.json`.
