//! Codex のセッション JSONL を解析・集計する．
//!
//! Codex は同じ応答の利用量を `token_usage_record` と
//! `event_msg/token_count.last_token_usage` の両方に記録する．このモジュールでは
//! 前者だけを請求量として扱い，後者は利用制限の観測値として保存する．
//!
//! 利用制限の観測値の行き先 (長期記録の JSONL と vault の `_limits.json`) は
//! [`limits`] が受け持つ．

pub mod limits;

pub use crate::common::aggregation::Scope;
use crate::common::aggregation::{CacheClass, TokenUsage, UsageRecord};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap as Map;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub const PROVIDER: &str = "codex";

/// 1 つの «席» (primary / secondary) の観測値．
///
/// `window_minutes` は枠の長さ (300 = 5 時間，10080 = 週) だが，**どちらの席がどの長さかは
/// プランによって変わる**ので，ここでは意味を付けずに観測値のまま持つ．
/// 詳しくは [`limits`] のモジュール解説を参照．
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    pub used_percent: f64,
    pub window_minutes: u64,
    pub resets_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitsObservation {
    pub observed_at: DateTime<Utc>,
    pub limit_id: Option<String>,
    pub plan_type: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<Credits>,
    /// 観測した `rate_limits` を**丸ごと**持っておく．
    ///
    /// 上の欄は「いま画面が使うもの」だけを型にしたものなので，`limit_name` /
    /// `individual_limit` / `spend_control_reached` / `rate_limit_reached_type` は
    /// 落ちる (2026-09-10 時点ではすべて null)．**出どころのログは刈られるので，
    /// 型に無い欄はその時点で永久に失われる** — とくに «実際に上限に当たったか»
    /// を示す欄は，後から «あの週なぜ止まったのか» を調べる唯一の手がかりになる．
    /// 知らない欄も含めてそのまま長期記録へ落とすため，生のまま抱えておく．
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

/// 仕様上の「包含関係が壊れた値」を記録する警告．
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anomaly {
    CachedInputExceedsInput { input: u64, cached_input: u64 },
    ReasoningOutputExceedsOutput { output: u64, reasoning_output: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutcome {
    Record(Box<UsageRecord>),
    /// 異常値でもレコード自体は捨てず，原始値を保持する．
    Anomalous {
        record: Box<UsageRecord>,
        anomalies: Vec<Anomaly>,
    },
    RateLimits(RateLimitsObservation),
    /// 走査状態へ引き継ぐメタデータ．
    Metadata {
        session_id: Option<String>,
        model: Option<String>,
        scope: Option<Scope>,
    },
    Skip,
    Broken,
}

#[derive(Debug, Clone, Default)]
pub struct FileAggregation {
    pub records: Vec<UsageRecord>,
    /// このファイルで最も新しい観測値．`state.json` に持ち越す 1 点．
    pub rate_limits: Option<RateLimitsObservation>,
    /// このファイルに含まれる観測値すべて (現れた順)．
    ///
    /// `state.json` に全部を持たせると，1 ファイルに数十点あるので state が膨らむ．
    /// 一方で長期記録の JSONL には全部を残したい (1 セッション 1 点では折れ線が粗い)ので，
    /// «読んだそのときに JSONL へ積む» ためにこちらを別に持つ．
    pub rate_limits_all: Vec<RateLimitsObservation>,
    pub anomalies: Vec<Anomaly>,
    pub broken_lines: usize,
    pub total_lines: usize,
}

fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn string_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

fn string_at_map(v: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

fn scope_from_value(value: Option<&Value>) -> Option<Scope> {
    let value = value?.as_str()?.to_ascii_lowercase();
    match value.as_str() {
        "subagent" | "sub-agent" | "agent" | "child" => Some(Scope::Subagent),
        "main" | "user" | "cli" | "exec" | "vscode" => Some(Scope::Main),
        "unknown" => Some(Scope::Unknown),
        _ => None,
    }
}

fn scope_from_object(v: &Value) -> Option<Scope> {
    scope_from_value(v.get("scope"))
        .or_else(|| scope_from_value(v.get("thread_source")))
        .or_else(|| scope_from_value(v.get("source")))
}

fn scope_from_map(v: &serde_json::Map<String, Value>) -> Option<Scope> {
    scope_from_value(v.get("scope"))
        .or_else(|| scope_from_value(v.get("thread_source")))
        .or_else(|| scope_from_value(v.get("source")))
}

fn parse_timestamp(v: &Value, fallback: Option<&str>) -> Option<DateTime<Utc>> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .or(fallback)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_window(v: &Value) -> Option<RateLimitWindow> {
    Some(RateLimitWindow {
        used_percent: v.get("used_percent")?.as_f64()?,
        window_minutes: v.get("window_minutes")?.as_u64()?,
        resets_at: v.get("resets_at")?.as_i64()?,
    })
}

fn parse_rate_limits(v: &Value, observed_at: DateTime<Utc>) -> Option<RateLimitsObservation> {
    let limits = v.get("rate_limits")?;
    Some(RateLimitsObservation {
        observed_at,
        limit_id: string_at(limits, "limit_id"),
        plan_type: string_at(limits, "plan_type"),
        primary: limits.get("primary").and_then(parse_window),
        secondary: limits.get("secondary").and_then(parse_window),
        raw: Some(limits.clone()),
        credits: limits.get("credits").map(|c| Credits {
            has_credits: c
                .get("has_credits")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            unlimited: c.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
            balance: c.get("balance").map(|b| match b {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }),
        }),
    })
}

fn parse_record(
    root: &Value,
    model_hint: Option<&str>,
    scope_hint: Option<Scope>,
) -> Option<(UsageRecord, Vec<Anomaly>)> {
    let payload = root.get("payload")?.as_object()?;
    let usage = payload.get("usage")?.as_object()?;
    let event_id =
        string_at_map(payload, "response_id").or_else(|| string_at(root, "response_id"))?;
    let timestamp = parse_timestamp(root, None)?;
    let session_id = string_at_map(payload, "session_id")
        .or_else(|| string_at_map(payload, "thread_id"))
        .unwrap_or_else(|| "unknown".to_string());
    let model = string_at_map(payload, "model").or_else(|| model_hint.map(ToOwned::to_owned));
    let scope = scope_from_map(payload)
        .or(scope_hint)
        .unwrap_or(Scope::Unknown);
    let input = u64_at(&Value::Object(usage.clone()), "input_tokens");
    let cached_input = u64_at(&Value::Object(usage.clone()), "cached_input_tokens");
    let output = u64_at(&Value::Object(usage.clone()), "output_tokens");
    let reasoning_output = u64_at(&Value::Object(usage.clone()), "reasoning_output_tokens");
    let cache_write = u64_at(&Value::Object(usage.clone()), "cache_write_input_tokens");
    let mut cache_writes = BTreeMap::new();
    // Codex has one cache class. Keep a zero entry too, so consumers can distinguish
    // “the provider has no cache class” from “this response used zero cached writes”.
    cache_writes.insert(CacheClass::Standard, cache_write);
    let record = UsageRecord {
        provider: crate::common::aggregation::Provider::Codex,
        event_id,
        session_id,
        timestamp,
        model,
        scope,
        tokens: TokenUsage {
            input,
            cached_input,
            output,
            reasoning_output,
            cache_writes,
        },
    };
    let mut anomalies = Vec::new();
    if cached_input > input {
        anomalies.push(Anomaly::CachedInputExceedsInput {
            input,
            cached_input,
        });
    }
    if reasoning_output > output {
        anomalies.push(Anomaly::ReasoningOutputExceedsOutput {
            output,
            reasoning_output,
        });
    }
    Some((record, anomalies))
}

/// 1行を解析する．`model_hint` と `scope_hint` は直前の同一セッションの状態．
pub fn parse_line(line: &str, model_hint: Option<&str>, scope_hint: Option<Scope>) -> ParseOutcome {
    if line.trim().is_empty() {
        return ParseOutcome::Skip;
    }
    let root: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return ParseOutcome::Broken,
    };
    let ty = root.get("type").and_then(Value::as_str);
    match ty {
        Some("session_meta") => {
            let payload = root.get("payload").unwrap_or(&root);
            ParseOutcome::Metadata {
                session_id: string_at(payload, "session_id").or_else(|| string_at(payload, "id")),
                model: string_at(payload, "model"),
                scope: scope_from_object(payload),
            }
        }
        Some("turn_context") => {
            let payload = root.get("payload").unwrap_or(&root);
            ParseOutcome::Metadata {
                session_id: string_at(payload, "session_id")
                    .or_else(|| string_at(payload, "thread_id")),
                model: string_at(payload, "model"),
                scope: scope_from_object(payload),
            }
        }
        Some("token_usage_record") => match parse_record(&root, model_hint, scope_hint) {
            Some((record, anomalies)) if anomalies.is_empty() => {
                ParseOutcome::Record(Box::new(record))
            }
            Some((record, anomalies)) => ParseOutcome::Anomalous {
                record: Box::new(record),
                anomalies,
            },
            None => ParseOutcome::Skip,
        },
        Some("event_msg")
            if root.pointer("/payload/type").and_then(Value::as_str) == Some("token_count") =>
        {
            let observed_at = match parse_timestamp(&root, None) {
                Some(t) => t,
                None => return ParseOutcome::Skip,
            };
            parse_rate_limits(root.get("payload").unwrap_or(&root), observed_at)
                .map(ParseOutcome::RateLimits)
                .unwrap_or(ParseOutcome::Skip)
        }
        _ => ParseOutcome::Skip,
    }
}

/// JSONLファイルを走査する．壊れた行は警告件数にして他行を処理する．
pub fn aggregate_file(path: &Path) -> std::io::Result<FileAggregation> {
    let reader = BufReader::new(File::open(path)?);
    let mut out = FileAggregation::default();
    let mut model: Option<String> = None;
    let mut scope: Option<Scope> = None;
    let mut session_id: Option<String> = None;
    let mut seen = HashSet::new();
    for line in reader.lines() {
        out.total_lines += 1;
        let line = match line {
            Ok(line) => line,
            Err(_) => {
                out.broken_lines += 1;
                continue;
            }
        };
        match parse_line(&line, model.as_deref(), scope) {
            ParseOutcome::Metadata {
                session_id: id,
                model: next_model,
                scope: next_scope,
            } => {
                session_id = id.or(session_id);
                model = next_model.or(model);
                scope = next_scope.or(scope);
            }
            ParseOutcome::Record(mut record) => {
                if record.session_id == "unknown" {
                    if let Some(id) = &session_id {
                        record.session_id = id.clone();
                    }
                }
                if seen.insert(record.event_id.clone()) {
                    out.records.push(*record);
                }
            }
            ParseOutcome::Anomalous {
                mut record,
                anomalies,
            } => {
                if record.session_id == "unknown" {
                    if let Some(id) = &session_id {
                        record.session_id = id.clone();
                    }
                }
                out.anomalies.extend(anomalies);
                if seen.insert(record.event_id.clone()) {
                    out.records.push(*record);
                }
            }
            ParseOutcome::RateLimits(observation) => {
                if out
                    .rate_limits
                    .as_ref()
                    .map(|old| old.observed_at <= observation.observed_at)
                    .unwrap_or(true)
                {
                    out.rate_limits = Some(observation.clone());
                }
                out.rate_limits_all.push(observation);
            }
            ParseOutcome::Broken => out.broken_lines += 1,
            ParseOutcome::Skip => {}
        }
    }
    out.records.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then(a.event_id.cmp(&b.event_id))
    });
    Ok(out)
}

pub fn find_sessions(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
        })
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    paths.sort();
    paths
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileState {
    mtime_ns: u128,
    size: u64,
    missing: bool,
    records: Vec<UsageRecord>,
    rate_limits: Option<RateLimitsObservation>,
    /// このファイルの利用制限の観測値を **長期記録 (JSONL) へ落とし終えたか**．
    ///
    /// 月次集計の増分走査は «mtime と size が変わっていなければ読み飛ばす» が，
    /// 読み飛ばしたファイルからは `rate_limits_all` が取れず，state に残る最新 1 点
    /// しか JSONL へ渡らない．利用制限の記録を後から足したので，**それ以前に
    /// 走査済みのファイルはこの印を持たない** — 持たないものは «まだ落としていない»
    /// とみなして 1 度だけ読み直す．
    ///
    /// これが無いと，実際に 985 点あった観測のうち 321 点しか長期記録に入らなかった
    /// (2026-09-10 に実測)．元のログはいずれ刈られるので，取りこぼすと復元できない．
    #[serde(default)]
    limits_harvested: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct State {
    version: u32,
    files: Map<String, FileState>,
}

fn mtime(path: &Path) -> u128 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 1 回の走査の結果．月次集計と利用制限の両方をここから作る．
///
/// 走査を 1 か所にまとめてあるのは，利用制限の観測値が月次集計と **同じ行の並び**から
/// 取れるため．`codex` (操作なし) では追加の走査費用ゼロで利用制限も更新できる．
struct Scan {
    state: State,
    state_path: PathBuf,
    /// 今回読んだファイルの観測値すべてと，読み飛ばしたファイルについて state に
    /// 残っている最新 1 点を合わせたもの．重複は JSONL へ積むときに落とす．
    observations: Vec<RateLimitsObservation>,
    broken: usize,
}

fn scan(c: &crate::config::Config, args: &crate::cli::ProviderArgs) -> crate::Result<Scan> {
    let root = c.codex_sessions_dir();
    if !root.is_dir() {
        return Err(crate::Error::Message(format!(
            "Codex sessions のディレクトリがありません: {}",
            root.display()
        )));
    }
    let state_path = c.state_dir("codex").join("state.json");
    let mut state: State = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    state.version = 1;
    let paths = find_sessions(&root);
    let mut alive = std::collections::HashSet::new();
    let mut broken = 0;
    let mut observations: Vec<RateLimitsObservation> = Vec::new();
    for p in &paths {
        let key = p.to_string_lossy().to_string();
        alive.insert(key.clone());
        let meta = std::fs::metadata(p)?;
        let mt = mtime(p);
        let known = state.files.get(&key);
        let unchanged = known
            .map(|f| !f.missing && f.mtime_ns == mt && f.size == meta.len())
            .unwrap_or(false);
        // 中身が変わっていなくても，**利用制限をまだ長期記録へ落としていなければ
        // 読み直す**．集計そのものは同じ結果になるので無駄に見えるが，読み飛ばすと
        // そのファイルの観測値は永久に手に入らない (→ `FileState::limits_harvested`)．
        let harvested = known.map(|f| f.limits_harvested).unwrap_or(false);
        let fresh = !args.all && unchanged && harvested;
        if fresh {
            continue;
        }
        let fa = aggregate_file(p)?;
        broken += fa.broken_lines;
        observations.extend(fa.rate_limits_all);
        state.files.insert(
            key,
            FileState {
                mtime_ns: mt,
                size: meta.len(),
                missing: false,
                records: fa.records,
                rate_limits: fa.rate_limits,
                limits_harvested: true,
            },
        );
    }
    for (k, v) in state.files.iter_mut() {
        if !alive.contains(k) && !v.missing {
            v.missing = true;
        }
    }
    if args.forget_missing {
        state.files.retain(|_, v| !v.missing);
    }
    // 読み飛ばした (mtime も size も変わっていない) ファイルの分を state の 1 点で補う．
    // 通常はその観測値は既に JSONL に入っているので何も増えないが，JSONL を失った場合に
    // 直近の点だけは作り直せる．
    for f in state.files.values() {
        if let Some(o) = &f.rate_limits {
            observations.push(o.clone());
        }
    }
    Ok(Scan {
        state,
        state_path,
        observations,
        broken,
    })
}

/// 観測値を長期記録へ積み，vault の `_limits.json` を作り直す．
///
/// 失敗はここでは握り潰さず [`String`] で返す．月次集計の側で «警告にして続ける» か
/// «エラーにする» かを決められるようにするため — `codex limits` は利用制限だけが
/// 仕事なので失敗はエラー，`codex` は月次集計の結果を守るので警告にする．
fn update_limits(
    c: &crate::config::Config,
    observations: &[RateLimitsObservation],
    dry_run: bool,
) -> std::result::Result<(), String> {
    let jsonl_path = c.state_dir("codex").join(limits::JSONL_FILE);
    let stored = limits::read_jsonl(&jsonl_path)?;
    if stored.broken_lines > 0 {
        eprintln!(
            "[WARN] {} に観測値として読めない行が {} 行ありました (読み飛ばしました)",
            jsonl_path.display(),
            stored.broken_lines
        );
    }
    let fresh = limits::new_observations(&stored.observations, observations);
    let mut all = stored.observations;
    all.extend(fresh.iter().cloned());
    let out_path = c.codex_output_dir().join(limits::OUTPUT_FILE);

    if dry_run {
        println!(
            "--dry-run: {} へ {} 件の観測値を追記する予定でした (既存 {} 件)．",
            jsonl_path.display(),
            fresh.len(),
            all.len() - fresh.len()
        );
        match limits::build_output(&all, "dry-run", now_jst()) {
            Some(out) => println!(
                "--dry-run: {} を書く予定でした (最新観測 {} / plan_type {} / primary {} 分 / history {} 件)．",
                out_path.display(),
                out.observed_at,
                out.plan_type.as_deref().unwrap_or("不明"),
                out.rate_limits
                    .primary
                    .as_ref()
                    .map(|w| w.window_minutes.to_string())
                    .unwrap_or_else(|| "なし".into()),
                out.history.len()
            ),
            None => println!(
                "--dry-run: 観測値が 1 件も無いため {} は書きません．",
                out_path.display()
            ),
        }
        return Ok(());
    }

    limits::append_jsonl(&jsonl_path, &fresh)?;
    let now = now_jst();
    let updated_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    match limits::build_output(&all, &updated_at, now) {
        // 観測が 1 件も無いときは «0%» のファイルを置かない．
        // 「まだ観測できていない」と「使っていない」は別のことなので，混ぜない．
        None => println!(
            "Codex の利用制限の観測値が 1 件も無いため {} は書いていません．",
            out_path.display()
        ),
        Some(out) => {
            let path = limits::write(&c.codex_output_dir(), &out)
                .map_err(|e| format!("{} を書けません ({e})", out_path.display()))?;
            println!("wrote {}", path.display());
        }
    }
    Ok(())
}

fn now_jst() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::Utc::now().with_timezone(&crate::common::date::jst())
}

