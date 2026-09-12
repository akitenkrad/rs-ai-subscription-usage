//! 公式の使用制限 API (`GET https://api.anthropic.com/api/oauth/usage`) の取得と記録．
//!
//! Claude Code の `/usage` 画面が読んでいるのと同じ API を叩き，返ってきた JSON を
//! そのまま `~/.local/share/claude-usage/limits.jsonl` に 1 行ずつ積む．
//! そのうえで JSONL 全体から vault 内の `_logs/_claude-usage/_limits.json` を作り直す
//! (Obsidian の Emera は vault の中のファイルしか読めないため)．
//!
//! # [`crate::limit`] / [`crate::windows`] との違い
//!
//! あちらは transcript から «こちらで見えている分» を数えて週の上限を逆算する．
//! こちらは公式の値そのものを取ってくる．**測っているものが違う**：
//!
//! - この API の % は **アカウント全体** (claude.ai / Cowork を含む) の値であり，
//!   Claude Code だけの分ではない．`windows.rs` の «使用%(下限)» とは一致しない．
//!   `limit.rs` のモジュール解説が «公式画面はアカウント全体» と言っているのと同じ性質の数字．
//! - `limit_dollars` / `used_dollars` / `remaining_dollars` は Max 20x では **すべて null**．
//!   % しか返らないので，**金額の分母はこの API からは埋まらない**．上限を USD で知りたければ
//!   従来どおり `limit.json` の観測点から逆算するしかない．
//!
//! # limits.jsonl は唯一の長期記録
//!
//! この API は «今この瞬間» の % しか返さない．過去に遡って取り直す手段は無いので，
//! `limits.jsonl` を消すと，そこまでに観測した点は二度と戻らない．
//! `state.json` と同じくバックアップの対象に含めること．
//!
//! # 未知の鍵を落とさない
//!
//! レスポンスの最上位には用途の分からないコードネームの鍵 (`tangelo` / `iguana_necktie` /
//! `nimbus_quill` / `cinder_cove` …) がある．今は意味が分からないが後から分かる可能性が
//! あるので，構造体に写し取らず [`serde_json::Value`] のまま丸ごと保存する．
//! (serde_json は鍵を辞書順に並べ替えるが，落ちる鍵は無い．)

use chrono::{DateTime, Duration, FixedOffset, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 取得先．Claude Code の `/usage` 画面と同じ．
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// この API は OAuth の beta ヘッダを要求する．
const BETA_HEADER: &str = "oauth-2025-04-20";
/// Claude Code の CLI と同じ User-Agent．
/// この 3 つのヘッダ (Authorization / anthropic-beta / User-Agent) で HTTP 200 になることを
/// 2026-08-29 に実測で確認している．
const USER_AGENT: &str = "claude-cli/2.1.251 (external, cli)";
/// キーチェーンの項目名 (`security find-generic-password -s ...`)．
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// 出力ファイル名 (vault 内)．
const OUTPUT_FILE: &str = "_limits.json";
/// `_limits.json` の `history` に残す日数．JSONL 側は全期間を持ったままにする．
pub const HISTORY_DAYS: i64 = 60;
/// 出力に添える注意書き．画面だけを見た人が数字を取り違えないようにするため，
/// ダッシュボードが読むファイル自身に書いておく．
pub const NOTE: &str = "アカウント全体 (claude.ai / Cowork を含む) の値．Claude Code だけの分ではない．limit_dollars は Max 20x では null なので金額の分母は無い．";

/// `~/.local/share/claude-usage/limits.jsonl`．
pub fn default_jsonl_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("claude-usage")
        .join("limits.jsonl")
}

// ── 認証 ────────────────────────────────────────────────────

/// キーチェーンに入っている OAuth の資格情報．
#[derive(Debug, Clone, PartialEq)]
pub struct Credentials {
    pub access_token: String,
    /// 失効時刻 (epoch ミリ秒)．鍵が無いこともある．
    pub expires_at_ms: Option<i64>,
    /// `claudeAiOauth.refreshToken` が非空で入っているか．
    pub refresh_token_present: bool,
    /// `claudeAiOauth.refreshTokenExpiresAt` (epoch ミリ秒)．鍵が無いこともある．
    pub refresh_token_expires_at_ms: Option<i64>,
}

