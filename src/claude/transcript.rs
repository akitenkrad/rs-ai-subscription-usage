//! transcript (`~/.claude/projects/**/*.jsonl`) の 1 行を解析する．
//!
//! 1 行 1 JSON．集計対象は次をすべて満たす行だけ:
//!
//! - `type == "assistant"`
//! - `message.usage` が存在する
//! - `message.model` が `<synthetic>` でない

use chrono::{DateTime, FixedOffset, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Asia/Tokyo (UTC+09:00)．
///
/// 日本には夏時間が無いので固定オフセットで足りる．chrono-tz を足さない．
pub fn jst() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).expect("+09:00 は常に有効なオフセット")
}

/// 集計の scope．メインの会話か，サブエージェントか．
///
/// 導出順は `enum` の定義順と一致させてある (`main` → `subagent`)．
/// 出力 JSON の entries の並び順にそのまま使う．
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Main,
    Subagent,
}

/// 集計対象となる 1 行分の使用量．
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// 重複排除の鍵 (`message.id`)．
    pub message_id: String,
    /// Asia/Tokyo のローカル日付 (`YYYY-MM-DD`)．
    pub date: String,
    /// Asia/Tokyo のローカル時 (0〜23)．
    ///
    /// 週の使用制限の窓は日の途中 (既定では水曜 14:00) で切り替わるので，
    /// 日付だけでは窓を切り出せない．
    pub hour: u8,
    /// `message.model` の生の値．エイリアス解決は出力時に行う．
    ///
    /// 生のまま持っておくと，`pricing.json` の `aliases` を直したときに
    /// 再走査せずとも次回実行で過去分の集計が直る．
    pub model: String,
    pub scope: Scope,
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

/// 1 行を解析した結果．
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// 集計対象の行．
    Record(Box<UsageRecord>),
    /// **人が打った発話**．週の制限の単位の候補の 1 つなので数える．
    ///
    /// tool_result の user 行 (ツールの返り値) は含めない．サブエージェントの
    /// 中の user 行も含めない — 人が打ったものではないため．
    Prompt { date: String, hour: u8 },
    /// JSON としては読めたが集計対象ではない行．
    Skip,
    /// JSON として読めない行．黙って捨てず件数を数える．
    Broken,
}

/// `v[key]` を u64 として読む．欠けていたり型が違えば 0．
fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// transcript の 1 行を解析する．
///
/// `path_is_subagent` は，そのファイルのパスが `/subagents/` を含むかどうか．
/// 含む場合は `isSidechain` の値によらず scope を `subagent` として扱う．
/// `type == "user"` の行を «人が打った発話» として数えるか判定する．
///
/// 数えないもの:
///
/// - ツールの返り値 (`content` に `tool_result` を含む) — 人が打ったものではない
/// - サブエージェントの中の user 行 (パスが `/subagents/` か `isSidechain`)
/// - 中身が空の行
fn parse_prompt(v: &Value, path_is_subagent: bool) -> ParseOutcome {
    if path_is_subagent
        || v.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return ParseOutcome::Skip;
    }
    let content = match v.get("message").and_then(|m| m.get("content")) {
        Some(c) => c,
        None => return ParseOutcome::Skip,
    };
    let is_prompt = match content {
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(blocks) => !blocks
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")),
        _ => false,
    };
    if !is_prompt {
        return ParseOutcome::Skip;
    }
    match v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
    {
        Some(dt) => {
            let local = dt.with_timezone(&jst());
            ParseOutcome::Prompt {
                date: local.format("%Y-%m-%d").to_string(),
                hour: local.hour() as u8,
            }
        }
        None => ParseOutcome::Skip,
    }
}

