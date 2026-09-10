//! 価格表 (`_logs/_claude-usage/pricing.json`) の読み込みと金額計算．
//!
//! 価格は CLI にもダッシュボードにも焼き込まない．必ずこのファイルから読む．

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// 1 モデル分の単価 (USD / 100万トークン)．
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// `pricing.json` の中身．未知のキーは無視する．
#[derive(Debug, Clone, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelPrice>,
}

impl Pricing {
    /// ファイルから読む．無い / 壊れている場合はエラー．
    ///
    /// 黙って全部 null にしてしまうと，価格表の事故に気付けないため．
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("価格表を読めません: {} ({e})", path.display()))?;
        let pricing: Pricing = serde_json::from_str(&text)
            .map_err(|e| format!("価格表の JSON が壊れています: {} ({e})", path.display()))?;
        if pricing.models.is_empty() {
            return Err(format!(
                "価格表に models が 1 件もありません: {}",
                path.display()
            ));
        }
        Ok(pricing)
    }

    /// 生のモデル名を `aliases` で正規化する．エイリアスに無ければそのまま返す．
    pub fn resolve<'a>(&'a self, raw: &'a str) -> &'a str {
        self.aliases.get(raw).map(String::as_str).unwrap_or(raw)
    }

    /// 正規化済みのモデル名の単価．`models` に無ければ `None`．
    pub fn price_of(&self, canonical: &str) -> Option<&ModelPrice> {
        self.models.get(canonical)
    }
}

/// 集計済みトークン数から金額 (USD) を求める．
///
/// 価格が無いモデルは `None`．0 で埋めない (0 円だったのか未設定なのか区別できなくなる)．
pub fn cost_usd(price: Option<&ModelPrice>, t: &crate::claude::aggregate::Totals) -> Option<f64> {
    let p = price?;
    let raw = (t.input as f64) * p.input
        + (t.output as f64) * p.output
        + (t.cache_write_5m as f64) * p.cache_write_5m
        + (t.cache_write_1h as f64) * p.cache_write_1h
        + (t.cache_read as f64) * p.cache_read;
    // USD / 100万トークン なので 1e6 で割る．表示の桁は 6 桁で丸める．
    Some(round6(raw / 1_000_000.0))
}

/// 小数第 6 位で丸める．
pub fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::aggregate::Totals;

    fn pricing() -> Pricing {
        serde_json::from_str(
            r#"{
              "aliases": {
                "opus": "claude-opus-5",
                "opus[1m]": "claude-opus-5",
                "claude-opus-5[1m]": "claude-opus-5"
              },
              "models": {
                "claude-opus-5": {"input":5.0,"output":25.0,"cache_write_5m":6.25,"cache_write_1h":10.0,"cache_read":0.5}
              }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn エイリアスを解決する() {
        let p = pricing();
        assert_eq!(p.resolve("opus[1m]"), "claude-opus-5");
        assert_eq!(p.resolve("claude-opus-5[1m]"), "claude-opus-5");
        assert_eq!(p.resolve("claude-opus-5"), "claude-opus-5");
        // 知らないモデル名はそのまま返る．
        assert_eq!(p.resolve("claude-unknown-9"), "claude-unknown-9");
    }

    #[test]
    fn 価格未設定のモデルは_none() {
        let p = pricing();
        let t = Totals {
            requests: 1,
            input: 1000,
            output: 1000,
            ..Default::default()
        };
        assert_eq!(cost_usd(p.price_of("claude-unknown-9"), &t), None);
    }

    #[test]
    fn 手計算した金額と一致する() {
        let p = pricing();
        let t = Totals {
            requests: 1,
            input: 1_000_000,
            output: 1_000_000,
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
            cache_read: 1_000_000,
        };
        // 100万トークンずつなので単価の総和がそのまま金額になる．
        // 5.0 + 25.0 + 6.25 + 10.0 + 0.5 = 46.75
        assert_eq!(cost_usd(p.price_of("claude-opus-5"), &t), Some(46.75));
    }

    #[test]
    fn 実データ相当の金額を手計算と突き合わせる() {
        let p = pricing();
        let t = Totals {
            requests: 1,
            input: 2,
            output: 159,
            cache_write_5m: 0,
            cache_write_1h: 31_533,
            cache_read: 26_479,
        };
        //   2 x 5.0        =        10.0
        // 159 x 25.0       =     3_975.0
        //  31533 x 10.0    =   315_330.0
        //  26479 x 0.5     =    13_239.5
        //                    -----------
        //                      332_554.5  / 1e6 = 0.3325545 → 0.332555 (6桁丸め)
        assert_eq!(cost_usd(p.price_of("claude-opus-5"), &t), Some(0.332555));
    }

    #[test]
    fn 空の集計は_0_円() {
        let p = pricing();
        let t = Totals::default();
        assert_eq!(cost_usd(p.price_of("claude-opus-5"), &t), Some(0.0));
    }

    #[test]
    fn 価格表が無ければエラー() {
        let err = Pricing::load(Path::new("/存在しないはずのパス/pricing.json"));
        assert!(err.is_err());
    }
}