impl Credentials {
    /// `now_ms` (epoch ミリ秒) の時点で失効しているか．
    ///
    /// 失効時刻が分からない場合は «切れていない» とみなす．分からないことを理由に
    /// 取得を止めると，鍵の名前が変わっただけで毎時のジョブが黙って死ぬ．
    /// その場合は API が 401 を返すので，どのみち気づける．
    pub fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms.map(|e| e <= now_ms).unwrap_or(false)
    }

    /// アクセストークンが更新され直す見込みがあるか．
    ///
    /// リフレッシュトークンの失効時刻が分からない場合は，更新できるとみなす．
    pub fn can_refresh(&self, now_ms: i64) -> bool {
        self.refresh_token_present
            && self
                .refresh_token_expires_at_ms
                .map(|e| e > now_ms)
                .unwrap_or(true)
    }
}

/// キーチェーンが返す JSON から `claudeAiOauth` を取り出す．
///
/// **エラーメッセージに入力をそのまま載せないこと．** アクセストークンと
/// リフレッシュトークンを含んでおり，ログ (launchd の .err) に残ると流出する．
pub fn parse_credentials(json: &str) -> Result<Credentials, String> {
    let v: Value = serde_json::from_str(json)
        .map_err(|_| "キーチェーンの中身が JSON として読めません".to_string())?;
    let oauth = v
        .get("claudeAiOauth")
        .ok_or_else(|| "キーチェーンの JSON に claudeAiOauth がありません".to_string())?;
    let token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "キーチェーンの JSON に claudeAiOauth.accessToken がありません".to_string()
        })?;
    if token.is_empty() {
        return Err("claudeAiOauth.accessToken が空です".to_string());
    }
    Ok(Credentials {
        access_token: token.to_string(),
        expires_at_ms: oauth.get("expiresAt").and_then(Value::as_i64),
        refresh_token_present: oauth
            .get("refreshToken")
            .and_then(Value::as_str)
            .is_some_and(|token| !token.is_empty()),
        refresh_token_expires_at_ms: oauth.get("refreshTokenExpiresAt").and_then(Value::as_i64),
    })
}

/// macOS のキーチェーンからアクセストークンを読む．
///
/// 専用の crate は使わず `security` を呼ぶ．依存を増やさずに済むうえ，
/// キーチェーンへのアクセス許可のダイアログも OS 側の作法どおりに出る．
pub fn read_keychain() -> Result<Credentials, String> {
    let out = Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .map_err(|e| format!("security コマンドを起動できません ({e})"))?;
    if !out.status.success() {
        // stderr も載せない．項目が見つからない旨しか入らないが，将来の版で
        // 中身が出るようになったときに気づけないため，最初から出さない方に倒す．
        return Err(format!(
            "キーチェーンから認証情報を読めません (項目 \"{KEYCHAIN_SERVICE}\"．Claude Code にログインしているか，キーチェーンへのアクセスを許可したかを確認してください)"
        ));
    }
    let json = String::from_utf8(out.stdout)
        .map_err(|_| "キーチェーンの中身が UTF-8 ではありません".to_string())?;
    parse_credentials(json.trim())
}

// ── 取得 ────────────────────────────────────────────────────

/// 使用制限 API を 1 回叩き，`fetched_at` を足した 1 レコードを返す．
///
/// トークンは引数で受け取るだけで，どこにも書き出さない．
pub fn fetch(token: &str, fetched_at: &str) -> Result<Value, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // launchd から回るので，繋がらないときに居座らないよう全体で切る．
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        // 4xx/5xx を Err にせず自分で見る．本文にサーバ側の理由が入っているため．
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", BETA_HEADER)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("使用制限 API に接続できません ({e})"))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("使用制限 API の応答本文を読めません ({e})"))?;
    if status != 200 {
        let head: String = body.chars().take(200).collect();
        return Err(format!(
            "使用制限 API が HTTP {status} を返しました: {head}"
        ));
    }
    to_record(&body, fetched_at)
}

/// 生レスポンスに `fetched_at` を足して 1 レコードにする．
///
/// 構造体に写し取らないのは，用途の分からない最上位の鍵を落とさないため
/// (モジュール解説の «未知の鍵を落とさない» を参照)．
pub fn to_record(body: &str, fetched_at: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| format!("使用制限 API の応答が JSON として読めません ({e})"))?;
    let mut obj = match v {
        Value::Object(m) => m,
        _ => return Err("使用制限 API の応答が JSON オブジェクトではありません".to_string()),
    };
    obj.insert(
        "fetched_at".to_string(),
        Value::String(fetched_at.to_string()),
    );
    Ok(Value::Object(obj))
}

