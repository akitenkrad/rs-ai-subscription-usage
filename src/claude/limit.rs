//! 週の使用制限の基準 (`_logs/_claude-usage/limit.json`)．
//!
//! # 制限の単位は «API 換算額»
//!
//! 週の制限はリクエスト数でもトークン量でもなく，**モデルとキャッシュ種別で重み付けした
//! トークン量** — つまり API 換算額 (USD) で数える．opus は sonnet の 2.5 倍，出力は入力の
//! 5 倍，キャッシュ読出は入力の 1/10 という価格そのままの重みが付く．
//!
//! ## どうやって決めたか (2026-08-29)
//!
//! 公式画面の % は claude.ai / Cowork を含む**アカウント全体**で，こちらから見えるのは
//! Claude Code の分だけ．そこで **5 時間のセッション窓の中で 4 点読み，その間 Web を
//! 触らない**ことで «見えない分» を定数 `H` に固定した．すると各点は
//!
//! ```text
//! 見える分 X = L x 表示% − H     (L = 上限, H ≥ 0)
//! ```
//!
//! を満たすはずで，これを満たせるかで候補を機械的に落とせる．
//!
//! | 候補 | 判定 |
//! |---|---|
//! | 全リクエスト数 / main のみ / opus のみ | ✗ 件数が減っているのに % が加速した |
//! | 人の発話回数 | ✗ 見えない分が負になる |
//! | 総トークン (重み無し) | ✗ 4 点目で棄却 |
//! | **API 換算額** | **○** |
//!
//! 3 点で当てはめた直線から 4 点目を 79.1〜80.7% と予測し，実際 79% で当たった．
//! 表示の遅れを 0〜6 分振っても結論は変わらない．
//!
//! # 上限の出どころ
//!
//! 上限は `limit.cost_usd_base` を**直接使う**．`cost_usd_base_range` は観測から出た
//! 範囲の記録で，計算には使わない (食い違えば `cross_check` が知らせる)．
//!
//! ```text
//! 窓の上限 = limit.cost_usd_base x 窓の開始日に有効な campaigns の倍率 (無ければ 1.0)
//! ```
//!
//! # この画面の % は必ず小さく出る
//!
//! 数えられるのは Claude Code の分だけで，claude.ai / Cowork の分は transcript に
//! 残らない．しかも日によって量が変わるので係数でも直せない．**窓の % は下限**であり，
//! 本当の値は `observations[]` の «直近に読んだ %» からの比で推定する (→ `windows::Estimate`)．
//!
//! # 捨てた基準
//!
//! - **版 1**: «ある窓の合計がちょうど 100%» と仮定して逆算した．窓の合計は
//!   «上限 + 超過» なので上限が 35% 過大になった
//! - **版 2**: トークン量 (重み無し)．実請求と 9.5 倍ずれた
//! - **版 3**: リクエスト数．観測 1 点では合っていたが，同じ窓の 2 点目と両立しなかった
//!
//! **版 1〜3 に共通する原因は «観測 1 点で単位を決めた» こと．** 1 点は複数の単位で
//! 同じ表示になる．同じ窓で 3 点以上取ること．

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const LIMIT_VERSION: u32 = 3;

/// 週の制限がリセットされる曜日と時刻．
#[derive(Debug, Clone, Deserialize)]
pub struct Reset {
    /// 0=日曜 … 6=土曜．
    pub weekday: u32,
    pub hour: u32,
    #[serde(default)]
    pub minute: u32,
    /// `"+09:00"` 形式．省略時は Asia/Tokyo とみなす．
    #[serde(default)]
    pub tz: Option<String>,
}

/// 公式画面のバー 1 本に対応するメーター．
///
/// 公式画面は «すべてのモデル» と «Fable» を別のバーで出しているので，
/// 集計もメーターごとに分ける．Fable の分を «すべてのモデル» に足さないこと．
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Meter {
    #[serde(default)]
    pub label: Option<String>,
    /// 指定があれば，このモデルだけを数える．
    #[serde(default)]
    pub only_models: Vec<String>,
    /// このモデルを数えない (`only_models` があるときは無視)．
    #[serde(default)]
    pub exclude_models: Vec<String>,
}

