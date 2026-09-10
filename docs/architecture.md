**English** | [日本語](architecture.ja.md)

# Architecture

## Two providers, never merged

The top-level split is by provider, and it goes all the way down: separate scanners, separate state files, separate output directories, separate limit records. Nothing anywhere adds Claude and Codex into one figure.

That is not an accident of implementation. The two subscriptions are metered against different denominators — Claude's weekly limit is measured in API-equivalent dollars weighted by model and cache class, Codex's in percentages the server reports against windows whose length varies by plan — and a token means a different amount of money in each. A combined "43% consumed" or a combined dollar total would be a number that no decision can be based on. The layout makes the merge awkward on purpose.

## The common layer and the provider layer

```
src/
├── cli.rs         clap definitions: provider -> optional operation + flags
├── main.rs        dispatch to the provider, print [ERROR], exit 1
├── config.rs      every path in one place; HOME and OBSIDIAN_VAULT
├── common/        what the providers share
│   ├── aggregation.rs   UsageRecord, TokenUsage, Scope, CacheClass
│   ├── state.rs         incremental state, versioning, migration
│   ├── atomic_write.rs  write to a temp file, then rename
│   ├── date.rs          the fixed +09:00 offset
│   ├── discovery.rs     recursive *.jsonl enumeration
│   └── jsonl.rs         line reading
├── claude/        transcripts, price table, weekly windows, usage API
└── codex/         session logs, rate-limit observations
```

`common` holds the vocabulary and the mechanics: what a usage record is, how incremental state works, how a file is written safely. It holds no knowledge of either log format.

The provider modules hold everything that is specific, and they are asymmetric because the providers are. Claude has a price table, a weekly-window model and an authenticated usage API; Codex has neither prices nor an API, but leaves rate-limit observations scattered through its own logs. Forcing a shared abstraction over that difference would only hide it.

`config.rs` is the single place that knows a path. Nothing else joins path components, so relocating the output or the state is a one-file change, and the tests can point `HOME` and `OBSIDIAN_VAULT` at temporary directories and get a fully isolated run.

## Incremental scanning and idempotency

Rescanning must be free of consequence: the jobs run daily and hourly, on overlapping data, and a manual run may happen at any time in between.

Two mechanisms provide that.

**Freshness by `mtime` + size.** Each entry in `state.json` records the source file's modification time and size along with the totals derived from it. Unchanged files are skipped. `--all` overrides the check and re-reads everything that still exists.

**Deduplication by identity, not by position.** Within a Claude transcript, responses are deduplicated by `message.id`; within a Codex log, by `response_id`. Codex limit observations are deduplicated by `observed_at` when appending to the JSONL, which is what makes a full `--all` rescan add zero rows. Identity is checked at the point of writing, so idempotency does not depend on the state file being intact.

There is one deliberate exception to skipping unchanged files. Rate-limit harvesting was added after the aggregation already existed, which meant files scanned before that change had been marked fresh without their observations ever being collected — and a skipped file yields nothing. Each state entry therefore carries a `limits_harvested` flag; an entry without it is re-read exactly once. Without that flag, only 321 of 985 actual observations reached the long-term record (measured 2026-09-10), and the source logs get pruned, so the shortfall would have been unrecoverable.

## The state files are the record, not a cache

Claude Code deletes transcripts under `~/.claude/projects` after about 30 days; Codex prunes `~/.codex/sessions` too. Consequently:

- Only the last 30 days can ever be recomputed from source. Everything older exists solely inside `state.json`.
- When a source file disappears, its state entry is kept and flagged `missing`, and its totals keep contributing to the monthly output. `--forget-missing` drops them, and that is irreversible.
- The record lives under `~/.local/share/`, not `~/.cache/` — a cache directory is something the OS is entitled to clear.
- Every write goes through a temp file and a rename. A crash mid-write must not be able to destroy history.
- Both `state.json` and `limits.jsonl` belong in your backups.

`state.json` is versioned. A version bump that needs richer data (adding hour granularity, then human prompt counts) triggers a full re-read of the transcripts that still exist, and the docs of the upgrade path are explicit that whatever has already been deleted cannot be backfilled — so figures derived from the missing dimension must not be quoted for old periods.

## What counts as usage

### Codex writes the same usage twice

Codex records the token usage of one response in two places: a `token_usage_record` line, and an `event_msg` line of type `token_count` carrying `last_token_usage`. They describe the same response. **Only the former is counted as billable** — summing both doubles the bill. The latter is used for something else entirely: its `rate_limits` payload is the source of every usage-limit observation, and its token figures describe the context window.

### Tokens nest

`cached_input` is a subset of `input`; `reasoning_output` is a subset of `output`. The raw values are stored as reported, and the differences (`non_cached_input`, `normal_output`) are computed for display by saturating subtraction. Adding a derived value to the raw value it came from counts the same tokens twice. When the nesting is violated in the source data — `cached_input` exceeding `input`, say — the record is kept with its original values and the violation is recorded as an anomaly rather than being clamped away.

### Money is a reference amount

Dollar figures are what the same traffic would have cost at published API list prices. They are not an invoice; the subscription is a flat fee. Prices come from `pricing.json` at run time, never from the binary, and a model with no entry yields `null`, not `0` — the distinction between "free" and "unknown" has to survive into the dashboard.

### Limits are observed, never inferred

No percentage anywhere is derived from token counts. Codex's come from the `rate_limits` object in its own logs; Claude's come from `GET https://api.anthropic.com/api/oauth/usage`, the same endpoint the `/usage` screen reads. Both are whole-account figures — claude.ai and Cowork usage is included in Claude's, and neither can be attributed to CLI activity alone — and both output files carry that caveat as a `note` field so it travels with the data.

The one place where a limit *is* reconstructed rather than read, Claude's `_windows.json`, is explicit about its own limits: because only Claude Code traffic is visible locally, the computed percentage is a **lower bound**, and the estimate of the true value is expressed as a range derived from the ratio to the last observed reading, never as a single confident number.

The corresponding trap on the Codex side — that `primary` and `secondary` are seats whose window lengths change with the plan, so the CLI records `window_minutes` and refuses to label the windows — is documented in [Output files](output.md#the-pitfall-primary-and-secondary-are-seats-not-window-lengths).

## Failure policy

Partial data beats no data, but silence is never acceptable.

- A line that will not parse as JSON is counted and skipped; the file and the run continue. The count and the offending files are reported.
- A broken line in the limits JSONL does not invalidate the other observations in it.
- A missing `limit.json` is not an error (there is simply no baseline, so `_windows.json` is skipped), but a malformed or wrong-version one is — otherwise "not configured" and "mistyped" look identical.
- A missing price table *is* an error, because the alternative is a month of `null` costs that looks like a pricing gap.
- During a plain `codex` run, a failed limits update is a warning: the monthly JSON has already been written and must not be lost. Under `codex limits` the same failure is an error.
- Error messages never include the keychain payload or the access token, because the launchd jobs write stderr to a log file.
