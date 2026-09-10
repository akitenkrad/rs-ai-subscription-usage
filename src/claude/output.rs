//! 月次 JSON (`_logs/_claude-usage/YYYY-MM.json`) の組み立てと書き出し．

use crate::claude::aggregate::{AggEntry, Totals};
use crate::claude::pricing::{cost_usd, Pricing};
use crate::claude::transcript::Scope;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const OUTPUT_VERSION: u32 = 1;

/// 出力 JSON の 1 エントリ．フィールドの並びは仕様どおり (ダッシュボードと共有の契約)．
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OutEntry {
    pub model: String,
    pub scope: Scope,
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    /// 価格未設定のモデルは `null`．0 で埋めない．
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OutDay {
    pub date: String,
    pub entries: Vec<OutEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MonthOutput {
    pub version: u32,
    pub provider: &'static str,
    pub month: String,
    pub updated_at: String,
    pub days: Vec<OutDay>,
    pub unpriced_models: Vec<String>,
}

/// すべての `AggEntry` を月ごとの出力に組み立てる．
///
/// - モデル名はここで `pricing.json` の `aliases` により正規化する
///   (別名が同じモデルに解決される場合はここで合算される)
/// - `days` は日付昇順，`entries` は model 昇順 → scope (main → subagent) 順
/// - データが 1 件も無い月は生成しない
pub fn build_months<'a, I>(
    entries: I,
    pricing: &Pricing,
    updated_at: &str,
) -> BTreeMap<String, MonthOutput>
where
    I: IntoIterator<Item = &'a AggEntry>,
{
    // (date, canonical_model, scope) → Totals．BTreeMap なのでこの時点で並び順が確定する．
    let mut merged: BTreeMap<(String, String, Scope), Totals> = BTreeMap::new();
    // 月 → 価格未設定モデル名
    let mut unpriced: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for e in entries {
        let canonical = pricing.resolve(&e.model).to_string();
        if pricing.price_of(&canonical).is_none() {
            unpriced
                .entry(month_of(&e.date))
                .or_default()
                .insert(canonical.clone());
        }
        merged
            .entry((e.date.clone(), canonical, e.scope))
            .or_default()
            .add(&Totals {
                requests: e.requests,
                input: e.input,
                output: e.output,
                cache_write_5m: e.cache_write_5m,
                cache_write_1h: e.cache_write_1h,
                cache_read: e.cache_read,
            });
    }

    let mut months: BTreeMap<String, MonthOutput> = BTreeMap::new();
    for ((date, model, scope), t) in merged {
        let month = month_of(&date);
        let out = months.entry(month.clone()).or_insert_with(|| MonthOutput {
            version: OUTPUT_VERSION,
            provider: "claude",
            month: month.clone(),
            updated_at: updated_at.to_string(),
            days: Vec::new(),
            unpriced_models: Vec::new(),
        });
        let entry = OutEntry {
            cost_usd: cost_usd(pricing.price_of(&model), &t),
            model,
            scope,
            requests: t.requests,
            input: t.input,
            output: t.output,
            cache_write_5m: t.cache_write_5m,
            cache_write_1h: t.cache_write_1h,
            cache_read: t.cache_read,
        };
        match out.days.last_mut() {
            Some(d) if d.date == date => d.entries.push(entry),
            _ => out.days.push(OutDay {
                date,
                entries: vec![entry],
            }),
        }
    }

    for (month, out) in months.iter_mut() {
        out.unpriced_models = unpriced
            .get(month)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
    }
    months
}

/// `2026-08-22` → `2026-08`
pub fn month_of(date: &str) -> String {
    date.get(..7).unwrap_or(date).to_string()
}

/// 一時ファイルに書いてから rename する (atomic write)．
///
/// ダッシュボードが読んでいる最中に半端な JSON を見せないため．
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// 月次 JSON を書き出す．
pub fn write_month(dir: &Path, out: &MonthOutput) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", out.month));
    let mut bytes = serde_json::to_vec_pretty(out)?;
    bytes.push(b'\n');
    write_atomic(&path, &bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> Pricing {
        serde_json::from_str(
            r#"{
              "aliases": {"opus[1m]":"claude-opus-5"},
              "models": {
                "claude-opus-5": {"input":5.0,"output":25.0,"cache_write_5m":6.25,"cache_write_1h":10.0,"cache_read":0.5},
                "claude-sonnet-5": {"input":2.0,"output":10.0,"cache_write_5m":2.50,"cache_write_1h":4.0,"cache_read":0.2}
              }
            }"#,
        )
        .unwrap()
    }

    fn e(date: &str, model: &str, scope: Scope, out: u64) -> AggEntry {
        AggEntry {
            date: date.to_string(),
            hour: Some(0),
            model: model.to_string(),
            scope,
            requests: 1,
            input: 0,
            output: out,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: 0,
        }
    }

    #[test]
    fn 月ごとに分かれ日付昇順に並ぶ() {
        let all = vec![
            e("2026-09-01", "claude-opus-5", Scope::Main, 1),
            e("2026-08-22", "claude-opus-5", Scope::Main, 1),
            e("2026-08-01", "claude-opus-5", Scope::Main, 1),
        ];
        let m = build_months(&all, &pricing(), "now");
        assert_eq!(
            m.keys().cloned().collect::<Vec<_>>(),
            vec!["2026-08", "2026-09"]
        );
        let aug = &m["2026-08"];
        assert_eq!(
            aug.days.iter().map(|d| d.date.as_str()).collect::<Vec<_>>(),
            vec!["2026-08-01", "2026-08-22"]
        );
    }

    #[test]
    fn entries_は_model_昇順_scope_順に並ぶ() {
        let all = vec![
            e("2026-08-22", "claude-sonnet-5", Scope::Subagent, 1),
            e("2026-08-22", "claude-opus-5", Scope::Subagent, 1),
            e("2026-08-22", "claude-sonnet-5", Scope::Main, 1),
            e("2026-08-22", "claude-opus-5", Scope::Main, 1),
        ];
        let m = build_months(&all, &pricing(), "now");
        let got: Vec<_> = m["2026-08"].days[0]
            .entries
            .iter()
            .map(|x| (x.model.as_str(), x.scope))
            .collect();
        assert_eq!(
            got,
            vec![
                ("claude-opus-5", Scope::Main),
                ("claude-opus-5", Scope::Subagent),
                ("claude-sonnet-5", Scope::Main),
                ("claude-sonnet-5", Scope::Subagent),
            ]
        );
    }

    #[test]
    fn エイリアス違いの同一モデルは合算される() {
        let all = vec![
            e("2026-08-22", "opus[1m]", Scope::Main, 100),
            e("2026-08-22", "claude-opus-5", Scope::Main, 200),
        ];
        let m = build_months(&all, &pricing(), "now");
        let entries = &m["2026-08"].days[0].entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "claude-opus-5");
        assert_eq!(entries[0].output, 300);
        assert_eq!(entries[0].requests, 2);
        // 300 x 25.0 / 1e6 = 0.0075
        assert_eq!(entries[0].cost_usd, Some(0.0075));
    }

    #[test]
    fn 価格未設定のモデルは_null_で列挙される() {
        let all = vec![
            e("2026-08-22", "claude-未来-9", Scope::Main, 100),
            e("2026-08-22", "claude-opus-5", Scope::Main, 100),
        ];
        let m = build_months(&all, &pricing(), "now");
        let aug = &m["2026-08"];
        assert_eq!(aug.unpriced_models, vec!["claude-未来-9".to_string()]);
        let unknown = aug.days[0]
            .entries
            .iter()
            .find(|x| x.model == "claude-未来-9")
            .unwrap();
        assert_eq!(unknown.cost_usd, None);
        let known = aug.days[0]
            .entries
            .iter()
            .find(|x| x.model == "claude-opus-5")
            .unwrap();
        assert!(known.cost_usd.is_some());
    }

    #[test]
    fn 空の入力では月が一つも作られない() {
        let all: Vec<AggEntry> = vec![];
        assert!(build_months(&all, &pricing(), "now").is_empty());
    }

    #[test]
    fn 契約どおりの形で直列化される() {
        let all = vec![AggEntry {
            date: "2026-08-28".to_string(),
            hour: Some(9),
            model: "claude-opus-5".to_string(),
            scope: Scope::Main,
            requests: 123,
            input: 456,
            output: 789,
            cache_write_5m: 0,
            cache_write_1h: 31533,
            cache_read: 26479,
        }];
        let m = build_months(&all, &pricing(), "2026-08-28T21:03:11+09:00");
        let v: serde_json::Value = serde_json::to_value(&m["2026-08"]).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["month"], "2026-08");
        assert_eq!(v["updated_at"], "2026-08-28T21:03:11+09:00");
        assert_eq!(v["days"][0]["date"], "2026-08-28");
        let en = &v["days"][0]["entries"][0];
        assert_eq!(en["model"], "claude-opus-5");
        assert_eq!(en["scope"], "main");
        assert_eq!(en["requests"], 123);
        assert_eq!(en["cache_write_1h"], 31533);
        assert!(en["cost_usd"].is_number());
        assert_eq!(v["unpriced_models"], serde_json::json!([]));
    }

    #[test]
    fn 書き出しは一時ファイル経由で行われ後始末される() {
        let dir = crate::claude::aggregate::tests::tmpdir("write");
        let all = vec![e("2026-08-22", "claude-opus-5", Scope::Main, 1)];
        let m = build_months(&all, &pricing(), "now");
        let path = write_month(&dir, &m["2026-08"]).unwrap();
        assert_eq!(path.file_name().unwrap(), "2026-08.json");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        let _: serde_json::Value = serde_json::from_str(&text).unwrap();
        // .tmp が残っていないこと．
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|x| x.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 月の切り出し() {
        assert_eq!(month_of("2026-08-22"), "2026-08");
        assert_eq!(month_of("2026-08"), "2026-08");
    }
}