// ── JSONL への追記 ──────────────────────────────────────────

/// 1 行を追記する．
///
/// **«完全な 1 行» を組み立ててから 1 回の `write_all` で書く．** 途中で失敗しても
/// 壊れた行を残さないため．追記なので既存の行には触れない — ネットワークが落ちている
/// 状態で launchd から回っても，そもそもここまで来ない．
pub fn append_jsonl(path: &Path, record: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("{} を作れません ({e})", parent.display()))?;
    }
    let mut line: Vec<u8> = Vec::new();
    if needs_leading_newline(path) {
        // 前回の追記が途中で切れていた場合に，新しい行をその尻に繋げない．
        // 繋げると «壊れた 1 行» が «壊れた 2 行» になる．
        line.push(b'\n');
    }
    serde_json::to_writer(&mut line, record).map_err(|e| format!("JSON にできません ({e})"))?;
    line.push(b'\n');

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{} を開けません ({e})", path.display()))?;
    f.write_all(&line)
        .map_err(|e| format!("{} に書けません ({e})", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("{} を同期できません ({e})", path.display()))?;
    Ok(())
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

/// JSONL の読み込み結果．
#[derive(Debug, Default)]
pub struct Jsonl {
    pub records: Vec<Value>,
    /// JSON として読めなかった行数．読み飛ばした分．
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
        match serde_json::from_str::<Value>(line) {
            Ok(v) if v.is_object() => out.records.push(v),
            _ => out.broken_lines += 1,
        }
    }
    Ok(out)
}

// ── `_limits.json` の組み立て ───────────────────────────────

/// `history` の 1 行．画面が折れ線を引くのに要るものだけを平らにしたもの．
///
/// 生の値が要る場合は `latest`，それ以前まで遡るなら `limits.jsonl` を見ること．
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryRow {
    pub fetched_at: String,
    pub five_hour_pct: Option<f64>,
    pub five_hour_resets_at: Option<String>,
    pub seven_day_pct: Option<f64>,
    pub seven_day_resets_at: Option<String>,
    /// Max 20x では今のところ常に null (API が null を返す)．
    pub seven_day_opus_pct: Option<f64>,
    /// 同上．
    pub seven_day_sonnet_pct: Option<f64>,
}

/// ダッシュボードが読む `_limits.json` の全体．
#[derive(Debug, Clone, Serialize)]
pub struct LimitsOutput {
    pub updated_at: String,
    pub source: String,
    pub note: String,
    /// `history` を何日ぶんに絞ったか．
    pub history_days: i64,
    /// `history` に入っている最も古い «あり得る» 時刻 (この時刻より前は切り捨てた)．
    pub history_from: String,
    /// 最新 1 件の生レスポンス (+ `fetched_at`)．未知の鍵もそのまま入っている．
    pub latest: Option<Value>,
    /// 古い順．
    pub history: Vec<HistoryRow>,
}

/// JSONL の全レコードから出力を組み立てる．
///
/// `history` は `now` から [`HISTORY_DAYS`] 日ぶんに絞る — 際限なく膨らませると
/// vault 側のファイルが毎時大きくなり，Sync と Emera の読み込みに響くため．
/// **JSONL 側は絞らない**ので，遡りたければそちらを見る．
///
/// `latest` は «最後の行» ではなく «`fetched_at` が最大のレコード»．
/// 行の順序が入れ替わっても最新が後戻りしないようにするため．
pub fn build_output(
    records: &[Value],
    updated_at: &str,
    now: DateTime<FixedOffset>,
) -> LimitsOutput {
    let mut rows: Vec<(DateTime<FixedOffset>, HistoryRow, &Value)> = records
        .iter()
        .filter_map(|r| history_row(r).map(|(dt, row)| (dt, row, r)))
        .collect();
    rows.sort_by_key(|(dt, _, _)| *dt);

    let latest = rows.last().map(|(_, _, raw)| (*raw).clone());
    let cutoff = now - Duration::days(HISTORY_DAYS);
    let history: Vec<HistoryRow> = rows
        .into_iter()
        .filter(|(dt, _, _)| *dt >= cutoff)
        .map(|(_, row, _)| row)
        .collect();

    LimitsOutput {
        updated_at: updated_at.to_string(),
        source: format!("GET {USAGE_URL}"),
        note: NOTE.to_string(),
        history_days: HISTORY_DAYS,
        history_from: cutoff.to_rfc3339_opts(SecondsFormat::Secs, false),
        latest,
        history,
    }
}