impl Meter {
    /// 正規化済みのモデル名がこのメーターに入るか．
    pub fn matches(&self, canonical_model: &str) -> bool {
        if !self.only_models.is_empty() {
            return self.only_models.iter().any(|m| m == canonical_model);
        }
        !self.exclude_models.iter().any(|m| m == canonical_model)
    }
}

/// 上限を裏付ける観測点．2 通りある．
///
/// - `pct`: 公式画面で読み取った «その時点の %»．丸められているので幅を持つ
/// - `derived_limit_cost_usd`: 外から直接分かった上限そのもの (USD)
#[derive(Debug, Clone, Deserialize)]
pub struct Observation {
    /// 画面を見た / 窓が閉じた時刻 (RFC3339)．
    pub at: String,
    pub meter: String,
    #[serde(default)]
    pub pct: Option<f64>,
    /// 表示の刻み幅．`1` なら整数に丸められているので上限は ±0.5% の幅を持つ．
    #[serde(default)]
    pub pct_precision: f64,
    /// 外から直接分かった «その倍率での上限» (USD)．
    #[serde(default)]
    pub derived_limit_cost_usd: Option<f64>,
    /// その時点で効いていたキャンペーン倍率．
    #[serde(default = "one")]
    pub multiplier: f64,
}

fn one() -> f64 {
    1.0
}

impl Observation {
    /// `at` を JST の naive 日時に直す．
    pub fn at_jst(&self) -> Option<NaiveDateTime> {
        DateTime::parse_from_rfc3339(&self.at)
            .ok()
            .map(|dt| dt.with_timezone(&crate::common::date::jst()).naive_local())
    }

    /// 表示の丸めを考えた % の下限・上限 (`pct_precision` が 0 なら幅なし)．
    pub fn pct_bounds(&self) -> Option<(f64, f64)> {
        let pct = self.pct?;
        let half = self.pct_precision / 2.0;
        Some(((pct - half).max(f64::MIN_POSITIVE), pct + half))
    }
}

/// 上限が一時的に増えるキャンペーン．`from`/`until` は両端を含む日付．
#[derive(Debug, Clone, Deserialize)]
pub struct Campaign {
    pub from: String,
    pub until: String,
    pub multiplier: f64,
}