fn write_state(state: &State, state_path: &Path) -> crate::Result<()> {
    std::fs::create_dir_all(state_path.parent().unwrap())?;
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|e| crate::Error::Message(e.to_string()))?;
    crate::common::atomic_write::write_atomic(state_path, &bytes)?;
    Ok(())
}

pub fn run(args: crate::cli::ProviderArgs) -> crate::Result<()> {
    let c = crate::config::Config::from_env();
    if !args.dry_run {
        c.migrate_legacy_output("codex")
            .map_err(|e| crate::Error::Message(format!("旧出力の移行に失敗しました: {e}")))?;
    }
    // 単一 variant でも `match` で受ける．操作が増えたときに «未知の操作» へ黙って
    // 落ちるのではなく，ここでコンパイルエラーにして気づけるようにするため．
    let limits_only = match args.command.as_ref() {
        None => false,
        Some(crate::cli::ProviderOperation::Limits) => true,
    };
    if !limits_only {
        if let Some(m) = args.month.as_deref() {
            if !crate::config::Config::validate_month(m) {
                return Err(crate::Error::Message(
                    "--month は YYYY-MM 形式で指定してください".into(),
                ));
            }
        }
    }

    let scan = scan(&c, &args)?;
    let broken = scan.broken;
    if broken > 0 {
        eprintln!("[WARN] JSON として読めない行が {} 行ありました", broken);
    }

    if limits_only {
        // 利用制限だけを更新する．月次 JSON には触れない．
        update_limits(&c, &scan.observations, args.dry_run).map_err(crate::Error::Message)?;
        if args.dry_run {
            println!("--dry-run のため何も書き出していません (state も更新していません)．");
            return Ok(());
        }
        return write_state(&scan.state, &scan.state_path);
    }

    let state = scan.state;
    let state_path = scan.state_path;
    let mut records = Vec::new();
    for f in state.files.values() {
        records.extend(f.records.clone());
    }
    let mut days: BTreeMap<String, BTreeMap<(String, Scope), (u64, u64, u64, u64, u64, u64)>> =
        BTreeMap::new();
    for r in records {
        let d = r.timestamp.format("%Y-%m-%d").to_string();
        let m = d[..7].to_string();
        if args.month.as_deref().map(|x| x != m).unwrap_or(false) {
            continue;
        }
        let model = r.model.unwrap_or_else(|| "unknown".into());
        let e = days
            .entry(d)
            .or_default()
            .entry((model, r.scope))
            .or_default();
        e.0 += 1;
        e.1 += r.tokens.input;
        e.2 += r.tokens.cached_input;
        e.3 += r.tokens.output;
        e.4 += r.tokens.reasoning_output;
        e.5 += r.tokens.cache_writes.values().sum::<u64>();
    }
    if args.dry_run {
        if let Err(e) = update_limits(&c, &scan.observations, true) {
            eprintln!("[WARN] 利用制限の更新に失敗しました: {e}");
        }
        println!("--dry-run のため何も書き出していません (state も更新していません)．");
        return Ok(());
    }
    // **月ごとに 1 ファイルへ分けて書く．**
    //
    // 以前はここで «`--month` が無ければいちばん古い月» を 1 つ選び，その名前の
    // ファイルへ **全期間の日** を書いていた．結果として `2026-08.json` の中に
    // 2026-09 の日が入り，`2026-09.json` は作られないという食い違いが起きる
    // (2026-09-10 に実測で確認)．日次ジョブは `--month` 無しで回るので，
    // 放っておくと毎朝この形の出力が出る．
    //
    // `--month` は «どの月を書くか» の絞り込みであって，出力を 1 ファイルに
    // 畳む指示ではない．レコード側は既に絞ってあるので，ここでは日付から月を
    // 引き直して束ね直すだけでよい．
    let mut by_month: BTreeMap<String, BTreeMap<String, _>> = BTreeMap::new();
    for (d, es) in days {
        by_month.entry(d[..7].to_string()).or_default().insert(d, es);
    }
    let dir = c.codex_output_dir();
    if !by_month.is_empty() {
        std::fs::create_dir_all(&dir)?;
    }
    for (month, month_days) in by_month {
        let mut out = serde_json::json!({"version":1,"provider":"codex","month":month,"updated_at":chrono::Utc::now().to_rfc3339(),"days":[]});
        let arr = out["days"].as_array_mut().unwrap();
        for (d, es) in month_days {
            let mut ent = Vec::new();
            for ((model, scope), (req, input, cached, output, reasoning, writes)) in es {
                ent.push(serde_json::json!({"model":model,"scope":scope,"requests":req,"input":input,"cached_input":cached,"non_cached_input":input.saturating_sub(cached),"output":output,"reasoning_output":reasoning,"normal_output":output.saturating_sub(reasoning),"cache_write_input":writes}));
            }
            arr.push(serde_json::json!({"date":d,"entries":ent}));
        }
        let path = dir.join(format!("{month}.json"));
        let bytes =
            serde_json::to_vec_pretty(&out).map_err(|e| crate::Error::Message(e.to_string()))?;
        crate::common::atomic_write::write_atomic(&path, &bytes)?;
        println!("wrote {}", path.display());
    }
    // 月次 JSON を書いた後に回す．利用制限の更新が失敗しても，既に書けている
    // 月次集計の結果は壊さない — こちらは警告に留めて先へ進む．
    if let Err(e) = update_limits(&c, &scan.observations, false) {
        eprintln!("[WARN] 利用制限の更新に失敗しました: {e}");
    }
    write_state(&state, &state_path)
}

impl UsageRecord {
    /// 表示用の非キャッシュ入力．原始値を変更しない．
    pub fn non_cached_input(&self) -> Option<u64> {
        self.tokens.input.checked_sub(self.tokens.cached_input)
    }

    /// 表示用の通常出力．原始値を変更しない．
    pub fn normal_output(&self) -> Option<u64> {
        self.tokens.output.checked_sub(self.tokens.reasoning_output)
    }
}
