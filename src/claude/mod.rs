pub mod aggregate;
pub mod discover;
pub mod limit;
pub mod limits_api;
pub mod output;
pub mod pricing;
pub mod transcript;
pub mod windows;

pub use transcript::Scope;

use crate::claude::output::MonthOutput;
use crate::common::date::jst;
use crate::common::state::{legacy_state_path, migrate_state, State};
use crate::config::Config;
use crate::{Error, Result};
use chrono::{SecondsFormat, Utc};

/// Claude の transcript を走査し，月次JSONと増分stateを更新する．
///
/// `--dry-run` では入力の走査と集計だけを行い，state，月次JSON，state migrationを
/// 含むファイル書き込みは一切行わない．通常実行では走査後に欠落transcriptを
/// `missing` として保持し，`--forget-missing` 指定時だけ破棄する．
pub fn run(args: crate::cli::ProviderArgs) -> Result<()> {
    let config = Config::from_env();
    let output_dir = config.claude_output_dir();
    if !args.dry_run {
        let _ = config
            .migrate_legacy_output("claude")
            .map_err(|e| Error::Message(format!("旧出力の移行に失敗しました: {e}")))?;
    }

    if matches!(
        args.command.as_ref(),
        Some(crate::cli::ProviderOperation::Limits)
    ) {
        return run_limits(&config, &output_dir);
    }
    if args.command.is_some() {
        return Err(Error::Message("未知のClaude操作です".into()));
    }
    if let Some(month) = args.month.as_deref() {
        if !Config::validate_month(month) {
            return Err(Error::Message(format!(
                "--month は YYYY-MM 形式で指定してください (指定値: {month})"
            )));
        }
    }

    let pricing_path = Config::pricing_path(&output_dir);
    let pricing = pricing::Pricing::load(&pricing_path).map_err(Error::Message)?;
    let projects_dir = config.claude_projects_dir();
    if !projects_dir.is_dir() {
        return Err(Error::Message(format!(
            "transcript のディレクトリがありません: {}",
            projects_dir.display()
        )));
    }

    let state_path = config.state_dir("claude").join("state.json");
    // migration はファイルを変更するため dry-run では行わない．
    if !args.dry_run {
        migrate_state(&legacy_state_path(&config.home), &state_path).map_err(Error::Message)?;
    }

    let loaded = State::load(&state_path);
    let mut state = loaded.state;
    let force_reread = args.all || loaded.needs_full_rescan;
    let paths = discover::find(&projects_dir);
    let now = Utc::now().with_timezone(&jst());
    let today = now.format("%Y-%m-%d").to_string();
    let stats = discover::scan_into(&paths, &mut state, force_reread, &today);

    let mut forgotten = 0;
    if args.forget_missing {
        forgotten = state.forget_missing();
    }

    if stats.broken_lines > 0 {
        eprintln!(
            "[WARN] JSON として読めない行が {} 行ありました:",
            stats.broken_lines
        );
        for file in stats.broken_files.iter().take(20) {
            eprintln!("       - {file}");
        }
        if stats.broken_files.len() > 20 {
            eprintln!("       ... 他 {} ファイル", stats.broken_files.len() - 20);
        }
    }

    let updated_at = Utc::now()
        .with_timezone(&jst())
        .to_rfc3339_opts(SecondsFormat::Secs, false);
    let months = output::build_months(state.all_entries(), &pricing, &updated_at);
    let targets: Vec<&MonthOutput> = months
        .values()
        .filter(|month| {
            args.month
                .as_deref()
                .map(|m| m == month.month)
                .unwrap_or(true)
        })
        .collect();

    print_summary(
        &stats,
        &state,
        &state_path,
        forgotten,
        &months,
        args.month.as_deref(),
    );

    if args.dry_run {
        println!();
        println!("--dry-run のため何も書き出していません (state も更新していません)．");
        return Ok(());
    }

    for month in targets {
        let path = output::write_month(&output_dir, month).map_err(|e| {
            Error::Message(format!("月次 JSON を書けません: {} ({e})", month.month))
        })?;
        println!("wrote {}", path.display());
    }

    if let Some(limit) =
        limit::Limit::load(&output_dir.join("limit.json")).map_err(Error::Message)?
    {
        let entries: Vec<_> = state.all_entries().cloned().collect();
        let prompts: Vec<_> = state.all_prompts().cloned().collect();
        let now_naive = now.naive_local();
        if let Some(windows) = windows::build_opt(
            entries.iter(),
            &prompts,
            &pricing,
            Some(&limit),
            now_naive,
            &updated_at,
        ) {
            let path = windows::write(&output_dir, &windows)
                .map_err(|e| Error::Message(format!("_windows.json を書けません ({e})")))?;
            println!("wrote {}", path.display());
        }
    }
    state.save(&state_path).map_err(|e| {
        Error::Message(format!(
            "state を書けません: {} ({e})",
            state_path.display()
        ))
    })?;
    Ok(())
}

