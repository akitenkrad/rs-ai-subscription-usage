//! Codex の利用制限 (rate limits) を長期記録し，ダッシュボードが読む `_limits.json` を作る．
//!
//! 観測値そのものの取り出しは [`super::parse_rate_limits`] が既に行っている
//! (`event_msg` / `token_count` の `payload.rate_limits`)．このモジュールは，
//! **取り出した観測値をどこに，どういう形で残すか**だけを受け持つ．
//!
//! # limits.jsonl は唯一の長期記録
//!
//! 観測値の出どころである `~/.codex/sessions/**/rollout-*.jsonl` は，Codex 自身が
//! いずれ刈る／消す．そこだけを正にすると，**過去の観測点は二度と復元できない**
//! (セッションのログが消えた時点で，その期間の消化率は世界のどこにも残らない)．
//! そのため観測値は `~/.local/share/ai-subscription-usage/codex/limits.jsonl` へ
//! 追記のみで積む．Claude 側の [`crate::claude::limits_api`] が公式 API の応答に対して
//! 同じことをしているのと同じ考え方で，`state.json` と同様にバックアップの対象に含めること．
//!
//! JSONL 側は **期間で絞らない**．vault へ出す `_limits.json` の `history` だけを
//! [`HISTORY_DAYS`] 日ぶんに切る (毎回大きくなるファイルを Obsidian Sync と Emera に
//! 読ませたくないため)．遡りたければ JSONL を見る．
//!
//! # 枠の意味はここでは決めない
//!
//! `primary` / `secondary` は «席» であって «枠の長さ» ではない．実ログを数えると，
//! 同じ Codex でもプランによって席と長さの対応が変わる:
//!
//! | plan_type | primary.window_minutes | secondary |
//! |---|---|---|
//! | plus | 10080 (週次) | null |
//! | plus | 300 (5 時間) | 10080 (週次) |
//! | prolite | 10080 (週次) | null |
//!
//! つまり «primary は 5 時間枠» と決め打つと，5 時間枠を持たないプランでは
//! 週次枠を 5 時間枠として表示してしまう．そこで **CLI は枠の意味を解釈せず**，
//! `window_minutes` を含めて観測値をそのまま記録する．«5 時間枠» / «週次枠» の
//! ラベル付けは，長さを見て判定する画面側の仕事である．
//! このモジュールに 300 や 10080 の分岐を書かないこと．

use super::{Credits, RateLimitWindow, RateLimitsObservation};
use chrono::{DateTime, Duration, FixedOffset, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 長期記録のファイル名 (`state_dir("codex")` の下)．
pub const JSONL_FILE: &str = "limits.jsonl";
/// vault に出すファイル名．
pub const OUTPUT_FILE: &str = "_limits.json";
/// `_limits.json` の `history` に残す日数．JSONL 側は全期間を持ったままにする．
pub const HISTORY_DAYS: i64 = 60;
/// 観測値の出どころ．画面から «この数字はどこから来たのか» を辿れるようにする．
pub const SOURCE: &str = "~/.codex/sessions/**/rollout-*.jsonl (event_msg/token_count.rate_limits)";
/// 出力に添える注意書き．画面だけを見た人が数字を取り違えないよう，
/// ダッシュボードが読むファイル自身に書いておく．
pub const NOTE: &str = "アカウント全体の値であり，Codex CLI だけの分ではない．used_percent はトークン数から推定した値ではなく，Codex のログに含まれる観測値そのもの．primary / secondary は席であって枠の長さではないので，5 時間枠か週次枠かは window_minutes で判定すること．";

/// `~/.local/share/ai-subscription-usage/codex/limits.jsonl`．
pub fn default_jsonl_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("ai-subscription-usage")
        .join("codex")
        .join(JSONL_FILE)
}

// ── JSONL の読み書き ────────────────────────────────────────

/// JSONL の読み込み結果．
#[derive(Debug, Default)]
pub struct Jsonl {
    pub observations: Vec<RateLimitsObservation>,
    /// 観測値として読めなかった行数．読み飛ばした分．
    pub broken_lines: usize,
}

/// JSONL を全部読む．壊れた行は読み飛ばして数える．
///
/// 1 行が壊れていても他の観測点は生きているので，全体をエラーにしない．
/// ファイルがまだ無い場合も空として扱う (初回実行)．
pub fn read_jsonl(path: &Path) -> Result<Jsonl, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Jsonl::default()),
        Err(e) => return Err(format!("{} を読めません ({e})", path.display())),
    };
    let mut out = Jsonl::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<RateLimitsObservation>(line) {
            Ok(v) => out.observations.push(v),
            Err(_) => out.broken_lines += 1,
        }
    }
    Ok(out)
}