/// 1 レコードから `history` の 1 行を作る．
///
/// `fetched_at` が読めないレコードは順序も新旧も決められないので落とす
/// (`limits.jsonl` には残ったままなので，失われるわけではない)．
fn history_row(rec: &Value) -> Option<(DateTime<FixedOffset>, HistoryRow)> {
    let at = rec.get("fetched_at")?.as_str()?;
    let dt = DateTime::parse_from_rfc3339(at).ok()?;
    Some((
        dt,
        HistoryRow {
            fetched_at: dt.to_rfc3339_opts(SecondsFormat::Secs, false),
            five_hour_pct: pct_of(rec, "five_hour"),
            five_hour_resets_at: resets_of(rec, "five_hour"),
            seven_day_pct: pct_of(rec, "seven_day"),
            seven_day_resets_at: resets_of(rec, "seven_day"),
            seven_day_opus_pct: pct_of(rec, "seven_day_opus"),
            seven_day_sonnet_pct: pct_of(rec, "seven_day_sonnet"),
        },
    ))
}

/// `rec[key].utilization`．鍵ごと null のことがある (Max 20x の `seven_day_opus` など)．
fn pct_of(rec: &Value, key: &str) -> Option<f64> {
    rec.get(key)?.get("utilization")?.as_f64()
}

/// `rec[key].resets_at` を秒までに丸めたもの．
fn resets_of(rec: &Value, key: &str) -> Option<String> {
    Some(normalize_ts(rec.get(key)?.get("resets_at")?.as_str()?))
}

/// RFC3339 を秒までに丸める．API はマイクロ秒まで返すが，画面には要らない．
///
/// 読めない文字列は **捨てずにそのまま返す**．こちらの想定が古いだけかもしれないため．
pub fn normalize_ts(s: &str) -> String {
    match DateTime::parse_from_rfc3339(s) {
        Ok(dt) => dt.to_rfc3339_opts(SecondsFormat::Secs, false),
        Err(_) => s.to_string(),
    }
}

/// `_limits.json` を書き出す (一時ファイル経由)．
pub fn write(dir: &Path, out: &LimitsOutput) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(OUTPUT_FILE);
    let mut bytes = serde_json::to_vec_pretty(out)?;
    bytes.push(b'\n');
    crate::claude::output::write_atomic(&path, &bytes)?;
    Ok(path)
}

