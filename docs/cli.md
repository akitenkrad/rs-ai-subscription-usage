**English** | [日本語](cli.ja.md)

# CLI reference

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

A provider is required; running the binary with no arguments exits with a usage error.

## Flags come before the subcommand

Each provider takes both options and an optional operation, and the options are defined on the provider, not on the operation. So this works:

```bash
ai-subscription-usage codex --dry-run limits
```

and this does not:

```bash
ai-subscription-usage codex limits --dry-run
# error: unexpected argument '--dry-run' found
```

`limits` is a subcommand of `codex`, and once the parser has descended into it, only `limits`' own options are in scope — and it has none. Anything you want to apply to the provider must be written before the operation name. Every example in these docs follows that order.

## `claude`

```bash
ai-subscription-usage claude [OPTIONS] [limits]
```

With no operation, it scans `~/.claude/projects/**/*.jsonl`, folds each response into (date, hour, model, scope) buckets, and writes one `YYYY-MM.json` per month into the vault. It also writes `_windows.json` if — and only if — a `limit.json` with at least one observation is present in the output directory.

A price table is mandatory: if `pricing.json` is missing or unreadable, the run fails instead of silently reporting every cost as `null`.

### Options

| Option | Effect |
|---|---|
| `--all` | Re-read every transcript that still exists, ignoring the `mtime`/`size` freshness check. Totals for transcripts that have already been deleted are **not** discarded. |
| `--month <YYYY-MM>` | Write only that month's file. The console summary still lists every month. If the month has no data, it says so. |
| `--dry-run` | Scan and print the summary; write nothing at all — not the monthly JSON, not the state file, not the legacy-layout migration. |
| `--forget-missing` | Permanently drop the aggregated totals of transcripts that no longer exist. **Irreversible**: those days cannot be recomputed, because the source transcripts are gone. |

### `claude limits`

```bash
ai-subscription-usage claude limits
```

Reads the OAuth access token from the login keychain (item `Claude Code-credentials`, via `security find-generic-password`), calls `GET https://api.anthropic.com/api/oauth/usage` once, appends the raw response to the long-term JSONL, and rebuilds `_limits.json` from the whole JSONL. If the access token has expired but a nonempty refresh token has a future or unknown expiry, the command treats this as temporary: it prints the expiration time (JST, RFC3339 to seconds) and instructions to start Claude Code on stdout, skips the API request, and exits successfully (`0`). The JSONL and previous `_limits.json` remain unchanged. If the refresh token is missing, empty, or expired, the command prints the expiration time and instructions to log in again with `/login` on stderr and exits with an error (`1`).

The percentages this returns cover the **whole account** — claude.ai and Cowork included — not just Claude Code. That caveat is written into the output file itself so a reader of the dashboard cannot miss it.

This operation takes no options of its own.

## `codex`

```bash
ai-subscription-usage codex [OPTIONS] [limits]
```

With no operation, it scans `~/.codex/sessions/**/rollout-*.jsonl`, aggregates the billable token usage, writes a monthly JSON, and — in the same pass — harvests every rate-limit observation into the long-term record and rebuilds `_limits.json`. Doing both from one scan is deliberate: the two kinds of data sit on the same lines, so the limits cost nothing extra to collect.

If the limits update fails during a plain `codex` run, it is reported as a warning and the monthly JSON is still written. Under `codex limits` the same failure is an error, because there the limits *are* the job.

One file is written per calendar month, named `YYYY-MM.json`, and each file contains only the days belonging to that month. `--month` narrows *which* months are written; it does not change how they are named or grouped.

### Options

The same four flags as `claude`, with provider-specific detail:

| Option | Effect |
|---|---|
| `--all` | Re-read every session log rather than only those whose `mtime`/`size` changed. |
| `--month <YYYY-MM>` | Restrict the run to that month only. |
| `--dry-run` | Scan and report what would be written — including how many new limit observations would be appended — without writing anything. |
| `--forget-missing` | Drop the state entries of session logs that no longer exist, discarding their totals. |

### `codex limits`

```bash
ai-subscription-usage codex limits
```

Scans the session logs the same way, but updates only the usage-limit record: the long-term JSONL and `_limits.json`. The monthly JSON is left untouched. This is what the hourly job runs.

Note that Codex's limit values are *observations left in the logs*, not a live API call — running this more often than Codex writes new lines will not produce new points.

## `all`

Reserved. Invoking it exits with an error saying it is not implemented; use `claude` and `codex` separately.

## Environment

| Variable | Meaning | Default |
|---|---|---|
| `OBSIDIAN_VAULT` | Vault root. Everything is written under `<vault>/_logs/_ai-subscription-usage/`. | The author's vault path — **set this explicitly.** |
| `HOME` | Where the source logs and the long-term record live: `~/.claude/projects`, `~/.codex/sessions`, `~/.local/share/ai-subscription-usage/`. | `/` if unset |

## Exit status and diagnostics

- `0` — the run completed. Each file written is announced as `wrote <path>`.
- `1` — a run-time error, printed as `[ERROR] <message>` on stderr: missing price table, missing source directory, an expired access token with no usable refresh token, an HTTP failure, a malformed `limit.json`.
- `2` — clap could not parse the command line (for example, a flag placed after the operation).

Warnings go to stderr as `[WARN]` and never abort the run: lines that are not valid JSON are counted and skipped, a state file at an unknown version is reported before starting from empty, and a failed limits update during aggregation is logged while the monthly JSON still gets written.