pub fn parse_line(line: &str, path_is_subagent: bool) -> ParseOutcome {
    let line = line.trim();
    if line.is_empty() {
        return ParseOutcome::Skip;
    }

    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return ParseOutcome::Broken,
    };

    let ty = v.get("type").and_then(Value::as_str);

    // 人の発話．**assistant の行より先に判定する** (type が違うので排他)．
    if ty == Some("user") {
        return parse_prompt(&v, path_is_subagent);
    }

    if ty != Some("assistant") {
        return ParseOutcome::Skip;
    }

    let message = match v.get("message") {
        Some(m) if m.is_object() => m,
        _ => return ParseOutcome::Skip,
    };

    let usage = match message.get("usage") {
        Some(u) if u.is_object() => u,
        _ => return ParseOutcome::Skip,
    };

    let model = match message.get("model").and_then(Value::as_str) {
        Some("<synthetic>") | None => return ParseOutcome::Skip,
        Some(m) => m.to_string(),
    };

    let message_id = match message.get("id").and_then(Value::as_str) {
        Some(id) => id.to_string(),
        None => return ParseOutcome::Skip,
    };

    let (date, hour) = match v.get("timestamp").and_then(Value::as_str) {
        Some(ts) => match DateTime::parse_from_rfc3339(ts) {
            // UTC の 15:00 以降は JST では翌日になる．
            Ok(dt) => {
                let local = dt.with_timezone(&jst());
                (local.format("%Y-%m-%d").to_string(), local.hour() as u8)
            }
            Err(_) => return ParseOutcome::Skip,
        },
        None => return ParseOutcome::Skip,
    };

    let scope = if path_is_subagent
        || v.get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        Scope::Subagent
    } else {
        Scope::Main
    };

    // `usage.iterations[]` は内訳であって合計ではない．トップレベルの値だけを読む．
    let (cache_write_5m, cache_write_1h) = match usage.get("cache_creation") {
        Some(cc) if cc.is_object() => (
            u64_at(cc, "ephemeral_5m_input_tokens"),
            u64_at(cc, "ephemeral_1h_input_tokens"),
        ),
        // `cache_creation` を持たない古い版の行では内訳が分からないので，
        // `cache_creation_input_tokens` をすべて 5m 扱いに寄せる．
        // (1h キャッシュは後発の機能なので，内訳が無い行は 5m とみなすのが実態に近い)
        _ => (u64_at(usage, "cache_creation_input_tokens"), 0),
    };

    ParseOutcome::Record(Box::new(UsageRecord {
        message_id,
        date,
        hour,
        model,
        scope,
        input: u64_at(usage, "input_tokens"),
        output: u64_at(usage, "output_tokens"),
        cache_write_5m,
        cache_write_1h,
        cache_read: u64_at(usage, "cache_read_input_tokens"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(line: &str, sub: bool) -> UsageRecord {
        match parse_line(line, sub) {
            ParseOutcome::Record(r) => *r,
            other => panic!("Record を期待したが {other:?} だった"),
        }
    }

    const WITH_CACHE_CREATION: &str = r#"{"type":"assistant","isSidechain":false,
      "timestamp":"2026-08-22T06:18:55.042Z",
      "message":{"id":"msg_1","model":"claude-opus-5","usage":{
        "input_tokens":2,"output_tokens":159,
        "cache_creation_input_tokens":31533,"cache_read_input_tokens":26479,
        "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":31533},
        "iterations":[{"input_tokens":2,"output_tokens":159}]}}}"#;

    #[test]
    fn cache_creation_ありの行を読む() {
        let r = rec(WITH_CACHE_CREATION, false);
        assert_eq!(r.message_id, "msg_1");
        assert_eq!(r.date, "2026-08-22");
        assert_eq!(r.hour, 15); // 06:18 UTC = 15:18 JST
        assert_eq!(r.model, "claude-opus-5");
        assert_eq!(r.scope, Scope::Main);
        assert_eq!(r.input, 2);
        assert_eq!(r.output, 159);
        assert_eq!(r.cache_write_5m, 0);
        assert_eq!(r.cache_write_1h, 31533);
        assert_eq!(r.cache_read, 26479);
    }

    #[test]
    fn iterations_は加算しない() {
        // iterations の中身を足してしまうと output は 159 の 2 倍になる．
        let r = rec(WITH_CACHE_CREATION, false);
        assert_eq!(r.output, 159);
        assert_eq!(r.input, 2);
    }

    #[test]
    fn cache_creation_無しの行は_5m_に寄せる() {
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T06:18:55Z",
          "message":{"id":"msg_2","model":"claude-sonnet-5","usage":{
            "input_tokens":10,"output_tokens":20,
            "cache_creation_input_tokens":777,"cache_read_input_tokens":5}}}"#;
        let r = rec(line, false);
        assert_eq!(r.cache_write_5m, 777);
        assert_eq!(r.cache_write_1h, 0);
    }

    #[test]
    fn usage_が無い行は_skip() {
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T06:18:55Z",
          "message":{"id":"msg_3","model":"claude-opus-5"}}"#;
        assert_eq!(parse_line(line, false), ParseOutcome::Skip);
    }

    #[test]
    fn 人の発話を数える() {
        let line = r#"{"type":"user","timestamp":"2026-08-29T11:09:00Z",
          "message":{"role":"user","content":"次はこれをやって"}}"#;
        match parse_line(line, false) {
            ParseOutcome::Prompt { date, hour } => {
                assert_eq!(date, "2026-08-29");
                assert_eq!(hour, 20); // UTC 11:09 → JST 20:09
            }
            other => panic!("発話として数えていない: {other:?}"),
        }
    }

    #[test]
    fn 配列の内容でも_tool_result_を含まなければ発話() {
        let line = r#"{"type":"user","timestamp":"2026-08-29T11:09:00Z",
          "message":{"role":"user","content":[{"type":"text","text":"これ"}]}}"#;
        assert!(matches!(
            parse_line(line, false),
            ParseOutcome::Prompt { .. }
        ));
    }

    #[test]
    fn tool_result_の_user_行は発話ではない() {
        let line = r#"{"type":"user","timestamp":"2026-08-29T11:09:00Z",
          "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}}"#;
        assert_eq!(parse_line(line, false), ParseOutcome::Skip);
    }

    #[test]
    fn サブエージェントの中の_user_行は発話ではない() {
        let line = r#"{"type":"user","timestamp":"2026-08-29T11:09:00Z",
          "message":{"role":"user","content":"サブエージェントへの指示"}}"#;
        assert_eq!(parse_line(line, true), ParseOutcome::Skip);
        let side = r#"{"type":"user","isSidechain":true,"timestamp":"2026-08-29T11:09:00Z",
          "message":{"role":"user","content":"指示"}}"#;
        assert_eq!(parse_line(side, false), ParseOutcome::Skip);
    }

    #[test]
    fn 中身の無い発話は数えない() {
        let line = r#"{"type":"user","timestamp":"2026-08-29T11:09:00Z",
          "message":{"role":"user","content":"   "}}"#;
        assert_eq!(parse_line(line, false), ParseOutcome::Skip);
    }

    #[test]
    fn synthetic_モデルは_skip() {
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T06:18:55Z",
          "message":{"id":"msg_4","model":"<synthetic>","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        assert_eq!(parse_line(line, false), ParseOutcome::Skip);
    }

    #[test]
    fn user_行は使用量としては数えない() {
        // 発話としては数えるが (→ 人の発話を数える)，トークンの集計には入れない．
        let line = r#"{"type":"user","timestamp":"2026-08-22T06:18:55Z","message":{"role":"user","content":"hi"}}"#;
        assert!(
            !matches!(parse_line(line, false), ParseOutcome::Record(_)),
            "user 行を使用量として数えている"
        );
    }

    #[test]
    fn message_が文字列でも_skip_で済ませる() {
        // 型が違うだけの行を「破損」と報告しないこと．
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T06:18:55Z","message":"oops"}"#;
        assert_eq!(parse_line(line, false), ParseOutcome::Skip);
    }

    #[test]
    fn 壊れた行は_broken() {
        assert_eq!(
            parse_line("{ this is not json", false),
            ParseOutcome::Broken
        );
        // 空行は破損ではない．
        assert_eq!(parse_line("   ", false), ParseOutcome::Skip);
    }

    #[test]
    fn utc_15時は_jst_では翌日になる() {
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T15:00:00.000Z",
          "message":{"id":"msg_5","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        assert_eq!(rec(line, false).date, "2026-08-23");

        // 境界の 1 秒手前は同日のまま．
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T14:59:59.999Z",
          "message":{"id":"msg_6","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        assert_eq!(rec(line, false).date, "2026-08-22");
    }

    #[test]
    fn utc_05時は_jst_では14時になる() {
        // 週の制限のリセットが JST 14:00 なので，この変換がずれると窓が丸ごと狂う．
        let line = r#"{"type":"assistant","timestamp":"2026-08-26T05:00:00.000Z",
          "message":{"id":"h1","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let r = rec(line, false);
        assert_eq!(r.date, "2026-08-26");
        assert_eq!(r.hour, 14);

        // 1 秒手前は 13 時台 (窓の 1 つ前に入る)．
        let line = r#"{"type":"assistant","timestamp":"2026-08-26T04:59:59.999Z",
          "message":{"id":"h2","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let r = rec(line, false);
        assert_eq!(r.hour, 13);
    }

    #[test]
    fn 日跨ぎでも時が正しく出る() {
        // UTC 15:00 = JST 翌日 00:00
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T15:00:00.000Z",
          "message":{"id":"h3","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let r = rec(line, false);
        assert_eq!((r.date.as_str(), r.hour), ("2026-08-23", 0));

        // UTC 14:59:59 = JST 同日 23:59:59
        let line = r#"{"type":"assistant","timestamp":"2026-08-22T14:59:59.000Z",
          "message":{"id":"h4","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let r = rec(line, false);
        assert_eq!((r.date.as_str(), r.hour), ("2026-08-22", 23));
    }

    #[test]
    fn utc_の月末は_jst_では翌月になる() {
        let line = r#"{"type":"assistant","timestamp":"2026-07-31T16:30:00.000Z",
          "message":{"id":"msg_7","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        assert_eq!(rec(line, false).date, "2026-08-01");
    }

    #[test]
    fn scope_は_issidechain_とパスの両方で決まる() {
        let sidechain = r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-08-22T00:00:00Z",
          "message":{"id":"a","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        assert_eq!(rec(sidechain, false).scope, Scope::Subagent);

        // isSidechain が無い / false でも，パスが subagents なら subagent．
        let plain = r#"{"type":"assistant","timestamp":"2026-08-22T00:00:00Z",
          "message":{"id":"b","model":"claude-opus-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        assert_eq!(rec(plain, false).scope, Scope::Main);
        assert_eq!(rec(plain, true).scope, Scope::Subagent);
    }

    #[test]
    fn scope_は_main_が_subagent_より前に並ぶ() {
        assert!(Scope::Main < Scope::Subagent);
    }
}