fn run_limits(config: &Config, output_dir: &std::path::Path) -> Result<()> {
    let now = Utc::now();
    let credentials = limits_api::read_keychain().map_err(Error::Message)?;
    if credentials.is_expired(now.timestamp_millis()) {
        let expired_at = credentials
            .expires_at_ms
            .and_then(chrono::DateTime::<Utc>::from_timestamp_millis)
            .map(|at| {
                at.with_timezone(&jst())
                    .to_rfc3339_opts(SecondsFormat::Secs, false)
            })
            .unwrap_or_else(|| "失効時刻不明".to_string());
        if credentials.can_refresh(now.timestamp_millis()) {
            println!(
                "アクセストークンが失効しています ({expired_at})．Claude Code を起動すると更新されます．今回の実測取得は見送り，前回の _limits.json をそのまま残します．"
            );
            return Ok(());
        }
        return Err(Error::Message(format!(
            "Claudeの認証情報の有効期限が切れています ({expired_at})．リフレッシュトークンも使えないため，Claude Code を起動して /login し直してください．"
        )));
    }
    let fetched_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let record =
        limits_api::fetch(&credentials.access_token, &fetched_at).map_err(Error::Message)?;
    // state.json と同じ新CLI専用の永続領域に置く．旧CLIの履歴は暗黙に混ぜない．
    let jsonl_path = config.state_dir("claude").join("limits.jsonl");
    limits_api::append_jsonl(&jsonl_path, &record).map_err(Error::Message)?;
    let history = limits_api::read_jsonl(&jsonl_path).map_err(Error::Message)?;
    let now_jst = now.with_timezone(&jst());
    let output = limits_api::build_output(
        &history.records,
        &now_jst.to_rfc3339_opts(SecondsFormat::Secs, false),
        now_jst,
    );
    let path = limits_api::write(output_dir, &output)
        .map_err(|e| Error::Message(format!("_limits.json を書けません ({e})")))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn print_summary(
    stats: &discover::ScanStats,
    state: &State,
    state_path: &std::path::Path,
    forgotten: usize,
    months: &std::collections::BTreeMap<String, MonthOutput>,
    selected_month: Option<&str>,
) {
    println!(
        "走査ファイル数: {} (読み込み {} / state 再利用 {})",
        stats.found, stats.read, stats.reused
    );
    println!("読み込み行数  : {}", stats.lines);
    println!(
        "state         : {} ({} ファイル / うち transcript 消滅済み {})",
        state_path.display(),
        state.files.len(),
        state.missing_count()
    );
    if forgotten > 0 {
        println!("  --forget-missing により {forgotten} ファイルの集計を捨てました (復元不可)");
    } else if stats.newly_missing > 0 {
        println!(
            "  今回新たに {} ファイルの transcript が消えていました (集計は保持しています)",
            stats.newly_missing
        );
    }

    println!();
    println!(
        "{:<9} {:>12} {:>16} {:>12}",
        "month", "requests", "tokens", "cost(USD)"
    );
    println!("{}", "-".repeat(53));
    let mut total_tokens = 0u64;
    let mut total_cost = 0.0;
    for month in months.values() {
        let (requests, tokens, cost) = month_totals(month);
        total_tokens += tokens;
        total_cost += cost;
        println!(
            "{:<9} {:>12} {:>16} {:>12.2}",
            month.month, requests, tokens, cost
        );
    }
    println!("{}", "-".repeat(53));
    println!(
        "{:<9} {:>12} {:>16} {:>12.2}",
        "total", "", total_tokens, total_cost
    );
    if let Some(month) = selected_month {
        if !months.contains_key(month) {
            println!();
            println!("{month} のデータはありませんでした．");
        }
    }
}

fn month_totals(month: &MonthOutput) -> (u64, u64, f64) {
    let mut requests = 0;
    let mut tokens = 0;
    let mut cost = 0.0;
    for day in &month.days {
        for entry in &day.entries {
            requests += entry.requests;
            tokens += entry.input
                + entry.output
                + entry.cache_write_5m
                + entry.cache_write_1h
                + entry.cache_read;
            cost += entry.cost_usd.unwrap_or(0.0);
        }
    }
    (requests, tokens, cost)
}