/// `incoming` のうち JSONL にまだ無いものだけを，古い順に返す．
///
/// 同一性は **`observed_at`** で見る．同じセッションログを何度走査しても
/// 同じ時刻の観測値しか出てこないので，これで «走査するたびに行が増える» のを止められる．
/// (走査は増分だが `--all` や state の破棄で全走査になり得るため，冪等性は
/// state ではなくこちら側で担保する．)
pub fn new_observations(
    existing: &[RateLimitsObservation],
    incoming: &[RateLimitsObservation],
) -> Vec<RateLimitsObservation> {
    let mut seen: BTreeSet<DateTime<Utc>> = existing.iter().map(|o| o.observed_at).collect();
    let mut fresh: Vec<RateLimitsObservation> = incoming
        .iter()
        .filter(|o| seen.insert(o.observed_at))
        .cloned()
        .collect();
    fresh.sort_by_key(|o| o.observed_at);
    fresh
}

/// 観測値を追記する．既存の行には触れない．
///
/// **1 行ずつ «完全な 1 行» を組み立ててから書く．** 途中で失敗しても壊れた行を残さないため．
/// 戻り値は実際に書いた行数．
pub fn append_jsonl(path: &Path, observations: &[RateLimitsObservation]) -> Result<usize, String> {
    if observations.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("{} を作れません ({e})", parent.display()))?;
    }
    let mut buf: Vec<u8> = Vec::new();
    if needs_leading_newline(path) {
        // 前回の追記が途中で切れていた場合に，新しい行をその尻に繋げない．
        // 繋げると «壊れた 1 行» が «壊れた 2 行» になる．
        buf.push(b'\n');
    }
    for o in observations {
        serde_json::to_writer(&mut buf, o).map_err(|e| format!("JSON にできません ({e})"))?;
        buf.push(b'\n');
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{} を開けません ({e})", path.display()))?;
    f.write_all(&buf)
        .map_err(|e| format!("{} に書けません ({e})", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("{} を同期できません ({e})", path.display()))?;
    Ok(observations.len())
}

/// 既存ファイルの末尾が改行で終わっていなければ true．
fn needs_leading_newline(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false; // まだ無い
    };
    let Ok(len) = f.seek(SeekFrom::End(0)) else {
        return false;
    };
    if len == 0 {
        return false;
    }
    if f.seek(SeekFrom::End(-1)).is_err() {
        return false;
    }
    let mut last = [0u8; 1];
    match f.read_exact(&mut last) {
        Ok(()) => last[0] != b'\n',
        Err(_) => false,
    }
}

// ── `_limits.json` の組み立て ───────────────────────────────

/// `history` の 1 行．画面が折れ線を引くのに要るものだけを平らにしたもの．
///
/// `*_window_minutes` を «5 時間» / «週次» に畳まずそのまま持つのは，席と枠の長さの
/// 対応がプランによって変わるため (モジュール解説を参照)．判定は画面側が行う．
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryRow {
    pub observed_at: String,
    pub primary_pct: Option<f64>,
    pub primary_window_minutes: Option<u64>,
    /// Unix 秒のまま出す．画面が ISO と Unix 秒の両方を受けて JST に直す．
    /// **観測に無いものを 0 や現在時刻で埋めないこと**．null は «分からない» の意味．
    pub primary_resets_at: Option<i64>,
    pub secondary_pct: Option<f64>,
    pub secondary_window_minutes: Option<u64>,
    pub secondary_resets_at: Option<i64>,
}

/// `rate_limits` の中身．観測値の形をそのまま写す．
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputWindows {
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
}

/// ダッシュボードが読む `_limits.json` の全体．
///
/// 鍵の名前は画面側の `normalizeLimits` が読む形に合わせてあるので，勝手に変えないこと．
#[derive(Debug, Clone, Serialize)]
pub struct LimitsOutput {
    pub version: u32,
    pub provider: &'static str,
    pub updated_at: String,
    pub source: &'static str,
    pub note: &'static str,
    /// 最新観測の時刻．`updated_at` (このファイルを書いた時刻) とは別物で，
    /// 画面に «いつ時点の数字か» を出すのはこちら．
    pub observed_at: String,
    pub limit_id: Option<String>,
    pub plan_type: Option<String>,
    pub credits: Option<Credits>,
    pub rate_limits: OutputWindows,
    /// `history` を何日ぶんに絞ったか．
    pub history_days: i64,
    /// 古い順．
    pub history: Vec<HistoryRow>,
}