/// 今の時刻 (UTC，秒まで) を `fetched_at` の形で返す．
pub fn now_fetched_at(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 実物のレスポンスを短くしたもの．用途の分からない鍵をわざと残してある．
    const BODY: &str = r#"{
      "five_hour":{"utilization":29.0,"resets_at":"2026-08-29T17:40:00.364259+00:00","limit_dollars":null,"used_dollars":null,"remaining_dollars":null,"locked_reason":null},
      "seven_day":{"utilization":46.0,"resets_at":"2026-09-02T05:00:01.364280+00:00","limit_dollars":null},
      "seven_day_opus":null,
      "seven_day_sonnet":null,
      "tangelo":null,
      "iguana_necktie":null,
      "nimbus_quill":{"utilization":0.0,"resets_at":null},
      "extra_usage":{"is_enabled":false,"user_disabled":true},
      "limits":[{"kind":"session","group":"session","percent":29,"severity":"normal","is_active":false}],
      "spend":{"used":{"amount_minor":0,"currency":"USD","exponent":2},"percent":0},
      "member_dashboard_available":false
    }"#;

    fn rec(at: &str, five: f64, seven: f64) -> Value {
        json!({
            "fetched_at": at,
            "five_hour": {"utilization": five, "resets_at": "2026-08-29T17:40:00.364259+00:00", "limit_dollars": null},
            "seven_day": {"utilization": seven, "resets_at": "2026-09-02T05:00:01.364280+00:00"},
            "seven_day_opus": null,
            "seven_day_sonnet": null,
            "tangelo": null
        })
    }

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-08-29T23:00:00+09:00").unwrap()
    }

    // ── 応答のパース ────────────────────────────────────────

    #[test]
    fn 未知の鍵を一つも落とさずにレコードにする() {
        let r = to_record(BODY, "2026-08-29T14:17:47+00:00").unwrap();
        let obj = r.as_object().unwrap();
        for key in [
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
            "tangelo",
            "iguana_necktie",
            "nimbus_quill",
            "extra_usage",
            "limits",
            "spend",
            "member_dashboard_available",
        ] {
            assert!(obj.contains_key(key), "{key} が落ちている");
        }
        // null の鍵も «鍵が無い» ではなく «null» のまま残ること．
        assert!(obj["tangelo"].is_null());
        assert_eq!(obj["nimbus_quill"]["utilization"], 0.0);
        assert_eq!(obj["fetched_at"], "2026-08-29T14:17:47+00:00");
        // 一度 JSON に落として読み直しても同じ．
        let round: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(round, r);
    }

    #[test]
    fn 応答が_json_でなければ弾く() {
        let e = to_record("<html>503</html>", "now").unwrap_err();
        assert!(e.contains("JSON として読めません"), "{e}");
    }

    #[test]
    fn 応答が配列なら弾く() {
        let e = to_record("[1,2,3]", "now").unwrap_err();
        assert!(e.contains("オブジェクトではありません"), "{e}");
    }

    // ── 認証 ────────────────────────────────────────────────

    #[test]
    fn キーチェーンの_json_からトークンと失効時刻を取り出す() {
        let c = parse_credentials(
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-XXXX","expiresAt":1788000000000,"scopes":["user:inference"]}}"#,
        )
        .unwrap();
        assert_eq!(c.access_token, "sk-ant-oat01-XXXX");
        assert_eq!(c.expires_at_ms, Some(1788000000000));
    }

    #[test]
    fn キーチェーンの_json_からリフレッシュトークンの有無と失効時刻を取り出す() {
        let c = parse_credentials(
            r#"{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh","expiresAt":1789239872398,"refreshTokenExpiresAt":1790059942398}}"#,
        )
        .unwrap();
        assert!(c.refresh_token_present);
        assert_eq!(c.refresh_token_expires_at_ms, Some(1790059942398));
    }

    #[test]
    fn リフレッシュトークンが無いか空なら更新できない() {
        for json in [
            r#"{"claudeAiOauth":{"accessToken":"access","refreshTokenExpiresAt":2000}}"#,
            r#"{"claudeAiOauth":{"accessToken":"access","refreshToken":"","refreshTokenExpiresAt":2000}}"#,
        ] {
            let c = parse_credentials(json).unwrap();
            assert!(!c.refresh_token_present);
            assert!(!c.can_refresh(1_000));
        }
    }

    #[test]
    fn リフレッシュトークンの失効時刻以降は更新できない() {
        let c = parse_credentials(
            r#"{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh","refreshTokenExpiresAt":1000}}"#,
        )
        .unwrap();
        assert!(c.can_refresh(999));
        assert!(!c.can_refresh(1_000));
        assert!(!c.can_refresh(1_001));
    }

    #[test]
    fn リフレッシュトークンがあり失効時刻が無ければ更新できるとみなす() {
        let c = parse_credentials(
            r#"{"claudeAiOauth":{"accessToken":"access","refreshToken":"refresh"}}"#,
        )
        .unwrap();
        assert!(c.refresh_token_present);
        assert_eq!(c.refresh_token_expires_at_ms, None);
        assert!(c.can_refresh(i64::MAX));
    }

    #[test]
    fn 失効時刻を過ぎたトークンを期限切れと判定する() {
        let c = Credentials {
            access_token: "t".into(),
            expires_at_ms: Some(1_000),
            refresh_token_present: false,
            refresh_token_expires_at_ms: None,
        };
        assert!(c.is_expired(1_000));
        assert!(c.is_expired(1_001));
        assert!(!c.is_expired(999));
    }

    #[test]
    fn 失効時刻が無ければ期限切れとはみなさない() {
        // 分からないことを理由に取得を止めない (止めると鍵の名前が変わっただけで死ぬ)．
        let c = Credentials {
            access_token: "t".into(),
            expires_at_ms: None,
            refresh_token_present: false,
            refresh_token_expires_at_ms: None,
        };
        assert!(!c.is_expired(i64::MAX));
    }

    #[test]
    fn 資格情報のエラーメッセージにトークンを載せない() {
        // accessToken が無い形でも，入力そのものを貼り付けて返さないこと．
        let bad = r#"{"claudeAiOauth":{"token":"sk-ant-oat01-SECRET"}}"#;
        let e = parse_credentials(bad).unwrap_err();
        assert!(!e.contains("SECRET"), "{e}");
        let e = parse_credentials("sk-ant-oat01-SECRET").unwrap_err();
        assert!(!e.contains("SECRET"), "{e}");
    }

    #[test]
    fn 資格情報のエラーメッセージにリフレッシュトークンを載せない() {
        for json in [
            r#"{"claudeAiOauth":{"refreshToken":"REFRESH_SECRET"}}"#,
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"REFRESH_SECRET"}}"#,
            r#"{"claudeAiOauth":{"accessToken":42,"refreshToken":"REFRESH_SECRET"}}"#,
            r#"{"claudeAiOauth":{"refreshToken":"REFRESH_SECRET"}"#,
        ] {
            let e = parse_credentials(json).unwrap_err();
            assert!(!e.contains("REFRESH_SECRET"), "{e}");
        }
    }

    // ── JSONL ───────────────────────────────────────────────

    #[test]
    fn 壊れた行があっても他の行は読める() {
        let dir = crate::claude::aggregate::tests::tmpdir("limits-broken");
        let path = dir.join("limits.jsonl");
        std::fs::write(
            &path,
            "{\"fetched_at\":\"2026-08-29T00:00:00+00:00\"}\n\
             {\"fetched_at\": 途中で切れ\n\
             \n\
             [1,2]\n\
             {\"fetched_at\":\"2026-08-29T01:00:00+00:00\"}\n",
        )
        .unwrap();
        let j = read_jsonl(&path).unwrap();
        assert_eq!(j.records.len(), 2);
        assert_eq!(j.broken_lines, 2); // 途中で切れた行と，オブジェクトでない行
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn まだ無いファイルは空として読む() {
        let dir = crate::claude::aggregate::tests::tmpdir("limits-none");
        let j = read_jsonl(&dir.join("limits.jsonl")).unwrap();
        assert!(j.records.is_empty());
        assert_eq!(j.broken_lines, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 追記は一行ずつ積まれる() {
        let dir = crate::claude::aggregate::tests::tmpdir("limits-append");
        let path = dir.join("sub").join("limits.jsonl");
        append_jsonl(&path, &rec("2026-08-29T00:00:00+00:00", 1.0, 2.0)).unwrap();
        append_jsonl(&path, &rec("2026-08-29T01:00:00+00:00", 3.0, 4.0)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
        let j = read_jsonl(&path).unwrap();
        assert_eq!(j.broken_lines, 0);
        assert_eq!(j.records.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 改行で終わっていない行の尻に継ぎ足さない() {
        let dir = crate::claude::aggregate::tests::tmpdir("limits-partial");
        let path = dir.join("limits.jsonl");
        // 前回の追記が途中で切れた状態を作る．
        std::fs::write(
            &path,
            "{\"fetched_at\":\"2026-08-29T00:00:00+00:00\",\"five_h",
        )
        .unwrap();
        append_jsonl(&path, &rec("2026-08-29T01:00:00+00:00", 3.0, 4.0)).unwrap();
        let j = read_jsonl(&path).unwrap();
        // 壊れた行は壊れたまま 1 行．新しい行はきちんと読める．
        assert_eq!(j.broken_lines, 1);
        assert_eq!(j.records.len(), 1);
        assert_eq!(j.records[0]["fetched_at"], "2026-08-29T01:00:00+00:00");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── `_limits.json` の組み立て ───────────────────────────

    #[test]
    fn history_は_60_日で切り詰められ_jsonl_側は絞らない() {
        let old = (now() - Duration::days(90)).to_rfc3339_opts(SecondsFormat::Secs, false);
        let edge = (now() - Duration::days(59)).to_rfc3339_opts(SecondsFormat::Secs, false);
        let recent = (now() - Duration::days(1)).to_rfc3339_opts(SecondsFormat::Secs, false);
        let records = vec![
            rec(&old, 10.0, 11.0),
            rec(&edge, 20.0, 21.0),
            rec(&recent, 30.0, 31.0),
        ];
        let out = build_output(&records, "now", now());
        assert_eq!(out.history.len(), 2);
        assert_eq!(out.history[0].five_hour_pct, Some(20.0));
        assert_eq!(out.history[1].five_hour_pct, Some(30.0));
        assert_eq!(out.history_days, 60);
        // 切り詰めても latest は最新のまま．
        assert_eq!(out.latest.unwrap()["fetched_at"], Value::String(recent));
    }

    #[test]
    fn history_は古い順に並び直される() {
        let records = vec![
            rec("2026-08-29T03:00:00+00:00", 3.0, 30.0),
            rec("2026-08-29T01:00:00+00:00", 1.0, 10.0),
            rec("2026-08-29T02:00:00+00:00", 2.0, 20.0),
        ];
        let out = build_output(&records, "now", now());
        let pcts: Vec<_> = out.history.iter().map(|h| h.five_hour_pct).collect();
        assert_eq!(pcts, vec![Some(1.0), Some(2.0), Some(3.0)]);
        // latest は «最後の行» ではなく «fetched_at が最大» のもの．
        assert_eq!(
            out.latest.unwrap()["fetched_at"],
            "2026-08-29T03:00:00+00:00"
        );
    }

    #[test]
    fn 時刻の読めないレコードは_history_から外す() {
        let records = vec![
            json!({"five_hour": {"utilization": 9.0}}),
            json!({"fetched_at": "きのう", "five_hour": {"utilization": 8.0}}),
            rec("2026-08-29T02:00:00+00:00", 2.0, 20.0),
        ];
        let out = build_output(&records, "now", now());
        assert_eq!(out.history.len(), 1);
        assert_eq!(out.history[0].five_hour_pct, Some(2.0));
    }

    #[test]
    fn 記録が一件も無ければ_latest_は_null() {
        let out = build_output(&[], "now", now());
        assert!(out.latest.is_none());
        assert!(out.history.is_empty());
        let v = serde_json::to_value(&out).unwrap();
        assert!(v["latest"].is_null());
    }

    #[test]
    fn 時刻は秒まで丸められ_読めないものはそのまま残る() {
        assert_eq!(
            normalize_ts("2026-08-29T17:40:00.364259+00:00"),
            "2026-08-29T17:40:00+00:00"
        );
        assert_eq!(
            normalize_ts("2026-08-29T17:40:00Z"),
            "2026-08-29T17:40:00+00:00"
        );
        assert_eq!(normalize_ts("いつか"), "いつか");
    }

    #[test]
    fn 契約どおりの形で直列化される() {
        let records = vec![to_record(BODY, "2026-08-29T14:17:47+00:00").unwrap()];
        let out = build_output(&records, "2026-08-29T23:20:00+09:00", now());
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["updated_at"], "2026-08-29T23:20:00+09:00");
        assert_eq!(v["source"], "GET https://api.anthropic.com/api/oauth/usage");
        assert!(v["note"].as_str().unwrap().contains("アカウント全体"));
        let h = &v["history"][0];
        assert_eq!(h["fetched_at"], "2026-08-29T14:17:47+00:00");
        assert_eq!(h["five_hour_pct"], 29.0);
        assert_eq!(h["five_hour_resets_at"], "2026-08-29T17:40:00+00:00");
        assert_eq!(h["seven_day_pct"], 46.0);
        assert_eq!(h["seven_day_resets_at"], "2026-09-02T05:00:01+00:00");
        // Max 20x では null が返る．0 で埋めない．
        assert!(h["seven_day_opus_pct"].is_null());
        assert!(h["seven_day_sonnet_pct"].is_null());
        // latest は生のまま (未知の鍵を含む)．
        assert!(v["latest"]["iguana_necktie"].is_null());
        assert_eq!(v["latest"]["limits"][0]["kind"], "session");
    }

    #[test]
    fn 書き出しは一時ファイル経由で行われ後始末される() {
        let dir = crate::claude::aggregate::tests::tmpdir("limits-write");
        let out = build_output(&[], "now", now());
        let path = write(&dir, &out).unwrap();
        assert_eq!(path.file_name().unwrap(), "_limits.json");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        let _: Value = serde_json::from_str(&text).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|x| x.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 取得時刻は秒までの_utc_で作る() {
        let t = DateTime::parse_from_rfc3339("2026-08-29T14:17:47.423281+00:00").unwrap();
        assert_eq!(
            now_fetched_at(t.with_timezone(&Utc)),
            "2026-08-29T14:17:47+00:00"
        );
    }
}