/// 上限そのもの．**API 換算額 (USD) で持つ．**
///
/// 2026-08-29 に，同じ 5 時間窓で 4 点読んで単位を確定した (→ モジュールの説明)．
#[derive(Debug, Clone, Deserialize)]
pub struct LimitValues {
    /// キャンペーン倍率を掛ける前の上限 (USD)．
    pub cost_usd_base: f64,
    /// 観測から出た上限の範囲 (USD)．計算には使わないが，どれくらい絞れているかの記録．
    #[serde(default)]
    pub cost_usd_base_range: Option<[f64; 2]>,
    /// 5 時間のセッション制限 (USD)．**週とは別メーター**で，いまは集計しない．
    #[serde(default)]
    pub session_5h_cost_usd: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Limit {
    #[serde(default)]
    pub version: u32,
    pub reset: Reset,
    /// 上限．v3 では必須．
    #[serde(rename = "limit")]
    pub values: Option<LimitValues>,
    #[serde(default)]
    pub meters: BTreeMap<String, Meter>,
    #[serde(default)]
    pub observations: Vec<Observation>,
    #[serde(default)]
    pub campaigns: Vec<Campaign>,
}

impl LimitValues {
    /// 昇順に整えた `cost_usd_base_range`．
    pub fn cost_usd_range(&self) -> Option<[f64; 2]> {
        let [a, b] = self.cost_usd_base_range?;
        Some(if a <= b { [a, b] } else { [b, a] })
    }
}

impl Limit {
    /// 読み込む．
    ///
    /// - ファイルが無い → `Ok(None)`．窓の集計は基準が無いので飛ばす (エラーにしない)
    /// - 壊れている / 版が違う → `Err`．黙って飛ばすと «基準が無い» のか «書き間違えた» のか
    ///   区別できない
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("limit.json を読めません: {} ({e})", path.display())),
        };
        let mut limit: Limit = serde_json::from_str(&text).map_err(|e| {
            format!(
                "limit.json の JSON が壊れています: {} ({e})",
                path.display()
            )
        })?;

        if limit.version != LIMIT_VERSION {
            return Err(format!(
                "limit.json の版が違います (期待 {LIMIT_VERSION}, 実際 {}).\n\
                 版 2 では calibration ではなく observations[] で «その時点の %» を与えます．",
                limit.version
            ));
        }
        if limit.reset.weekday > 6 || limit.reset.hour > 23 || limit.reset.minute > 59 {
            return Err(format!(
                "limit.json: reset が範囲外です (weekday={} hour={} minute={})",
                limit.reset.weekday, limit.reset.hour, limit.reset.minute
            ));
        }
        // メーターの指定が無ければ «全モデルで 1 本» とみなす．
        if limit.meters.is_empty() {
            limit.meters.insert(
                "all_models".to_string(),
                Meter {
                    label: None,
                    only_models: Vec::new(),
                    exclude_models: Vec::new(),
                },
            );
        }
        match &limit.values {
            None => return Err("limit.json: limit.cost_usd_base がありません".to_string()),
            Some(v) if v.cost_usd_base <= 0.0 => {
                return Err(format!(
                    "limit.json: limit.cost_usd_base は正の数である必要があります ({})",
                    v.cost_usd_base
                ))
            }
            Some(_) => {}
        }
        for (i, o) in limit.observations.iter().enumerate() {
            if !limit.meters.contains_key(&o.meter) {
                return Err(format!(
                    "limit.json: observations[{i}] の meter «{}» が meters に定義されていません",
                    o.meter
                ));
            }
            if o.pct.is_none() && o.derived_limit_cost_usd.is_none() {
                return Err(format!(
                    "limit.json: observations[{i}] に pct も derived_limit_cost_usd もありません"
                ));
            }
            if let Some(p) = o.pct {
                if p <= 0.0 || p > 100.0 {
                    return Err(format!(
                        "limit.json: observations[{i}] の pct が範囲外です ({p})"
                    ));
                }
            }
            if o.multiplier <= 0.0 {
                return Err(format!(
                    "limit.json: observations[{i}] の multiplier は正の数である必要があります ({})",
                    o.multiplier
                ));
            }
            if o.at_jst().is_none() {
                return Err(format!(
                    "limit.json: observations[{i}] の at を解釈できません ({})",
                    o.at
                ));
            }
        }
        Ok(Some(limit))
    }

    /// その日に有効なキャンペーンの倍率．該当が無ければ 1.0．
    pub fn multiplier_on(&self, date: NaiveDate) -> f64 {
        for c in &self.campaigns {
            let (Ok(from), Ok(until)) = (parse_date(&c.from), parse_date(&c.until)) else {
                continue;
            };
            if from <= date && date <= until {
                return c.multiplier;
            }
        }
        1.0
    }

    /// キャンペーンの境目 (開始日の 00:00 / 終了日の翌日 00:00) を，
    /// 早い順に「(境目の瞬間, limit.json に書かれている日付文字列)」で返す．
    pub fn boundaries(&self) -> Vec<(NaiveDateTime, String)> {
        let mut v = Vec::new();
        for c in &self.campaigns {
            if let Ok(d) = parse_date(&c.from) {
                v.push((d.and_hms_opt(0, 0, 0).unwrap(), c.from.clone()));
            }
            if let Ok(d) = parse_date(&c.until) {
                // 終了日は «その日を含む» ので，境目は翌日の 00:00．
                if let Some(next) = d.succ_opt() {
                    v.push((next.and_hms_opt(0, 0, 0).unwrap(), c.until.clone()));
                }
            }
        }
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// キャンペーン倍率を掛ける前の上限 (USD)．
    pub fn cost_usd_base(&self) -> f64 {
        self.values.as_ref().map(|v| v.cost_usd_base).unwrap_or(0.0)
    }

    /// 上限が `cost_usd_base_range` の外に出ていないか．
    ///
    /// 範囲は観測から出たもので，`cost_usd_base` はその中の代表値のはず．
    /// 片方だけ直したときに気付けるようにする (計算には `cost_usd_base` を使う)．
    pub fn cross_check(&self) -> Option<String> {
        let v = self.values.as_ref()?;
        let [lo, hi] = v.cost_usd_range()?;
        if v.cost_usd_base >= lo && v.cost_usd_base <= hi {
            return None;
        }
        Some(format!(
            "limit.json: cost_usd_base ({}) が cost_usd_base_range [{lo}, {hi}] の外です．\
             計算には cost_usd_base を使います．",
            v.cost_usd_base
        ))
    }

    /// リセット時刻のタイムゾーン (秒)．既定は +09:00．
    pub fn reset_tz_offset_secs(&self) -> i32 {
        self.reset
            .tz
            .as_deref()
            .and_then(parse_tz_offset)
            .unwrap_or(9 * 3600)
    }
}