/// `history` を **1 日 1 点** に間引く．
///
/// 間引くのは，観測が «Codex を叩いた回数» ぶん出るためである．Claude 側の
/// 利用制限は毎時 1 点 (API を叩く回数) なので 60 日で 1,440 点にしかならないが，
/// Codex は応答ごとに 1 点付くので，3 週間で 985 点あった (2026-09-10 実測)．
/// そのまま載せると `_limits.json` が 262 KB になり，**画面を開くたびに読む**
/// ファイルとしては重い．しかも折れ線としては 1 日の中の点が潰れて見えない．
///
/// 残すのは **その日いちばん高かった点**．利用制限で知りたいのは «その日どこまで
/// 迫ったか» であって平均でも最後の値でもない (最後の値を採ると，枠がリセットされた
/// 直後に観測した日は «0%» に見えてしまう)．
///
/// **最新の観測は，その日の最大でなくても必ず残す** — 画面の «いま何 %» と
/// 折れ線の右端が食い違わないようにするため．
///
/// 間引くのは vault へ出す `history` だけで，[`JSONL`](append_observations) は
/// 全期間・全点を持ったままである．遡りたければそちらを読む．
fn thin_history(
    sorted: &[&RateLimitsObservation],
    cutoff: DateTime<Utc>,
) -> Vec<HistoryRow> {
    // 日の区切りは JST．vault の時刻はすべて JST で読むので，UTC で切ると
    // 日本時間の朝と前夜が同じ «日» に入る．
    let jst = FixedOffset::east_opt(9 * 3600).expect("+09:00");
    let day_of = |o: &RateLimitsObservation| o.observed_at.with_timezone(&jst).date_naive();
    // その日の «高さ»．primary が読めなければ secondary で比べる．
    let height = |o: &RateLimitsObservation| {
        o.primary
            .as_ref()
            .map(|w| w.used_percent)
            .or_else(|| o.secondary.as_ref().map(|w| w.used_percent))
            .unwrap_or(f64::NEG_INFINITY)
    };

    let live: Vec<&RateLimitsObservation> = sorted
        .iter()
        .copied()
        .filter(|o| o.observed_at >= cutoff)
        .collect();
    let Some(latest) = live.last().copied() else {
        return Vec::new();
    };

    let mut peak: std::collections::BTreeMap<chrono::NaiveDate, &RateLimitsObservation> =
        std::collections::BTreeMap::new();
    for o in &live {
        peak
            .entry(day_of(o))
            .and_modify(|best| {
                // 同じ高さなら後の観測を採る (その日の «到達点» の時刻に寄せる)．
                if height(o) >= height(best) {
                    *best = o;
                }
            })
            .or_insert(o);
    }
    // 最新の点は必ず入れる．その日の最大が別の点でも，右端だけは実際の «いま» にする．
    peak.insert(day_of(latest), match peak.get(&day_of(latest)) {
        Some(best) if best.observed_at > latest.observed_at => best,
        _ => latest,
    });

    let mut rows: Vec<&RateLimitsObservation> = peak.into_values().collect();
    rows.sort_by_key(|o| o.observed_at);
    rows.into_iter().map(history_row).collect()
}

/// 観測値から出力を組み立てる．**観測が 1 件も無ければ [`None`]**．
///
/// 空の枠や 0% のファイルを置かないため．画面は «無い» を正常な状態として扱う作りで，
/// 「0%」と「読めない」を混ぜると，まだ一度も観測できていない状態が
/// «今週はまだ使っていない» に化ける．
///
/// 最新は «最後に走査した観測» ではなく **«`observed_at` が最大の観測»**．
/// ファイルの走査順 (`find_sessions` の並び) が変わっても，最新が後戻りしないようにするため．
pub fn build_output(
    observations: &[RateLimitsObservation],
    updated_at: &str,
    now: DateTime<FixedOffset>,
) -> Option<LimitsOutput> {
    let mut sorted: Vec<&RateLimitsObservation> = observations.iter().collect();
    sorted.sort_by_key(|o| o.observed_at);
    let latest = *sorted.last()?;

    let cutoff = (now - Duration::days(HISTORY_DAYS)).with_timezone(&Utc);
    let history = thin_history(&sorted, cutoff);

    Some(LimitsOutput {
        version: 1,
        provider: super::PROVIDER,
        updated_at: updated_at.to_string(),
        source: SOURCE,
        note: NOTE,
        observed_at: iso(latest.observed_at),
        limit_id: latest.limit_id.clone(),
        plan_type: latest.plan_type.clone(),
        credits: latest.credits.clone(),
        rate_limits: OutputWindows {
            primary: latest.primary.clone(),
            secondary: latest.secondary.clone(),
        },
        history_days: HISTORY_DAYS,
        history,
    })
}

fn history_row(o: &RateLimitsObservation) -> HistoryRow {
    HistoryRow {
        observed_at: iso(o.observed_at),
        primary_pct: o.primary.as_ref().map(|w| w.used_percent),
        primary_window_minutes: o.primary.as_ref().map(|w| w.window_minutes),
        primary_resets_at: o.primary.as_ref().map(|w| w.resets_at),
        secondary_pct: o.secondary.as_ref().map(|w| w.used_percent),
        secondary_window_minutes: o.secondary.as_ref().map(|w| w.window_minutes),
        secondary_resets_at: o.secondary.as_ref().map(|w| w.resets_at),
    }
}

/// 観測時刻を秒までの RFC3339 にする．ミリ秒は画面に要らない．
fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// `_limits.json` を書き出す (一時ファイル経由)．
///
/// 途中で失敗しても既存の `_limits.json` を壊さないため，必ず
/// [`crate::common::atomic_write::write_atomic`] を通す．
pub fn write(dir: &Path, out: &LimitsOutput) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(OUTPUT_FILE);
    let mut bytes = serde_json::to_vec_pretty(out)?;
    bytes.push(b'\n');
    crate::common::atomic_write::write_atomic(&path, &bytes)?;
    Ok(path)
}
