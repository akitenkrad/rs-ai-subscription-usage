**English** | [日本語](usecases.ja.md)

# Use cases

## What this tool is for

Both Claude Code and Codex CLI leave a per-session JSONL log on disk. Each log line carries the token usage of one response, and — for Codex — the rate-limit percentages the server reported at that moment. Those logs are pruned: Claude Code deletes transcripts under `~/.claude/projects` after about 30 days, and Codex eventually clears out `~/.codex/sessions`. Anything you did not extract before the prune is gone for good.

`ai-subscription-usage` walks those logs, folds them into monthly totals, and keeps a permanent append-only record of every usage-limit observation. The output is plain JSON in an Obsidian vault, so a dashboard inside the vault can chart it without any server.

### Questions it answers

- How many tokens did each model consume, per day, split between the main conversation and subagents?
- What would this month's traffic have cost at published API prices?
- How far into the 5-hour and weekly usage limits am I, and how did that curve look over the past weeks?
- Which share of the spend comes from subagents rather than from what I typed?

### Questions it deliberately does not answer

- **"What is my combined AI spend?"** The tool never adds Claude and Codex together. They are separate subscriptions with separate limits; a single sum would be a number with no meaning attached to it.
- **"What will I be billed?"** The dollar figures are a *reference amount converted at API list prices*, not an invoice. A subscription is a flat fee; this is what the same traffic would have cost through the API. Models with no entry in the price table are reported as `null`, never as `$0`.
- **"How full is my limit, computed from tokens?"** Consumption percentages are never estimated from token counts. Only observed values count — Codex's own `rate_limits` payload, or Anthropic's usage endpoint. Where nothing has been observed, the output says nothing rather than `0%`.

## Typical uses

### Daily aggregation into a dashboard

Run the aggregation once a day and the limit fetch once an hour, via launchd (see [Operations](operations.md)). The vault then always holds a current `YYYY-MM.json` per provider plus a `_limits.json` thinned to one point per day, which is cheap enough for a dashboard to read on every open.

### Reconstructing history after the logs are pruned

The state file and the limits JSONL are not caches — they are the only long-term record. Once a transcript is deleted, its totals survive only because they were folded into `state.json`, and a usage-limit reading survives only because it was appended to `limits.jsonl`. Both live under `~/.local/share/ai-subscription-usage/`, and both belong in your backups. See [Architecture](architecture.md#the-state-files-are-the-record-not-a-cache).

### Checking a scan before it writes

`--dry-run` scans and prints the summary without touching the state file, the monthly JSON, or the limits record. It is the safe way to see what a new machine or a restored backup would produce.

### Investigating a week that hit the wall

`_limits.json` keeps 60 days of daily peaks for the dashboard, but the JSONL behind it keeps every observation, along with the raw `rate_limits` object exactly as the log reported it — including fields the tool does not model, such as whether a limit was actually reached. That raw copy is usually the only remaining evidence of why a particular week stopped early.

## Scope and assumptions

- macOS. Claude's usage-limit fetch reads the OAuth token from the login keychain through the `security` command, and scheduling is done with launchd agents.
- Timestamps are folded into days using a fixed `+09:00` (JST) offset. Days are cut at JST midnight, not UTC.
- Both providers' logs are read directly from their default locations (`~/.claude/projects`, `~/.codex/sessions`). Nothing is sent anywhere except the one authenticated request to Anthropic's usage endpoint made by `claude limits`.