/// 中央値．偶数個なら中央 2 つの平均．空なら `None`．
///
/// 平均でなく中央値にするのは，観測点 1 つの丸め誤差に引きずられないため．
pub fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    Some(if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    })
}

pub fn parse_date(s: &str) -> Result<NaiveDate, chrono::ParseError> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
}

/// `"+09:00"` / `"-05:30"` / `"Z"` を秒に直す．
pub fn parse_tz_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    if s == "Z" || s == "z" {
        return Some(0);
    }
    let bytes = s.as_bytes();
    if bytes.len() != 6 || bytes[3] != b':' {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let h: i32 = s[1..3].parse().ok()?;
    let m: i32 = s[4..6].parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn limit() -> Limit {
        serde_json::from_str(
            r#"{
              "version": 3,
              "reset": {"weekday":3,"hour":14,"minute":0,"tz":"+09:00"},
              "meters": {
                "all_models": {"label":"すべてのモデル",
                               "exclude_models":["claude-fable-5","claude-mythos-5"]},
                "fable": {"label":"Fable","only_models":["claude-fable-5","claude-mythos-5"]}
              },
              "observations": [
                {"at":"2026-08-28T15:45:00+09:00","meter":"all_models",
                 "pct":13,"pct_precision":1,"multiplier":1.5,"basis":"requests"}
              ],
              "campaigns": [{"from":"2026-08-01","until":"2026-08-31","multiplier":1.5}],
              "limit": {"cost_usd_base":9362,"cost_usd_base_range":[9000,9500],
                        "session_5h_cost_usd":[536,559]}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn メーターはモデルを振り分ける() {
        let l = limit();
        let all = &l.meters["all_models"];
        let fable = &l.meters["fable"];

        assert!(all.matches("claude-opus-5"));
        assert!(all.matches("claude-sonnet-5"));
        // Fable は «すべてのモデル» に入らない (公式画面が別バーなので)．
        assert!(!all.matches("claude-fable-5"));
        assert!(!all.matches("claude-mythos-5"));

        assert!(fable.matches("claude-fable-5"));
        assert!(fable.matches("claude-mythos-5"));
        assert!(!fable.matches("claude-opus-5"));
    }

    #[test]
    fn 指定の無いメーターは全モデルを数える() {
        let m = Meter {
            label: None,
            only_models: vec![],
            exclude_models: vec![],
        };
        assert!(m.matches("claude-opus-5"));
        assert!(m.matches("claude-fable-5"));
    }

    #[test]
    fn 観測点の時刻と_pct_の幅() {
        let l = limit();
        let o = &l.observations[0];
        assert_eq!(o.at_jst().unwrap().to_string(), "2026-08-28 15:45:00");
        // pct_precision = 1 なら 12.5 〜 13.5．
        assert_eq!(o.pct_bounds(), Some((12.5, 13.5)));

        // precision が 0 なら幅なし．
        let mut o2 = o.clone();
        o2.pct_precision = 0.0;
        assert_eq!(o2.pct_bounds(), Some((13.0, 13.0)));

        // pct を持たない観測点には幅が無い．
        let mut o3 = o.clone();
        o3.pct = None;
        assert_eq!(o3.pct_bounds(), None);
    }

    #[test]
    fn 上限は_api_換算額で持つ() {
        let l = limit();
        assert_eq!(l.cost_usd_base(), 9362.0);
        // 代表値は観測から出た範囲の中にある．
        assert_eq!(l.cross_check(), None);
    }

    #[test]
    fn 代表値が範囲の外なら知らせる() {
        let mut l = limit();
        l.values.as_mut().unwrap().cost_usd_base = 9999.0;
        let msg = l.cross_check().unwrap();
        assert!(msg.contains("の外です"), "{msg}");
        // それでも計算には cost_usd_base を使う．
        assert_eq!(l.cost_usd_base(), 9999.0);
    }

    #[test]
    fn 実請求からの観測点は_pct_を持たない() {
        let l: Limit = serde_json::from_str(
            r#"{"version":3,"reset":{"weekday":3,"hour":14},"meters":{"all_models":{}},
                "observations":[{"at":"2026-08-19T14:00:00+09:00","meter":"all_models",
                                 "derived_limit_cost_usd":14043,"multiplier":1.5}],
                "limit":{"cost_usd_base":9362}}"#,
        )
        .unwrap();
        let o = &l.observations[0];
        assert_eq!(o.pct, None);
        assert_eq!(o.derived_limit_cost_usd, Some(14043.0));
        assert_eq!(o.pct_bounds(), None);
    }

    #[test]
    fn 上限が無い_limit_はエラー() {
        let dir = crate::claude::aggregate::tests::tmpdir("limit-no-limit");
        let p = dir.join("limit.json");
        std::fs::write(
            &p,
            r#"{"version":3,"reset":{"weekday":3,"hour":14},"meters":{"all_models":{}},
                "observations":[]}"#,
        )
        .unwrap();
        let err = Limit::load(&p).unwrap_err();
        assert!(err.contains("cost_usd_base"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 根拠の無い観測点はエラー() {
        let dir = crate::claude::aggregate::tests::tmpdir("limit-empty-obs");
        let p = dir.join("limit.json");
        std::fs::write(
            &p,
            r#"{"version":3,"reset":{"weekday":3,"hour":14},"meters":{"all_models":{}},
                "observations":[{"at":"2026-08-19T14:00:00+09:00","meter":"all_models"}],
                "limit":{"cost_usd_base":9362}}"#,
        )
        .unwrap();
        let err = Limit::load(&p).unwrap_err();
        assert!(err.contains("derived_limit_cost_usd"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 観測点の_at_は_jst_に直される() {
        let o: Observation = serde_json::from_str(
            r#"{"at":"2026-08-28T06:45:00Z","meter":"m","pct":13,"multiplier":1.0}"#,
        )
        .unwrap();
        // UTC 06:45 = JST 15:45
        assert_eq!(o.at_jst().unwrap().to_string(), "2026-08-28 15:45:00");
    }

    #[test]
    fn 中央値は外れ値に引きずられない() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![5.0]), Some(5.0));
        assert_eq!(median(vec![1.0, 100.0]), Some(50.5));
        assert_eq!(median(vec![1.0, 2.0, 100.0]), Some(2.0));
        // 平均なら 34.3 になるところが 2.0．
        assert_eq!(median(vec![100.0, 2.0, 1.0]), Some(2.0));
        assert_eq!(median(vec![1.0, 2.0, 3.0, 4.0]), Some(2.5));
    }

    #[test]
    fn キャンペーンの倍率は日付で決まる() {
        let l = limit();
        assert_eq!(l.multiplier_on(parse_date("2026-07-31").unwrap()), 1.0);
        assert_eq!(l.multiplier_on(parse_date("2026-08-01").unwrap()), 1.5);
        assert_eq!(l.multiplier_on(parse_date("2026-08-31").unwrap()), 1.5);
        assert_eq!(l.multiplier_on(parse_date("2026-09-01").unwrap()), 1.0);
    }

    #[test]
    fn 境目は開始日の_0時と終了日の翌日_0時() {
        let b = limit().boundaries();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].0.to_string(), "2026-08-01 00:00:00");
        assert_eq!(b[0].1, "2026-08-01");
        assert_eq!(b[1].0.to_string(), "2026-09-01 00:00:00");
        assert_eq!(b[1].1, "2026-08-31");
    }

    #[test]
    fn タイムゾーンを秒に直す() {
        assert_eq!(parse_tz_offset("+09:00"), Some(32400));
        assert_eq!(parse_tz_offset("-05:30"), Some(-19800));
        assert_eq!(parse_tz_offset("Z"), Some(0));
        assert_eq!(parse_tz_offset("+9:00"), None);
        assert_eq!(limit().reset_tz_offset_secs(), 32400);
    }

    #[test]
    fn 古い版の_limit_は読まずにエラーにする() {
        let dir = crate::claude::aggregate::tests::tmpdir("limit-v1");
        let p = dir.join("limit.json");
        std::fs::write(
            &p,
            r#"{"version":2,"reset":{"weekday":3,"hour":14},"meters":{"all_models":{}},
                "observations":[{"at":"2026-08-28T15:45:00+09:00","meter":"all_models","pct":13}]}"#,
        )
        .unwrap();
        let err = Limit::load(&p).unwrap_err();
        assert!(err.contains("版が違います"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 無いファイルは_none_で壊れたファイルはエラー() {
        let dir = crate::claude::aggregate::tests::tmpdir("limit-load");
        assert!(Limit::load(&dir.join("no-such.json")).unwrap().is_none());

        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{ 壊れている").unwrap();
        assert!(Limit::load(&bad).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 定義されていないメーターを指す観測点はエラー() {
        let dir = crate::claude::aggregate::tests::tmpdir("limit-bad-meter");
        let p = dir.join("limit.json");
        std::fs::write(
            &p,
            r#"{"version":3,"reset":{"weekday":3,"hour":14},
                "meters":{"all_models":{}},"limit":{"cost_usd_base":9362},
                "observations":[{"at":"2026-08-28T15:45:00+09:00","meter":"typo","pct":13}]}"#,
        )
        .unwrap();
        let err = Limit::load(&p).unwrap_err();
        assert!(err.contains("meters に定義されていません"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn retired_は無視される() {
        // 捨てた基準 (v1 / v2) は記録として残っているだけ．読まない．
        let dir = crate::claude::aggregate::tests::tmpdir("limit-retired");
        let p = dir.join("limit.json");
        std::fs::write(
            &p,
            r#"{"version":3,"reset":{"weekday":3,"hour":14},"meters":{"all_models":{}},
                "observations":[],"limit":{"cost_usd_base":9362},
                "retired":{"v1":"...","v2":"トークン量を基準にした"}}"#,
        )
        .unwrap();
        let l = Limit::load(&p).unwrap().unwrap();
        assert!(l.observations.is_empty());
        assert_eq!(l.cost_usd_base(), 9362.0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
