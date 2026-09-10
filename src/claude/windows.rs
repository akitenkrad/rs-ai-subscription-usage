//! 週の使用制限の «窓» ごとの集計 (`_logs/_claude-usage/_windows.json`)．
//!
//! 週の制限は日の途中 (既定では水曜 14:00 JST) で切り替わるので，日単位の月次 JSON では
//! 窓を切り出せない．state を時間単位にしてあるのはこのため．
//!
//! **制限の単位はリクエスト数**であってトークン量ではない．上限は `limit.json` の
//! `limit.cost_usd_base` を直接使う (2026-08-29 に 4 点観測で単位を確定した)．
//! 観測点からの逆算は履歴・検算のために `base_limit.samples` として出すだけで，
//! 上限そのものには使わない．詳しくは [`crate::limit`]．
//!
//! **パーセントはここでは出さない．** 上限と実測の両方を出せば画面が割れる．
//! 2 か所で割ると片方だけ古くなる．

use crate::claude::aggregate::{AggEntry, PromptEntry, Totals};
use crate::claude::limit::{median, Limit, Meter};
use crate::claude::pricing::Pricing;
use crate::claude::transcript::Scope;
use crate::common::date::jst;
use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, SecondsFormat, TimeDelta, Timelike,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const WINDOWS_VERSION: u32 = 1;
const JST_OFFSET_SECS: i32 = 9 * 3600;
const MINUTES_PER_WEEK: i64 = 7 * 24 * 60;

// ── 出力の形 (画面側と共有の契約．順番も含めて変えないこと) ──────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResetOut {
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
}

/// 観測点 1 つから出た素の上限 (USD)．検算のために全部並べる．
///
/// **上限そのものは `limit.cost_usd_base` を使う．** これは «観測どうしが食い違って
/// いないか» を見るための履歴．
///
/// なお公式画面はアカウント全体 (claude.ai / Cowork 込み) なので，ここで出る値は
/// **見えない分だけ小さく**なる．窓をまたいで並べたときのばらつきの大半はそれ．
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Sample {
    pub at: String,
    pub meter: String,
    /// `"pct"` (画面の %) か `"derived"` (外から直接分かった上限) か．
    pub basis: String,
    pub pct: Option<f64>,
    pub multiplier: f64,
    pub window_start: String,
    /// 窓の start から at までの API 換算額 (pct 基準の逆算の分子)．
    pub consumed_cost_usd: Option<f64>,
    /// この観測点から出た素の上限 (USD)．
    pub cost_usd: f64,
    /// at がまだ終わっていない時間帯に入っているか．
    ///
    /// 集計は 1 時間刻みなので，その時間帯が埋まりきるまでこの逆算値は動く．
    pub partial_hour: bool,
}

