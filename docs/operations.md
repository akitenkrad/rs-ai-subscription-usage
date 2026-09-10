**English** | [日本語](operations.ja.md)

# Operations

## Requirements

- Rust toolchain (edition 2021) to build.
- macOS. `claude limits` reads the OAuth token from the login keychain through `security find-generic-password`, and scheduling uses launchd.
- Claude Code and/or Codex CLI installed, having written logs to `~/.claude/projects` and `~/.codex/sessions`.
- An Obsidian vault to write into, named by `OBSIDIAN_VAULT`.

## Build and test

```bash
cargo build --release
make test          # cargo test
```

## Install the scheduled jobs

```bash
make deploy
```

`deploy` installs the binary with `cargo install --path .`, copies the four launchd property lists into `~/Library/LaunchAgents/`, and re-bootstraps each one (`launchctl bootout` — ignoring the failure when it was not loaded — followed by `launchctl bootstrap`), so re-running it after a change is safe.

| Job | Command | Schedule |
|---|---|---|
| `…ai-subscription-usage` | `claude` | daily at 05:10 |
| `…ai-subscription-usage-codex` | `codex` | daily at 05:15 |
| `…ai-subscription-usage-limits` | `claude limits` | every hour, also at load |
| `…ai-subscription-usage-codex-limits` | `codex limits` | every hour, also at load |

The two aggregation jobs run once a day, staggered by five minutes. The two limit jobs run hourly, because a limit reading cannot be recovered after the fact: Anthropic's endpoint only reports the present moment, and Codex's observations vanish when its session logs are pruned. Hourly sampling is what gives the charts their resolution.

Each job writes stdout and stderr to `~/Library/Logs/obsidian-ai-subscription-usage*.log` and `*.err`.

launchd requires absolute paths, so the bundled property lists spell out the full path to the installed binary and to the log files, and they are written for the author's account. **Edit the paths in the `.plist` files for your own user before running `make deploy`.** They also do not set `OBSIDIAN_VAULT`, so a launchd run falls back to the compiled-in default vault path; add an `EnvironmentVariables` dictionary if your vault lives elsewhere.

Check what is loaded, or run one job by hand:

```bash
launchctl list | grep ai-subscription-usage
launchctl kickstart -k "gui/$(id -u)/com.akitenkrad.obsidian.ai-subscription-usage-codex-limits"
```

## Backups

Back up `~/.local/share/ai-subscription-usage/` — both providers' `state.json` and `limits.jsonl`.

These are not caches. Source logs are pruned after roughly a month, so the state file holds the only surviving copy of older daily totals, and the JSONL holds the only copy of every usage-limit reading ever taken. The vault output can always be rebuilt from them; they cannot be rebuilt from anything.

The vault files themselves need no special treatment beyond whatever backs up the vault.

## Troubleshooting

**`価格表を読めません` (cannot read the price table).** `pricing.json` is missing from `<vault>/_logs/_ai-subscription-usage/claude/`. Create it — see [Output files](output.md#claudepricingjson-required). This is a hard error by design: the alternative is a month of `null` costs that looks like a pricing gap rather than a missing file.

**`transcript のディレクトリがありません` / `Codex sessions のディレクトリがありません`.** `~/.claude/projects` or `~/.codex/sessions` does not exist, or `HOME` points somewhere unexpected.

**`キーチェーンから認証情報を読めません` (cannot read credentials from the keychain).** The `Claude Code-credentials` keychain item was not found, or access was denied. Confirm you are logged in to Claude Code, and allow keychain access when macOS asks. Note that the prompt appears on the terminal session, so the first run of `claude limits` is best done by hand rather than from launchd.

**`Claudeの認証情報の有効期限が切れています` (credentials expired).** Re-authenticate in Claude Code; the token is refreshed there, not here.

**`使用制限 API が HTTP 401 を返しました`.** The token was rejected. Re-authenticating usually resolves it. If the keychain item has been renamed by a newer Claude Code release, the read fails earlier, with the keychain message instead.

**`error: unexpected argument '--dry-run' found`.** The flag was placed after the operation. Flags belong to the provider: write `codex --dry-run limits`, not `codex limits --dry-run`. See [CLI reference](cli.md#flags-come-before-the-subcommand).

**`[WARN] JSON として読めない行が N 行ありました`.** Some log lines were not valid JSON; they were counted and skipped, and everything else was aggregated normally. Truncated final lines from an interrupted session are the usual cause.

**`_windows.json` is not being written.** It requires `limit.json` to exist in the Claude output directory *and* to contain at least one observation. Without it the file is skipped silently — that is not an error.

**`_limits.json` is not being written for Codex.** No rate-limit observation has been collected yet. The tool refuses to write a `0%` file, because that would be indistinguishable from genuinely unused. Run Codex once and re-run `codex limits`.

**The state version changed and the run re-read everything.** Expected after an upgrade that needs finer data. Whatever the pruning already removed cannot be backfilled, so figures that depend on the newly added dimension should not be quoted for older periods.