/// 画面の % の丸めから出る上限の範囲 (`[下限, 上限]`，USD)．
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Range {
    pub cost_usd: [f64; 2],
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BaseLimit {
    /// 制限の単位．常に `"cost_usd"` (モデルとキャッシュ種別で重み付けしたトークン量)．
    pub unit: String,
    /// キャンペーン倍率を掛ける前の上限 (USD)．
    pub cost_usd: f64,
    pub source: String,
    /// **リクエスト数やトークン数の «上限» は存在しない．** 誤解を招くので常に null．
    pub requests: Option<u64>,
    pub tokens: Option<u64>,
    /// 画面の % 由来の観測点があるときだけ入る．
    pub range: Option<Range>,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ByModel {
    pub model: String,
    pub scope: Scope,
    pub requests: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DayTotal {
    pub date: String,
    pub requests: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

/// «直近に公式画面を読んだ値» からの推定．
///
/// # なぜ幅で出すのか
///
/// 1. **制限の単位が分かっていない**（候補 6 つ）
/// 2. アンカーの % は整数なので ±0.5 の幅がある
///
/// # なぜ比で出すのか
///
/// 公式画面は claude.ai / Cowork を含むアカウント全体で，こちらから見えるのは
/// Claude Code の分だけ．絶対値では見えない分を足せないが，**見えない分が
/// Claude Code の使用に比例して増えるなら，比は正しい**．比例からのずれが誤差になる．
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Estimate {
    pub anchor_at: String,
    pub anchor_pct: f64,
    /// 推定した時点 (いま，または窓の終わり)．
    pub at: String,
    pub low_pct: f64,
    pub high_pct: f64,
    /// アンカー時点までの API 換算額 (Claude Code の分のみ)．
    pub at_anchor_cost_usd: f64,
    /// いま (窓が閉じていれば窓の終わり) までの API 換算額．
    pub now_cost_usd: f64,
    /// `now / at_anchor`．
    pub ratio: f64,
}

/// 窓 x メーターの集計．公式画面のバー 1 本に対応する．
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MeterWindow {
    pub label: String,
    pub requests: u64,
    pub tokens: u64,
    pub cost_usd: f64,
    /// この窓の上限 (USD)．上限が分からないメーター (Fable) は `null`．
    pub limit_cost_usd: Option<f64>,
    /// 上限に対する消化率 (%)．
    ///
    /// **見えない分 (claude.ai / Cowork) を含まないので下限**．本当の値は
    /// `estimate` を見ること．
    pub used_pct: Option<f64>,
    /// 上限を超えた分の API 換算額．超えていなければ 0．
    pub overage_cost_usd: f64,
    /// 超過が始まった時刻．上限に達していなければ `null`．
    pub overage_from: Option<String>,
    /// **リクエスト数やトークン数の «上限» は存在しない．** 常に null．
    pub limit_requests: Option<u64>,
    pub limit_tokens: Option<u64>,
    /// 直近の観測点を起点にした推定 (幅つき)．観測点がこの窓に無ければ `null`．
    pub estimate: Option<Estimate>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Window {
    pub start: String,
    pub end: String,
    pub current: bool,
    pub multiplier: f64,
    /// キャンペーンの開始/終了日が窓の**内側**に落ちるときだけ日付が入る．
    ///
    /// **倍率は按分しない．** 週の途中でキャンペーンが切れたとき上限がどう扱われるかは
    /// 不明なので，画面が注意を出すための印にすぎない．
    pub campaign_boundary_inside: Option<String>,
    /// 窓ごとの上限と超過は `meters` の中にある．ここは常に null．
    pub limit_requests: Option<u64>,
    pub limit_tokens: Option<u64>,
    /// **全モデルの合計** (メーターで分ける前)．画面が使っているのでそのまま残す．
    pub requests: u64,
    /// 人が打った発話の回数．**制限の単位ではない** (2026-08-29 に棄却された)．
    /// «どれくらい会話したか» の実測として出す．
    pub prompts: u64,
    pub tokens: u64,
    pub cost_usd: f64,
    pub by_model: Vec<ByModel>,
    pub days: Vec<DayTotal>,
    /// メーターごとの内訳．公式画面が «すべてのモデル» と «Fable» を別バーで出すため．
    pub meters: BTreeMap<String, MeterWindow>,
}

/// 時が分からず窓に置けなかった日．
///
/// state 版 1 から引き継いだ行にだけ現れる — 版 2 へ移った時点で transcript が
/// 既に消えていて，読み直して時を埋められなかった分．**これ以上増えることはない**
/// (いま実在する transcript は読むときに時が入るため)．
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UnresolvedDay {
    pub date: String,
    /// その日の，窓の合計から外れたリクエスト数．
    pub requests: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WindowsOutput {
    pub version: u32,
    pub updated_at: String,
    pub reset: ResetOut,
    /// 観測点を持つメーター (通常 `all_models`) の素の上限．
    pub base_limit: BaseLimit,
    /// 時が分からず窓の集計から外した日の数 (transcript が既に消えている分)．
    ///
    /// `unresolved` の長さと同じ．画面の互換のために残してある．
    pub unresolved_days: usize,
    /// 窓の集計から外した日の内訳 (日付の昇順)．
    ///
    /// 日数だけを出しても «どれくらい足りないのか» が分からず，結局 state を開いて
    /// 数え直すことになる．日付と件数まで持たせる．
    pub unresolved: Vec<UnresolvedDay>,
    /// **新しい窓が先頭．**
    pub windows: Vec<Window>,
}

impl WindowsOutput {
    /// 窓の合計から外れたリクエスト数の合計．
    pub fn unresolved_requests(&self) -> u64 {
        self.unresolved.iter().map(|u| u.requests).sum()
    }

    /// 窓から外れた日の要約 (`"2026-07-29 の 7 リクエスト"`)．外れた日が無ければ `None`．
    ///
    /// 日数だけを出しても影響の大きさが分からないので，日付と件数を並べる．
    /// 多いときは頭の 3 日だけ出して残りは数える (警告 1 行に収めるため)．
    pub fn unresolved_summary(&self) -> Option<String> {
        const HEAD: usize = 3;
        if self.unresolved.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = self
            .unresolved
            .iter()
            .take(HEAD)
            .map(|u| format!("{} の {} リクエスト", u.date, u.requests))
            .collect();
        if self.unresolved.len() > HEAD {
            parts.push(format!(
                "ほか {} 日 (計 {} 日 / {} リクエスト)",
                self.unresolved.len() - HEAD,
                self.unresolved.len(),
                self.unresolved_requests()
            ));
        }
        Some(parts.join("，"))
    }
}

// ── 窓の切り出し ────────────────────────────────────────────────────────

/// リセット時刻を Asia/Tokyo での (曜日, 0 時からの分) に直す．
///
/// state の «時» は JST なので，リセット時刻も JST に揃えないと窓がずれる．
pub fn reset_in_jst(weekday: u32, hour: u32, minute: u32, tz_offset_secs: i32) -> (u32, u32) {
    let total = (weekday as i64) * 1440 + (hour as i64) * 60 + minute as i64;
    let shift = ((JST_OFFSET_SECS - tz_offset_secs) / 60) as i64;
    let t = (total + shift).rem_euclid(MINUTES_PER_WEEK);
    ((t / 1440) as u32, (t % 1440) as u32)
}

/// `dt` (JST) 以前で最も近い «窓の開始時刻»．
///
/// 例: リセットが水曜 14:00 なら，水曜 13:59 は**前の週**の窓に入る．
pub fn window_start_at_or_before(
    dt: NaiveDateTime,
    weekday: u32,
    minute_of_day: u32,
) -> NaiveDateTime {
    let cur = (dt.weekday().num_days_from_sunday() as i64) * 1440
        + (dt.hour() as i64) * 60
        + dt.minute() as i64;
    let target = (weekday as i64) * 1440 + minute_of_day as i64;
    let mut delta = cur - target;
    if delta < 0 {
        delta += MINUTES_PER_WEEK;
    }
    let floored = dt
        .date()
        .and_hms_opt(dt.hour(), dt.minute(), 0)
        .expect("時分は常に有効");
    floored - TimeDelta::minutes(delta)
}

/// JST の naive 日時を `+09:00` 付きの文字列にする．
fn to_jst_string(dt: NaiveDateTime) -> String {
    DateTime::<FixedOffset>::from_naive_utc_and_offset(
        dt - TimeDelta::seconds(JST_OFFSET_SECS as i64),
        jst(),
    )
    .to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// 集計行 1 本を «時» の瞬間に置いたもの．
struct Placed {
    at: NaiveDateTime,
    date: String,
    model: String,
    scope: Scope,
    totals: Totals,
    cost: f64,
}

/// 超過の集計結果．
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Overage {
    pub cost: f64,
    pub from: Option<NaiveDateTime>,
}

/// そのメーターの上限が分かっているか (観測点を持つメーターだけ)．
fn meter_has_limit(limit: &Limit, meter: &str) -> bool {
    limit.observations.iter().any(|o| o.meter == meter)
}

/// 上限 (USD) を超えた分を積む．
///
/// 単位が API 換算額なので，超過額は «上限を超えた分の金額» そのもの．
/// 時系列に並べて累計が上限に達した時間帯を見つけ，その時間帯は超過に当たる分だけ足す．
///
/// # 粒度
///
/// 集計は 1 時間刻みなので «超過が始まった時刻» は時間帯の単位でしか分からない
/// (`from` は超過が始まった時間帯の頭)．**金額そのものは時間帯の粒度でも正確**で，
/// «累計 − 上限» をそのまま足しているだけ．リクエスト基準のときは «またぐ時間帯の
/// 単価が一様» という仮定が要って +9.2% ずれたが，その問題は消えた．
///
/// なお超過は **Claude Code の分しか見えていない**ので，実際にはもっと早く超過が
/// 始まっている (claude.ai / Cowork の分がその手前に積まれている)．
pub fn overage(hours: &BTreeMap<NaiveDateTime, (u64, f64)>, limit_cost: Option<f64>) -> Overage {
    let Some(limit) = limit_cost else {
        return Overage::default();
    };
    let mut cum = 0.0f64;
    let mut out = Overage::default();
    for (at, (_req, cost)) in hours {
        if cum + cost <= limit {
            cum += cost;
            continue;
        }
        if out.from.is_none() {
            // この時間帯の途中で上限を超えた．超過に当たる分だけを足す．
            out.from = Some(*at);
            out.cost += cum + cost - limit;
        } else {
            out.cost += cost;
        }
        cum += cost;
    }
    out
}

pub fn token_sum(t: &Totals) -> u64 {
    t.input + t.output + t.cache_write_5m + t.cache_write_1h + t.cache_read
}

/// `start` から `cutoff` までの API 換算額を数える．
///
/// 集計は 1 時間刻みなので，`cutoff` をまたぐ時間帯は **経過した分だけ按分**する
/// (15:45 なら 45/60)．丸ごと入れると多すぎ，丸ごと捨てると少なすぎる．
fn cost_upto(placed: &[Placed], meter: &Meter, start: NaiveDateTime, cutoff: NaiveDateTime) -> f64 {
    let hour_floor = cutoff
        .date()
        .and_hms_opt(cutoff.hour(), 0, 0)
        .expect("時は常に有効");
    let frac = cutoff.minute() as f64 / 60.0;
    placed
        .iter()
        .filter(|p| meter.matches(&p.model) && p.at >= start && p.at <= hour_floor)
        .map(|p| p.cost * if p.at == hour_floor { frac } else { 1.0 })
        .sum()
}

/// アンカー (直近の % 観測) からの推定を作る．
///
/// アンカー時点の量が 0 の候補は比を出せないので外す (発話が 0 件のときなど)．
fn estimate_for(
    placed: &[Placed],
    meter: &Meter,
    meter_name: &str,
    limit: &Limit,
    start: NaiveDateTime,
    end: NaiveDateTime,
    now: NaiveDateTime,
) -> Option<Estimate> {
    // この窓に入っている，その メーター の % 観測のうち最も新しいもの．
    let anchor = limit
        .observations
        .iter()
        .filter(|o| o.meter == meter_name && o.pct.is_some())
        .filter_map(|o| o.at_jst().map(|at| (at, o)))
        .filter(|(at, _)| *at >= start && *at < end && *at <= now)
        .max_by_key(|(at, _)| *at)?;
    let (anchor_at, obs) = anchor;
    let pct = obs.pct?;
    let half = obs.pct_precision / 2.0;

    let at = now.min(end);
    let a = cost_upto(placed, meter, start, anchor_at);
    let b = cost_upto(placed, meter, start, at);
    if a <= 0.0 {
        return None;
    }
    let ratio = (b / a * 10_000.0).round() / 10_000.0;

    Some(Estimate {
        anchor_at: to_jst_string(anchor_at),
        anchor_pct: pct,
        at: to_jst_string(at),
        low_pct: round2((pct - half) * ratio),
        high_pct: round2((pct + half) * ratio),
        at_anchor_cost_usd: round2(a),
        now_cost_usd: round2(b),
        ratio,
    })
}

/// 窓ごとの集計を組み立てる．
///
/// `now` は «いま» (JST)．`current: true` はこれを含む窓ちょうど 1 つに付く．
pub fn build<'a, I>(
    entries: I,
    prompts: &[PromptEntry],
    pricing: &Pricing,
    limit: &Limit,
    now: NaiveDateTime,
    updated_at: &str,
) -> WindowsOutput
where
    I: IntoIterator<Item = &'a AggEntry>,
{
    let (weekday, minute_of_day) = reset_in_jst(
        limit.reset.weekday,
        limit.reset.hour,
        limit.reset.minute,
        limit.reset_tz_offset_secs(),
    );

    let mut placed: Vec<Placed> = Vec::new();
    // 時が分からない行は窓に置けない．日単位の集計 (月次 JSON) には残っているが，
    // ここでは外し，日付ごとの件数を unresolved として出す．
    let mut unresolved: BTreeMap<String, u64> = BTreeMap::new();

    for e in entries {
        let Some(hour) = e.hour else {
            *unresolved.entry(e.date.clone()).or_default() += e.requests;
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(&e.date, "%Y-%m-%d") else {
            continue;
        };
        let Some(at) = date.and_hms_opt(hour as u32, 0, 0) else {
            continue;
        };
        let canonical = pricing.resolve(&e.model).to_string();
        let totals = Totals {
            requests: e.requests,
            input: e.input,
            output: e.output,
            cache_write_5m: e.cache_write_5m,
            cache_write_1h: e.cache_write_1h,
            cache_read: e.cache_read,
        };
        // 価格未設定のモデルは 0 として足す (窓の cost_usd は数値の契約なので null にできない)．
        // どのモデルが未設定かは月次 JSON の unpriced_models と stderr の警告で分かる．
        let cost =
            crate::claude::pricing::cost_usd(pricing.price_of(&canonical), &totals).unwrap_or(0.0);
        placed.push(Placed {
            at,
            date: e.date.clone(),
            model: canonical,
            scope: e.scope,
            totals,
            cost,
        });
    }

    // 発話は (日付, 時) しか持たないので，同じように時刻へ落とす．
    let mut placed_prompts: Vec<(NaiveDateTime, u64)> = prompts
        .iter()
        .filter_map(|p| {
            let date = NaiveDate::parse_from_str(&p.date, "%Y-%m-%d").ok()?;
            let at = date.and_hms_opt(p.hour as u32, 0, 0)?;
            Some((at, p.count))
        })
        .collect();
    placed_prompts.sort_by_key(|(at, _)| *at);

    // ── 観測点から素の上限を逆算する (検算用) ────────────────────────
    //
    // 上限そのものは limit.cost_usd_base を使う．
    // ここで出すのは «2 つの観測が食い違っていないか» を見るための履歴．
    let mut samples: Vec<Sample> = Vec::new();
    // 画面の % 由来の観測点から出る上限の幅 (リクエスト数)．
    let mut pct_range: Vec<(f64, f64)> = Vec::new();

    for o in &limit.observations {
        let Some(at) = o.at_jst() else { continue };
        let Some(meter) = limit.meters.get(&o.meter) else {
            continue;
        };
        let ws = window_start_at_or_before(at, weekday, minute_of_day);
        let hour_end = at.date().and_hms_opt(at.hour(), 0, 0).unwrap() + TimeDelta::hours(1);

        // 外から上限そのものが分かっている観測点は逆算しない．
        if let Some(derived) = o.derived_limit_cost_usd {
            samples.push(Sample {
                at: to_jst_string(at),
                meter: o.meter.clone(),
                basis: "derived".to_string(),
                pct: None,
                multiplier: o.multiplier,
                window_start: to_jst_string(ws),
                consumed_cost_usd: None,
                cost_usd: round2(derived / o.multiplier),
                partial_hour: false,
            });
            continue;
        }

        let Some(pct) = o.pct else { continue };

        // 窓の start から at までの API 換算額．
        //
        // 集計は 1 時間刻みなので，at をまたぐ時間帯 (例: 15:45 なら 15:00〜16:00) は
        // そのままでは «at より後» の分まで混ざる．丸ごと入れると分母が膨らんで上限が
        // 過大に，丸ごと捨てると過小になるので，**経過した分だけ按分**する (15:45 なら 45/60)．
        let consumed_f = cost_upto(&placed, meter, ws, at);

        let scale = |pct: f64| (100.0 / pct) / o.multiplier;
        samples.push(Sample {
            at: to_jst_string(at),
            meter: o.meter.clone(),
            basis: "pct".to_string(),
            pct: Some(pct),
            multiplier: o.multiplier,
            window_start: to_jst_string(ws),
            consumed_cost_usd: Some(round2(consumed_f)),
            cost_usd: round2(consumed_f * scale(pct)),
            partial_hour: now < hour_end,
        });

        if let Some((lo_pct, hi_pct)) = o.pct_bounds() {
            // pct が大きいほうが上限は小さくなる．
            pct_range.push((consumed_f * scale(hi_pct), consumed_f * scale(lo_pct)));
        }
    }

    // 幅は画面の % 由来の観測点があるときだけ．複数あれば中央値を採る．
    let range = if pct_range.is_empty() {
        None
    } else {
        let lo = median(pct_range.iter().map(|x| x.0).collect()).unwrap_or(0.0);
        let hi = median(pct_range.iter().map(|x| x.1).collect()).unwrap_or(0.0);
        Some(Range {
            cost_usd: [round2(lo), round2(hi)],
        })
    };

    // 上限は limit.cost_usd_base をそのまま使う．
    let base_cost = limit.cost_usd_base();

    let boundaries = limit.boundaries();
    let mut windows: Vec<Window> = Vec::new();

    if let Some(earliest) = placed.iter().map(|p| p.at).min() {
        let first = window_start_at_or_before(earliest, weekday, minute_of_day);
        let last = window_start_at_or_before(now, weekday, minute_of_day);

        let mut start = first;
        while start <= last {
            let end = start + TimeDelta::days(7);
            // 境界は start を含み end を含まない．
            let inside: Vec<&Placed> = placed
                .iter()
                .filter(|p| p.at >= start && p.at < end)
                .collect();

            let multiplier = limit.multiplier_on(start.date());
            let boundary = boundaries
                .iter()
                .find(|(at, _)| *at > start && *at < end)
                .map(|(_, label)| label.clone());

            let mut by_model_map: BTreeMap<(String, Scope), (Totals, f64)> = BTreeMap::new();
            let mut by_day_map: BTreeMap<String, (Totals, f64)> = BTreeMap::new();
            let mut by_meter_map: BTreeMap<String, (Totals, f64)> = BTreeMap::new();
            // 超過の計算には «メーター x 時» の並びが要る (上限を超えた地点を探すため)．
            let mut by_meter_hour: BTreeMap<String, BTreeMap<NaiveDateTime, (u64, f64)>> =
                BTreeMap::new();
            let mut total = Totals::default();
            let mut total_cost = 0.0f64;

            for p in &inside {
                total.add(&p.totals);
                total_cost += p.cost;
                let m = by_model_map.entry((p.model.clone(), p.scope)).or_default();
                m.0.add(&p.totals);
                m.1 += p.cost;
                let d = by_day_map.entry(p.date.clone()).or_default();
                d.0.add(&p.totals);
                d.1 += p.cost;
                for (name, meter) in &limit.meters {
                    if meter.matches(&p.model) {
                        let e = by_meter_map.entry(name.clone()).or_default();
                        e.0.add(&p.totals);
                        e.1 += p.cost;
                        let h = by_meter_hour
                            .entry(name.clone())
                            .or_default()
                            .entry(p.at)
                            .or_default();
                        h.0 += p.totals.requests;
                        h.1 += p.cost;
                    }
                }
            }

            let meters: BTreeMap<String, MeterWindow> = limit
                .meters
                .iter()
                .map(|(name, meter)| {
                    let (t, c) = by_meter_map.get(name).cloned().unwrap_or_default();
                    // 上限が分かっているのは観測点を持つメーターだけ (Fable は不明)．
                    let limit_cost = meter_has_limit(limit, name)
                        .then(|| round2(base_cost * multiplier))
                        .filter(|l| *l > 0.0);
                    let empty = BTreeMap::new();
                    let hours = by_meter_hour.get(name).unwrap_or(&empty);
                    let over = overage(hours, limit_cost);
                    // **見えない分 (claude.ai / Cowork) を含まないので下限．**
                    let used_pct = limit_cost.map(|l| round2(round2(c) / l * 100.0));
                    (
                        name.clone(),
                        MeterWindow {
                            label: meter.label.clone().unwrap_or_else(|| name.clone()),
                            requests: t.requests,
                            tokens: token_sum(&t),
                            cost_usd: round2(c),
                            limit_cost_usd: limit_cost,
                            used_pct,
                            overage_cost_usd: round2(over.cost),
                            overage_from: over.from.map(to_jst_string),
                            // リクエスト数やトークン数の «上限» は存在しない．
                            limit_requests: None,
                            limit_tokens: None,
                            estimate: estimate_for(&placed, meter, name, limit, start, end, now),
                        },
                    )
                })
                .collect();

            windows.push(Window {
                start: to_jst_string(start),
                end: to_jst_string(end),
                current: start <= now && now < end,
                multiplier,
                campaign_boundary_inside: boundary,
                // 制限の単位は API 換算額なので，リクエスト数やトークン数の上限は存在しない．
                // 窓ごとの上限と超過は meters の中にある．
                limit_requests: None,
                limit_tokens: None,
                requests: total.requests,
                prompts: placed_prompts
                    .iter()
                    .filter(|(at, _)| *at >= start && *at < end)
                    .map(|(_, n)| *n)
                    .sum(),
                tokens: token_sum(&total),
                cost_usd: round2(total_cost),
                by_model: by_model_map
                    .into_iter()
                    .map(|((model, scope), (t, c))| ByModel {
                        model,
                        scope,
                        requests: t.requests,
                        tokens: token_sum(&t),
                        cost_usd: round2(c),
                    })
                    .collect(),
                days: by_day_map
                    .into_iter()
                    .map(|(date, (t, c))| DayTotal {
                        date,
                        requests: t.requests,
                        tokens: token_sum(&t),
                        cost_usd: round2(c),
                    })
                    .collect(),
                meters,
            });

            start = end;
        }
    }

    // 新しい窓が先頭．
    windows.reverse();

    WindowsOutput {
        version: WINDOWS_VERSION,
        updated_at: updated_at.to_string(),
        reset: ResetOut {
            weekday,
            hour: minute_of_day / 60,
            minute: minute_of_day % 60,
        },
        base_limit: BaseLimit {
            unit: "cost_usd".to_string(),
            cost_usd: round2(base_cost),
            source: "limit.cost_usd_base".to_string(),
            requests: None,
            tokens: None,
            range,
            samples,
        },
        unresolved_days: unresolved.len(),
        unresolved: unresolved
            .into_iter()
            .map(|(date, requests)| UnresolvedDay { date, requests })
            .collect(),
        windows,
    }
}

/// 観測点が 1 つも無ければ窓の集計を作らない．
///
/// 上限が分からない以上，`_windows.json` を書いても消化率を出せない．
/// 空の窓を書くより，ファイルごと作らないほうが «基準が無い» ことが画面に伝わる．
pub fn build_opt<'a, I>(
    entries: I,
    prompts: &[PromptEntry],
    pricing: &Pricing,
    limit: Option<&Limit>,
    now: NaiveDateTime,
    updated_at: &str,
) -> Option<WindowsOutput>
where
    I: IntoIterator<Item = &'a AggEntry>,
{
    let l = limit?;
    if l.observations.is_empty() {
        return None;
    }
    Some(build(entries, prompts, pricing, l, now, updated_at))
}

/// `_windows.json` を書き出す (一時ファイル + rename)．
pub fn write(dir: &Path, out: &WindowsOutput) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("_windows.json");
    let mut bytes = serde_json::to_vec_pretty(out)?;
    bytes.push(b'\n');
    crate::claude::output::write_atomic(&path, &bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn pricing() -> Pricing {
        serde_json::from_str(
            r#"{"aliases":{},"models":{
              "claude-opus-5":{"input":5.0,"output":25.0,"cache_write_5m":6.25,"cache_write_1h":10.0,"cache_read":0.5},
              "claude-sonnet-5":{"input":2.0,"output":10.0,"cache_write_5m":2.50,"cache_write_1h":4.0,"cache_read":0.2},
              "claude-fable-5":{"input":10.0,"output":50.0,"cache_write_5m":12.50,"cache_write_1h":20.0,"cache_read":1.0}}}"#,
        )
        .unwrap()
    }

    /// 観測点 1 件 (2026-08-28 15:45 / all_models / 13% / x1.5)．
    fn limit() -> Limit {
        crate::claude::limit::tests::limit()
    }

    /// リクエスト数と費用を指定した行．
    ///
    /// **1 件 = 出力 40,000 トークン = opus で $1 ちょうど**にしてあるので，
    /// 費用基準のテストでも «n 件 = $n» と読める．
    fn reqs(date: &str, hour: u8, requests: u64) -> AggEntry {
        AggEntry {
            requests,
            output: 40_000 * requests,
            ..e(date, Some(hour), "claude-opus-5", 0)
        }
    }

    fn e(date: &str, hour: Option<u8>, model: &str, out: u64) -> AggEntry {
        AggEntry {
            date: date.to_string(),
            hour,
            model: model.to_string(),
            scope: Scope::Main,
            requests: 1,
            input: 0,
            output: out,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: 0,
        }
    }

    // ── 窓の切り出し ────────────────────────────────────────────────

    #[test]
    fn リセット時刻を_jst_に直す() {
        assert_eq!(reset_in_jst(3, 14, 0, 9 * 3600), (3, 14 * 60));
        assert_eq!(reset_in_jst(3, 14, 0, 0), (3, 23 * 60));
        // UTC の水曜 20:00 は JST では木曜 05:00 (曜日が繰り上がる)．
        assert_eq!(reset_in_jst(3, 20, 0, 0), (4, 5 * 60));
        assert_eq!(reset_in_jst(0, 0, 0, 0), (0, 9 * 60));
        // +14:00 の日曜 00:00 は JST では土曜 19:00 (週をまたいで戻る)．
        assert_eq!(reset_in_jst(0, 0, 0, 14 * 3600), (6, 19 * 60));
    }

    #[test]
    fn 水曜14時より前は前の週の窓に入る() {
        let (w, m) = (3u32, 14 * 60u32);
        assert_eq!(
            dt("2026-08-26 00:00:00").weekday().num_days_from_sunday(),
            3
        );
        assert_eq!(
            window_start_at_or_before(dt("2026-08-26 14:00:00"), w, m),
            dt("2026-08-26 14:00:00")
        );
        assert_eq!(
            window_start_at_or_before(dt("2026-08-26 13:59:59"), w, m),
            dt("2026-08-19 14:00:00")
        );
        assert_eq!(
            window_start_at_or_before(dt("2026-08-27 09:00:00"), w, m),
            dt("2026-08-26 14:00:00")
        );
        assert_eq!(
            window_start_at_or_before(dt("2026-09-01 23:59:00"), w, m),
            dt("2026-08-26 14:00:00")
        );
        assert_eq!(
            window_start_at_or_before(dt("2026-09-02 14:00:00"), w, m),
            dt("2026-09-02 14:00:00")
        );
    }

    #[test]
    fn 窓は_start_を含み_end_を含まない() {
        let entries = vec![
            e("2026-08-26", Some(13), "claude-opus-5", 100),
            e("2026-08-26", Some(14), "claude-opus-5", 200),
            e("2026-09-02", Some(13), "claude-opus-5", 400),
            e("2026-09-02", Some(14), "claude-opus-5", 800),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-09-02 20:00:00"),
            "now",
        );
        assert_eq!(out.windows[0].start, "2026-09-02T14:00:00+09:00");
        assert_eq!(out.windows[1].start, "2026-08-26T14:00:00+09:00");
        assert_eq!(out.windows[1].end, "2026-09-02T14:00:00+09:00");
        assert_eq!(out.windows[0].tokens, 800);
        assert_eq!(out.windows[1].tokens, 600);
        assert_eq!(out.windows[2].tokens, 100);
        assert_eq!(out.windows.iter().filter(|w| w.current).count(), 1);
        assert!(out.windows[0].current);
    }

    #[test]
    fn データの最初の日から現在までの窓をすべて出す() {
        let entries = vec![e("2026-08-01", Some(0), "claude-opus-5", 1)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        assert_eq!(out.windows.len(), 5);
        assert_eq!(
            out.windows.last().unwrap().start,
            "2026-07-29T14:00:00+09:00"
        );
        assert_eq!(out.windows[0].start, "2026-08-26T14:00:00+09:00");
        assert_eq!(out.windows[1].tokens, 0);
    }

    // ── 観測点からの逆算 (検算用) ──────────────────────────────────

    #[test]
    fn 上限は_limit_cost_usd_base_をそのまま使う() {
        let entries = vec![e("2026-08-26", Some(14), "claude-opus-5", 1)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        assert_eq!(out.base_limit.unit, "cost_usd");
        assert_eq!(out.base_limit.cost_usd, 9362.0);
        assert_eq!(out.base_limit.source, "limit.cost_usd_base");
        // トークンやドルの «上限» は存在しない．
        assert_eq!(out.base_limit.tokens, None);
        assert_eq!(out.base_limit.requests, None);
        // 窓の上限も同じ (x1.5)．
        assert_eq!(
            out.windows[0].meters["all_models"].limit_cost_usd,
            Some(14043.0)
        );
        assert_eq!(out.windows[0].limit_tokens, None);
        assert_eq!(out.windows[0].limit_requests, None);
    }

    #[test]
    fn pct_の観測点はリクエスト数で逆算される() {
        // 窓の開始から 15:45 までに 90 + 40x(45/60) = 120 リクエスト．
        // 13% なら 100% は 923，倍率 1.5 なので素の上限は 615．
        let entries = vec![
            reqs("2026-08-26", 14, 90),
            reqs("2026-08-28", 15, 40),
            reqs("2026-08-28", 16, 9_999), // 観測点より後は数えない
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        assert_eq!(out.base_limit.samples.len(), 1);
        let s = &out.base_limit.samples[0];
        assert_eq!(s.basis, "pct");
        assert_eq!(s.pct, Some(13.0));
        assert_eq!(s.window_start, "2026-08-26T14:00:00+09:00");
        assert_eq!(s.consumed_cost_usd, Some(120.0));
        assert_eq!(s.cost_usd, 615.38);
        assert!(!s.partial_hour);
        // ただし上限そのものは limit.cost_usd_base のまま (逆算では上書きしない)．
        assert_eq!(out.base_limit.cost_usd, 9362.0);
    }

    #[test]
    fn 実請求からの観測点は逆算せず倍率で割るだけ() {
        let mut l = limit();
        l.observations = serde_json::from_str(
            r#"[{"at":"2026-08-19T14:00:00+09:00","meter":"all_models",
                 "derived_limit_cost_usd":14043,"multiplier":1.5}]"#,
        )
        .unwrap();
        let entries = vec![reqs("2026-08-13", 10, 5)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &l,
            dt("2026-08-28 17:00:00"),
            "now",
        );
        let s = &out.base_limit.samples[0];
        assert_eq!(s.basis, "derived");
        assert_eq!(s.pct, None);
        assert_eq!(s.consumed_cost_usd, None);
        // 14043 / 1.5 = 9362
        assert_eq!(s.cost_usd, 9362.0);
        // pct の観測点が無いので幅は出ない．
        assert_eq!(out.base_limit.range, None);
    }

    #[test]
    fn 観測点をまたぐ時間帯は経過分だけ按分する() {
        let entries = vec![reqs("2026-08-28", 15, 400)];
        let at_min = |m: u32| {
            let l: Limit = serde_json::from_str(&format!(
                r#"{{"version":3,"reset":{{"weekday":3,"hour":14,"minute":0,"tz":"+09:00"}},
                     "meters":{{"all_models":{{}}}},
                     "observations":[{{"at":"2026-08-28T15:{m:02}:00+09:00","meter":"all_models",
                                       "pct":10,"multiplier":1.0}}],"campaigns":[],
                     "limit":{{"cost_usd_base":9362}}}}"#
            ))
            .unwrap();
            build(
                &entries,
                &[],
                &pricing(),
                &l,
                dt("2026-08-28 20:00:00"),
                "now",
            )
            .base_limit
            .samples[0]
                .consumed_cost_usd
                .unwrap()
        };
        assert_eq!(at_min(0), 0.0);
        assert_eq!(at_min(15), 100.0);
        assert_eq!(at_min(30), 200.0);
        assert_eq!(at_min(45), 300.0);
        // 59/60 = 0.98333… なので 393.33
        assert!((at_min(59) - 393.33).abs() < 0.01, "{}", at_min(59));
    }

    #[test]
    fn 観測点を含む時間帯がまだ終わっていなければ印を付ける() {
        let entries = vec![reqs("2026-08-28", 15, 130)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 15:50:00"),
            "now",
        );
        assert!(out.base_limit.samples[0].partial_hour);
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        assert!(!out.base_limit.samples[0].partial_hour);
    }

    #[test]
    fn 複数の観測点は中央値を採る() {
        // 幅 (range) が中央値で決まることを見る．
        let l: Limit = serde_json::from_str(
            r#"{"version":3,"reset":{"weekday":3,"hour":14,"minute":0,"tz":"+09:00"},
                "meters":{"all_models":{}},
                "observations":[
                  {"at":"2026-08-26T15:00:00+09:00","meter":"all_models","pct":10,"pct_precision":1,"multiplier":1.0},
                  {"at":"2026-08-26T16:00:00+09:00","meter":"all_models","pct":10,"pct_precision":1,"multiplier":1.0},
                  {"at":"2026-08-26T17:00:00+09:00","meter":"all_models","pct":10,"pct_precision":1,"multiplier":1.0}],
                "campaigns":[],"limit":{"cost_usd_base":9362}}"#,
        )
        .unwrap();
        let entries = vec![
            reqs("2026-08-26", 14, 100),
            reqs("2026-08-26", 15, 100),
            reqs("2026-08-26", 16, 700),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &l,
            dt("2026-08-26 20:00:00"),
            "now",
        );
        let got: Vec<f64> = out.base_limit.samples.iter().map(|s| s.cost_usd).collect();
        assert_eq!(got, vec![1000.0, 2000.0, 9000.0]);
        // 幅は中央値のもの (10% ± 0.5 → 2000 のあたり)．平均に引きずられない．
        let r = out.base_limit.range.unwrap();
        assert_eq!(r.cost_usd, [1904.76, 2105.26]);
    }

    #[test]
    fn 丸めの幅は金額で出る() {
        let entries = vec![reqs("2026-08-28", 15, 130)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        // 130 x (45/60) = 97.5．pct 13 ± 0.5 → 97.5/0.135/1.5 = 481 …… 97.5/0.125/1.5 = 520
        assert_eq!(out.base_limit.samples[0].consumed_cost_usd, Some(97.5));
        assert_eq!(out.base_limit.range.unwrap().cost_usd, [481.48, 520.0]);
    }

    // ── 超過 ────────────────────────────────────────────────────────

    #[test]
    fn 上限に達しなければ超過は_0() {
        let entries = vec![reqs("2026-08-26", 14, 100)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        let m = &out.windows[0].meters["all_models"];
        assert_eq!(m.requests, 100);
        assert_eq!(m.overage_cost_usd, 0.0);
        assert_eq!(m.overage_from, None);
    }

    #[test]
    fn 超過は上限を超えた地点以降の実額を積む() {
        // 上限 10 リクエスト．終盤ほど高くつく状況を作る．
        let l: Limit = serde_json::from_str(
            r#"{"version":3,"reset":{"weekday":3,"hour":14,"minute":0,"tz":"+09:00"},
                "meters":{"all_models":{}},
                "observations":[{"at":"2026-08-26T15:00:00+09:00","meter":"all_models","pct":10,"multiplier":1.0}],
                "campaigns":[],"limit":{"cost_usd_base":10}}"#,
        )
        .unwrap();
        // 14 時台: 10 リクエストで $8 (320,000 出力)．上限 $10 に届かない
        // 15 時台:  5 リクエストで $12 (480,000 出力)．ここで超える
        let entries = vec![
            AggEntry {
                requests: 10,
                output: 320_000,
                ..reqs("2026-08-26", 14, 0)
            },
            AggEntry {
                requests: 5,
                output: 480_000,
                ..reqs("2026-08-26", 15, 0)
            },
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &l,
            dt("2026-08-26 20:00:00"),
            "now",
        );
        let m = &out.windows[0].meters["all_models"];
        assert_eq!(m.requests, 15);
        assert_eq!(m.limit_cost_usd, Some(10.0));
        assert_eq!(m.cost_usd, 20.0);
        // 超過は «累計 − 上限» = $10．件数では按分しない (単位が金額なので按分が要らない)．
        assert_eq!(m.overage_cost_usd, 10.0);
        assert_eq!(m.overage_from.as_deref(), Some("2026-08-26T15:00:00+09:00"));
        // 使用率は 20 / 10 = 200%（ただし見えない分を含まないので下限）．
        assert_eq!(m.used_pct, Some(200.0));
    }

    #[test]
    fn 上限をまたぐ時間帯は金額でそのまま切れる() {
        let l: Limit = serde_json::from_str(
            r#"{"version":3,"reset":{"weekday":3,"hour":14,"minute":0,"tz":"+09:00"},
                "meters":{"all_models":{}},
                "observations":[{"at":"2026-08-26T15:00:00+09:00","meter":"all_models","pct":10,"multiplier":1.0}],
                "campaigns":[],"limit":{"cost_usd_base":10}}"#,
        )
        .unwrap();
        // 14 時台の 1 本の行に $100 ぶん．上限 $10 をこの中でまたぐ．
        let entries = vec![AggEntry {
            requests: 20,
            output: 4_000_000,
            ..reqs("2026-08-26", 14, 0)
        }];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &l,
            dt("2026-08-26 20:00:00"),
            "now",
        );
        let m = &out.windows[0].meters["all_models"];
        // $100 のうち上限 $10 を超えた $90．
        assert_eq!(m.overage_cost_usd, 90.0);
        assert_eq!(m.overage_from.as_deref(), Some("2026-08-26T14:00:00+09:00"));
    }

    #[test]
    fn 超過は時系列の順に積まれる() {
        // 時間帯の並びが入れ替わっても «後ろ» が超過になる．
        let l: Limit = serde_json::from_str(
            r#"{"version":3,"reset":{"weekday":3,"hour":14,"minute":0,"tz":"+09:00"},
                "meters":{"all_models":{}},
                "observations":[{"at":"2026-08-26T15:00:00+09:00","meter":"all_models","pct":10,"multiplier":1.0}],
                "campaigns":[],"limit":{"cost_usd_base":2}}"#,
        )
        .unwrap();
        let entries = vec![
            AggEntry {
                requests: 1,
                output: 400_000,
                ..reqs("2026-08-27", 3, 0)
            }, // $10
            AggEntry {
                requests: 1,
                output: 40_000,
                ..reqs("2026-08-26", 14, 0)
            }, // $1
            AggEntry {
                requests: 1,
                output: 20_000,
                ..reqs("2026-08-26", 20, 0)
            }, // $0.5
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &l,
            dt("2026-08-28 17:00:00"),
            "now",
        );
        let m = &out.windows[0].meters["all_models"];
        // 先の 2 件 ($1 + $0.5) は上限 $2 の内側．08-27 03 時で超えて $9.5 の超過．
        assert_eq!(m.overage_cost_usd, 9.5);
        assert_eq!(m.overage_from.as_deref(), Some("2026-08-27T03:00:00+09:00"));
    }

    #[test]
    fn 上限が不明なメーターに超過は出ない() {
        let entries = vec![AggEntry {
            requests: 99_999,
            ..reqs("2026-08-26", 14, 0)
        }];
        let mut entries = entries;
        entries[0].model = "claude-fable-5".to_string();
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        let f = &out.windows[0].meters["fable"];
        assert_eq!(f.limit_cost_usd, None);
        assert_eq!(f.used_pct, None);
        assert_eq!(f.overage_cost_usd, 0.0);
        assert_eq!(f.overage_from, None);
    }

    // ── メーター ────────────────────────────────────────────────────
    // ── メーター ────────────────────────────────────────────────────

    #[test]
    fn fable_は別メーターで数えられ_all_models_に混ざらない() {
        let entries = vec![
            e("2026-08-26", Some(14), "claude-opus-5", 1_000_000),
            e("2026-08-26", Some(15), "claude-fable-5", 1_000_000),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        let w = &out.windows[0];

        // 窓の合計は全モデル (契約どおりそのまま)．
        assert_eq!(w.tokens, 2_000_000);
        // opus $25 + fable $50 = $75
        assert_eq!(w.cost_usd, 75.0);

        let all = &w.meters["all_models"];
        let fable = &w.meters["fable"];
        assert_eq!(all.label, "すべてのモデル");
        assert_eq!(all.tokens, 1_000_000);
        assert_eq!(all.cost_usd, 25.0);
        assert_eq!(fable.label, "Fable");
        assert_eq!(fable.tokens, 1_000_000);
        assert_eq!(fable.cost_usd, 50.0);
        // 合計は分割の和になる (取りこぼしも二重計上も無い)．
        assert_eq!(all.tokens + fable.tokens, w.tokens);
        assert_eq!(all.requests + fable.requests, w.requests);
    }

    #[test]
    fn 観測点の無いメーターは上限を出さない() {
        let entries = vec![e("2026-08-26", Some(14), "claude-fable-5", 100)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        let w = &out.windows[0];
        // Fable は観測点が無いので上限は null (0 で埋めない)．
        assert_eq!(w.meters["fable"].limit_cost_usd, None);
        // all_models は観測点があるので出る．
        assert_eq!(w.meters["all_models"].limit_cost_usd, Some(14043.0));
        // リクエスト数 / トークン数の «上限» はどちらのメーターにも存在しない．
        assert_eq!(w.meters["fable"].limit_tokens, None);
        assert_eq!(w.meters["all_models"].limit_tokens, None);
        assert_eq!(w.meters["all_models"].limit_requests, None);
    }

    #[test]
    fn 逆算は観測点のメーターだけを数える() {
        // Fable を大量に使っても all_models の上限は変わらない．
        let base = vec![e("2026-08-26", Some(14), "claude-opus-5", 130)];
        let with_fable = vec![
            e("2026-08-26", Some(14), "claude-opus-5", 130),
            e("2026-08-28", Some(15), "claude-fable-5", 9_999_999),
        ];
        let a = build(
            &base,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        let b = build(
            &with_fable,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        assert_eq!(a.base_limit.tokens, b.base_limit.tokens);
        assert_eq!(
            a.base_limit.samples[0].consumed_cost_usd,
            b.base_limit.samples[0].consumed_cost_usd
        );
    }

    // ── そのほか ────────────────────────────────────────────────────

    #[test]
    fn 倍率は窓の開始日で決まり境目は印だけ付く() {
        let entries = vec![
            e("2026-07-30", Some(0), "claude-opus-5", 1),
            e("2026-08-27", Some(0), "claude-opus-5", 1),
            e("2026-08-28", Some(15), "claude-opus-5", 130),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );

        let w0826 = out
            .windows
            .iter()
            .find(|w| w.start.starts_with("2026-08-26"))
            .unwrap();
        assert_eq!(w0826.multiplier, 1.5);
        assert_eq!(
            w0826.campaign_boundary_inside.as_deref(),
            Some("2026-08-31")
        );

        let w0729 = out
            .windows
            .iter()
            .find(|w| w.start.starts_with("2026-07-29"))
            .unwrap();
        assert_eq!(w0729.multiplier, 1.0);
        assert_eq!(
            w0729.campaign_boundary_inside.as_deref(),
            Some("2026-08-01")
        );
        // 倍率 1.0 の窓は上限も 1 倍 (按分しない)．
        assert_eq!(
            w0729.meters["all_models"].limit_cost_usd,
            Some(out.base_limit.cost_usd)
        );
        assert_eq!(
            w0826.meters["all_models"].limit_cost_usd,
            Some(round2(out.base_limit.cost_usd * 1.5))
        );

        let w0819 = out
            .windows
            .iter()
            .find(|w| w.start.starts_with("2026-08-19"))
            .unwrap();
        assert_eq!(w0819.campaign_boundary_inside, None);
    }

    #[test]
    fn hour_が_null_の行は窓から外れ_unresolved_days_に数えられる() {
        let entries = vec![
            e("2026-08-27", Some(10), "claude-opus-5", 100),
            e("2026-06-15", None, "claude-opus-5", 9_999),
            e("2026-06-15", None, "claude-sonnet-5", 8_888),
            e("2026-06-16", None, "claude-opus-5", 7_777),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        assert_eq!(out.unresolved_days, 2);
        let sum: u64 = out.windows.iter().map(|w| w.tokens).sum();
        assert_eq!(sum, 100);
        assert_eq!(out.windows.len(), 1);
    }

    #[test]
    fn 推定はアンカーからの比で幅を持って出る() {
        // 観測は 2026-08-28 15:45 に 13%（limit.rs の fixture）．
        // その時点までに 100 リクエスト，その後さらに 50 積んで 150 にする．
        // 比は 1.5 なので，推定は 13±0.5 の 1.5 倍 → 18.75 〜 20.25%．
        let entries = vec![reqs("2026-08-27", 10, 100), reqs("2026-08-28", 16, 50)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        let w = out.windows.iter().find(|w| w.current).unwrap();
        let est = w.meters["all_models"]
            .estimate
            .as_ref()
            .expect("推定が無い");
        assert_eq!(est.anchor_pct, 13.0);
        assert_eq!(&est.anchor_at[..16], "2026-08-28T15:45");
        // 比は $150/$100 = 1.5
        assert_eq!(est.at_anchor_cost_usd, 100.0);
        assert_eq!(est.now_cost_usd, 150.0);
        assert!(
            (est.ratio - 1.5).abs() < 1e-9,
            "比が 1.5 でない: {}",
            est.ratio
        );
        // 幅はアンカーの丸め (±0.5) だけ．単位が決まったので候補による幅は無い．
        assert!(
            (est.low_pct - 18.75).abs() < 1e-9,
            "下限が違う: {}",
            est.low_pct
        );
        assert!(
            (est.high_pct - 20.25).abs() < 1e-9,
            "上限が違う: {}",
            est.high_pct
        );
    }

    #[test]
    fn サブエージェントも同じように数える() {
        // 単位は金額なので scope で区別しない (2026-08-29 に «main のみ» は棄却された)．
        let entries = vec![
            reqs("2026-08-27", 10, 100),
            AggEntry {
                scope: Scope::Subagent,
                ..reqs("2026-08-28", 16, 100)
            },
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        let est = out.windows.iter().find(|w| w.current).unwrap().meters["all_models"]
            .estimate
            .as_ref()
            .unwrap();
        assert!(
            (est.ratio - 2.0).abs() < 1e-9,
            "サブエージェントも数えるので比 2.0"
        );
        assert!(
            (est.low_pct - 25.0).abs() < 1e-9,
            "下限は 12.5 x 2.0: {}",
            est.low_pct
        );
        assert!(
            (est.high_pct - 27.0).abs() < 1e-9,
            "上限は 13.5 x 2.0: {}",
            est.high_pct
        );
    }

    #[test]
    fn 発話は実測として窓に出るが上限には使わない() {
        let entries = vec![reqs("2026-08-27", 10, 100), reqs("2026-08-28", 16, 100)];
        let prompts = vec![
            PromptEntry {
                date: "2026-08-27".into(),
                hour: 10,
                count: 4,
            },
            PromptEntry {
                date: "2026-08-28".into(),
                hour: 16,
                count: 4,
            },
        ];
        let out = build(
            &entries,
            &prompts,
            &pricing(),
            &limit(),
            dt("2026-08-28 17:00:00"),
            "now",
        );
        let w = out.windows.iter().find(|w| w.current).unwrap();
        assert_eq!(w.prompts, 8);
        // 推定は金額だけで決まる (発話が増えても比は変わらない)．
        assert!((w.meters["all_models"].estimate.as_ref().unwrap().ratio - 2.0).abs() < 1e-9);
    }

    #[test]
    fn アンカーが無い窓には推定を出さない() {
        // 観測点はどれもこの窓の外．
        let entries = vec![reqs("2026-08-13", 10, 100)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-14 17:00:00"),
            "now",
        );
        let w = &out.windows[0];
        assert!(
            w.meters["all_models"].estimate.is_none(),
            "アンカーが無いのに推定が出ている"
        );
    }

    #[test]
    fn 窓から外れた日は日付と件数まで出す() {
        // «1 日分あります» だけだと，毎回 state を開いて中身を数え直すことになる．
        let entries = vec![
            e("2026-08-27", Some(10), "claude-opus-5", 100),
            e("2026-06-16", None, "claude-opus-5", 7_777),
            e("2026-06-15", None, "claude-opus-5", 9_999),
            e("2026-06-15", None, "claude-sonnet-5", 8_888),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        // 日付の昇順．件数は同じ日のぶんを足す．
        assert_eq!(
            out.unresolved,
            vec![
                UnresolvedDay {
                    date: "2026-06-15".to_string(),
                    requests: 2
                },
                UnresolvedDay {
                    date: "2026-06-16".to_string(),
                    requests: 1
                },
            ]
        );
        assert_eq!(out.unresolved_days, out.unresolved.len());
        assert_eq!(out.unresolved_requests(), 3);
        assert_eq!(
            out.unresolved_summary().unwrap(),
            "2026-06-15 の 2 リクエスト，2026-06-16 の 1 リクエスト"
        );
    }

    #[test]
    fn 窓から外れた日が無ければ要約も出さない() {
        let entries = vec![e("2026-08-27", Some(10), "claude-opus-5", 100)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        assert_eq!(out.unresolved_summary(), None);
    }

    #[test]
    fn 窓から外れた日が多いときは頭だけ出して残りは数える() {
        let mut entries = vec![e("2026-08-27", Some(10), "claude-opus-5", 100)];
        for d in 10..=16 {
            entries.push(e(&format!("2026-06-{d}"), None, "claude-opus-5", 1));
        }
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        assert_eq!(
            out.unresolved_summary().unwrap(),
            concat!(
                "2026-06-10 の 1 リクエスト，2026-06-11 の 1 リクエスト，2026-06-12 の 1 リクエスト，",
                "ほか 4 日 (計 7 日 / 7 リクエスト)"
            )
        );
    }

    #[test]
    fn by_model_と_days_の中身() {
        let entries = vec![
            e("2026-08-26", Some(14), "claude-opus-5", 1_000_000),
            e("2026-08-27", Some(9), "claude-sonnet-5", 1_000_000),
            e("2026-08-27", Some(10), "claude-opus-5", 1_000_000),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        let w = &out.windows[0];
        assert_eq!(w.requests, 3);
        assert_eq!(w.tokens, 3_000_000);
        assert_eq!(w.cost_usd, 60.0);
        assert_eq!(w.by_model.len(), 2);
        assert_eq!(w.by_model[0].model, "claude-opus-5");
        assert_eq!(w.by_model[0].cost_usd, 50.0);
        assert_eq!(
            w.days.iter().map(|d| d.date.as_str()).collect::<Vec<_>>(),
            vec!["2026-08-26", "2026-08-27"]
        );
        assert_eq!(w.days[1].tokens, 2_000_000);
    }

    #[test]
    fn 契約どおりの形で直列化される() {
        let entries = vec![
            e("2026-08-26", Some(14), "claude-opus-5", 130),
            e("2026-08-26", Some(15), "claude-fable-5", 10),
        ];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "2026-08-28T16:00:00+09:00",
        );
        let v: serde_json::Value = serde_json::to_value(&out).unwrap();

        assert_eq!(v["version"], 1);
        assert_eq!(v["updated_at"], "2026-08-28T16:00:00+09:00");
        assert_eq!(v["reset"]["weekday"], 3);
        assert_eq!(v["reset"]["hour"], 14);
        assert_eq!(v["unresolved_days"], 0);
        assert_eq!(v["unresolved"], serde_json::json!([]));

        // base_limit は API 換算額．リクエスト数 / トークン数の «上限» は null．
        assert_eq!(v["base_limit"]["unit"], "cost_usd");
        assert_eq!(v["base_limit"]["cost_usd"], 9362.0);
        assert_eq!(v["base_limit"]["source"], "limit.cost_usd_base");
        assert!(v["base_limit"]["tokens"].is_null());
        assert!(v["base_limit"]["requests"].is_null());
        assert!(v["base_limit"]["range"]["cost_usd"].is_array());
        assert_eq!(v["base_limit"]["samples"][0]["meter"], "all_models");
        assert_eq!(v["base_limit"]["samples"][0]["basis"], "pct");
        assert_eq!(v["base_limit"]["samples"][0]["pct"], 13.0);
        assert!(v["base_limit"]["samples"][0]["consumed_cost_usd"].is_number());

        let w = &v["windows"][0];
        assert_eq!(w["start"], "2026-08-26T14:00:00+09:00");
        assert_eq!(w["end"], "2026-09-02T14:00:00+09:00");
        assert_eq!(w["current"], true);
        assert_eq!(w["multiplier"], 1.5);
        assert_eq!(w["campaign_boundary_inside"], "2026-08-31");
        // 窓の合計は全モデル (契約どおり残す)．
        assert_eq!(w["tokens"], 140);
        assert!(w["by_model"].is_array());
        assert!(w["days"].is_array());
        // メーターごとの内訳．
        assert_eq!(w["meters"]["all_models"]["tokens"], 130);
        assert_eq!(w["meters"]["fable"]["tokens"], 10);
        // 上限は金額だけ．リクエスト数 / トークン数の上限は存在しない．
        assert_eq!(w["meters"]["all_models"]["limit_cost_usd"], 14043.0);
        assert!(w["meters"]["fable"]["limit_cost_usd"].is_null());
        assert!(w["meters"]["all_models"]["limit_tokens"].is_null());
        assert!(w["meters"]["all_models"]["limit_requests"].is_null());
        assert!(w["limit_tokens"].is_null());
        assert!(w["limit_requests"].is_null());
        // 超過と使用率が並ぶ．
        assert_eq!(w["meters"]["all_models"]["overage_cost_usd"], 0.0);
        assert!(w["meters"]["all_models"]["overage_from"].is_null());
        // **使用率は CLI が出す**（見えない分を含まない下限であることが分かるように）．
        assert!(w["meters"]["all_models"]["used_pct"].is_number());
        // 窓レベルには持たない．
        assert!(v.get("percent").is_none());
        assert!(w.get("percent").is_none());
    }

    #[test]
    fn 書き出しは一時ファイル経由で行われる() {
        let dir = crate::claude::aggregate::tests::tmpdir("windows-write");
        let entries = vec![e("2026-08-26", Some(14), "claude-opus-5", 2)];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        let path = write(&dir, &out).unwrap();
        assert_eq!(path.file_name().unwrap(), "_windows.json");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        let _: serde_json::Value = serde_json::from_str(&text).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|x| x.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn データが無ければ窓も空() {
        let entries: Vec<AggEntry> = vec![];
        let out = build(
            &entries,
            &[],
            &pricing(),
            &limit(),
            dt("2026-08-28 16:00:00"),
            "now",
        );
        assert!(out.windows.is_empty());
        assert_eq!(out.unresolved_days, 0);
        // 消費が 0 なら逆算しようがない (それでも上限は limit.cost_usd_base から出る)．
        assert_eq!(out.base_limit.samples[0].consumed_cost_usd, Some(0.0));
        assert_eq!(out.base_limit.cost_usd, 9362.0);
    }
}
