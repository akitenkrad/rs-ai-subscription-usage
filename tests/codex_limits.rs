//! Codex の利用制限 (rate limits) の記録と出力．
//!
//! fixture は実ログの形をそのまま使う (値だけを丸めてある)．
//! 2026-09-10 に実ログ 985 件を数えて分かった 3 パターンを全部持たせてあり，
//! **CLI が primary / secondary を «5 時間枠» «週次枠» と解釈していない**ことを
//! ここで固定する．

use ai_subscription_usage::codex::limits::{
    append_jsonl, build_output, new_observations, read_jsonl, write, HISTORY_DAYS,
};
use ai_subscription_usage::codex::{
    aggregate_file, parse_line, ParseOutcome, RateLimitsObservation,
};
use chrono::{FixedOffset, TimeZone};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/rate_limits.jsonl")
}

/// 実ログの 1 行から観測値を作る．テストの中で構造体を手で組み立てないのは，
/// «ログの形が変わったら気づく» ようにするため．
fn observation(line: &str) -> RateLimitsObservation {
    match parse_line(line, None, None) {
        ParseOutcome::RateLimits(o) => o,
        other => panic!("観測値になりませんでした: {other:?}"),
    }
}

fn line_at(observed_at: &str, primary_pct: f64, window_minutes: u64) -> String {
    format!(
        r#"{{"timestamp":"{observed_at}","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"limit_id":"codex","primary":{{"used_percent":{primary_pct},"window_minutes":{window_minutes},"resets_at":1789614195}},"secondary":null,"plan_type":"prolite"}}}}}}"#
    )
}

fn jst(y: i32, m: u32, d: u32) -> chrono::DateTime<FixedOffset> {
    FixedOffset::east_opt(9 * 3600)
        .unwrap()
        .with_ymd_and_hms(y, m, d, 12, 0, 0)
        .unwrap()
}

#[test]
fn three_observed_plan_shapes_are_read_with_window_minutes_intact() {
    let result = aggregate_file(&fixture()).unwrap();
    // 壊れた行が 1 行あっても，他の 3 件は拾える．
    assert_eq!(result.broken_lines, 1);
    let all = &result.rate_limits_all;
    assert_eq!(all.len(), 3);

    // plus / primary=10080 (週次) / secondary なし
    assert_eq!(all[0].plan_type.as_deref(), Some("plus"));
    assert_eq!(all[0].primary.as_ref().unwrap().window_minutes, 10080);
    assert!(all[0].secondary.is_none());

    // plus / primary=300 (5 時間) + secondary=10080 (週次)
    assert_eq!(all[1].primary.as_ref().unwrap().window_minutes, 300);
    assert_eq!(all[1].secondary.as_ref().unwrap().window_minutes, 10080);

    // prolite / primary=10080 (週次) / 5 時間枠そのものが無い
    assert_eq!(all[2].plan_type.as_deref(), Some("prolite"));
    assert_eq!(all[2].primary.as_ref().unwrap().window_minutes, 10080);
    assert_eq!(all[2].primary.as_ref().unwrap().used_percent, 1.0);
    assert_eq!(all[2].primary.as_ref().unwrap().resets_at, 1789614195);
    assert!(all[2].secondary.is_none());

    // 1 ファイルの中の «最新» は最後の観測．
    assert_eq!(result.rate_limits.unwrap().observed_at, all[2].observed_at);
}

#[test]
fn output_records_the_observation_verbatim_without_labelling_windows() {
    let all = aggregate_file(&fixture()).unwrap().rate_limits_all;
    let out = build_output(&all, "now", jst(2026, 9, 10)).expect("観測が 3 件ある");
    let json = serde_json::to_value(&out).unwrap();

    // 最新 (prolite) の観測値がそのまま出ている．primary は 10080 のまま，
    // «週次だから secondary へ» のような席の入れ替えをしない．
    assert_eq!(json["plan_type"], "prolite");
    assert_eq!(json["rate_limits"]["primary"]["window_minutes"], 10080);
    assert_eq!(json["rate_limits"]["primary"]["used_percent"], 1.0);
    // resets_at は Unix 秒のまま．0 や現在時刻で埋めない．
    assert_eq!(json["rate_limits"]["primary"]["resets_at"], 1789614195i64);
    assert!(json["rate_limits"]["secondary"].is_null());
    assert_eq!(json["credits"]["balance"], "0");
    assert_eq!(json["provider"], "codex");
    assert_eq!(json["version"], 1);

    // 枠の意味を表す鍵を CLI 側で作っていないこと (画面が window_minutes で判定する)．
    let text = serde_json::to_string(&json).unwrap();
    for forbidden in ["five_hour", "weekly", "seven_day", "week_pct", "5h"] {
        assert!(
            !text.contains(forbidden),
            "CLI が枠を解釈しています: {forbidden}"
        );
    }

    // history も観測値をそのまま平らにしただけ．secondary が無い行は null．
    assert_eq!(json["history"][0]["primary_window_minutes"], 10080);
    assert_eq!(json["history"][1]["primary_window_minutes"], 300);
    assert_eq!(json["history"][1]["secondary_window_minutes"], 10080);
    assert!(json["history"][2]["secondary_pct"].is_null());
}

#[test]
fn appending_the_same_log_twice_adds_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.jsonl");
    let all = aggregate_file(&fixture()).unwrap().rate_limits_all;

    for _ in 0..3 {
        let stored = read_jsonl(&path).unwrap();
        let fresh = new_observations(&stored.observations, &all);
        append_jsonl(&path, &fresh).unwrap();
    }
    let stored = read_jsonl(&path).unwrap();
    assert_eq!(stored.observations.len(), 3);
    assert_eq!(stored.broken_lines, 0);
    // 追記したものを読み戻しても観測値は変わらない．
    assert_eq!(stored.observations[2].plan_type.as_deref(), Some("prolite"));
    assert_eq!(
        stored.observations[2].primary.as_ref().unwrap().resets_at,
        1789614195
    );
}

#[test]
fn latest_is_the_max_observed_at_not_the_last_scanned() {
    let a = observation(&line_at("2026-09-01T00:00:00Z", 10.0, 10080));
    let b = observation(&line_at("2026-09-10T00:00:00Z", 42.0, 10080));
    let c = observation(&line_at("2026-09-05T00:00:00Z", 20.0, 10080));

    let forward =
        build_output(&[a.clone(), b.clone(), c.clone()], "now", jst(2026, 9, 10)).unwrap();
    let shuffled = build_output(&[c, b, a], "now", jst(2026, 9, 10)).unwrap();

    assert_eq!(forward.observed_at, "2026-09-10T00:00:00+00:00");
    assert_eq!(
        forward.rate_limits.primary.as_ref().unwrap().used_percent,
        42.0
    );
    // 走査順が変わっても最新が後戻りしない．
    assert_eq!(shuffled.observed_at, forward.observed_at);
    assert_eq!(
        shuffled
            .history
            .iter()
            .map(|h| h.primary_pct)
            .collect::<Vec<_>>(),
        forward
            .history
            .iter()
            .map(|h| h.primary_pct)
            .collect::<Vec<_>>()
    );
}

#[test]
fn history_is_cut_at_sixty_days_but_the_jsonl_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.jsonl");
    let old = observation(&line_at("2026-01-01T00:00:00Z", 1.0, 10080));
    let recent = observation(&line_at("2026-09-09T00:00:00Z", 2.0, 10080));
    append_jsonl(&path, &[old.clone(), recent.clone()]).unwrap();

    let stored = read_jsonl(&path).unwrap();
    // JSONL 側は全期間を持つ．
    assert_eq!(stored.observations.len(), 2);

    let out = build_output(&stored.observations, "now", jst(2026, 9, 10)).unwrap();
    assert_eq!(out.history_days, HISTORY_DAYS);
    assert_eq!(out.history.len(), 1);
    assert_eq!(out.history[0].primary_pct, Some(2.0));
    // 切り捨てても «最新» は変わらない．
    assert_eq!(out.observed_at, "2026-09-09T00:00:00+00:00");
}

#[test]
fn no_observation_produces_no_output_at_all() {
    assert!(build_output(&[], "now", jst(2026, 9, 10)).is_none());
}

#[test]
fn broken_jsonl_rows_do_not_hide_the_other_observations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.jsonl");
    let good = observation(&line_at("2026-09-09T00:00:00Z", 2.0, 10080));
    append_jsonl(&path, &[good]).unwrap();
    // 途中で切れた行を挟む (前回の追記が落ちた状態を模す)．
    std::fs::write(
        &path,
        format!(
            "{}{{\"observed_at\":\"2026-09-0",
            std::fs::read_to_string(&path).unwrap()
        ),
    )
    .unwrap();
    let newer = observation(&line_at("2026-09-10T00:00:00Z", 3.0, 10080));
    // 壊れた行の尻に繋げず，改行を入れてから追記する．
    append_jsonl(&path, &[newer]).unwrap();

    let stored = read_jsonl(&path).unwrap();
    assert_eq!(stored.broken_lines, 1);
    assert_eq!(stored.observations.len(), 2);
    let out = build_output(&stored.observations, "now", jst(2026, 9, 10)).unwrap();
    assert_eq!(out.rate_limits.primary.unwrap().used_percent, 3.0);
}

#[test]
fn a_failed_write_leaves_the_existing_limits_json_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("_limits.json");
    let first = observation(&line_at("2026-09-09T00:00:00Z", 2.0, 10080));
    let out = build_output(&[first], "first", jst(2026, 9, 10)).unwrap();
    write(dir.path(), &out).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    // 一時ファイルの置き場をディレクトリで塞ぎ，rename の前で失敗させる．
    std::fs::create_dir(dir.path().join("_limits.json.tmp")).unwrap();
    let second = observation(&line_at("2026-09-10T00:00:00Z", 99.0, 10080));
    let out2 = build_output(&[second], "second", jst(2026, 9, 10)).unwrap();
    assert!(write(dir.path(), &out2).is_err());

    // 途中で失敗しても，既に置いてあるファイルは前の内容のまま．
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    let json: serde_json::Value = serde_json::from_str(&before).unwrap();
    assert_eq!(json["rate_limits"]["primary"]["used_percent"], 2.0);
}

// ── `history` の間引き ─────────────────────────────────────
//
// 観測は «Codex を叩いた回数» ぶん出るので (実測で 3 週間 985 点)，全部載せると
// `_limits.json` が 262 KB になり，**画面を開くたびに読む**ファイルとしては重い．
// 1 日 1 点へ落とす．JSONL 側は全点を持ったままである．

#[test]
fn history_keeps_one_point_per_day() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.jsonl");
    // 同じ日に 3 点，翌日に 2 点．
    let obs: Vec<_> = [
        ("2026-09-08T01:00:00Z", 10.0),
        ("2026-09-08T05:00:00Z", 40.0),
        ("2026-09-08T09:00:00Z", 25.0),
        ("2026-09-09T02:00:00Z", 50.0),
        ("2026-09-09T06:00:00Z", 55.0),
    ]
    .iter()
    .map(|(t, p)| observation(&line_at(t, *p, 10080)))
    .collect();
    append_jsonl(&path, &obs).unwrap();

    let stored = read_jsonl(&path).unwrap();
    assert_eq!(stored.observations.len(), 5, "JSONL は全点を持つ");

    let out = build_output(&stored.observations, "now", jst(2026, 9, 10)).unwrap();
    assert_eq!(out.history.len(), 2, "1 日 1 点に落ちる");
    // 残るのは «その日いちばん高かった点»．平均でも最後の値でもない．
    assert_eq!(out.history[0].primary_pct, Some(40.0));
    assert_eq!(out.history[1].primary_pct, Some(55.0));
}

#[test]
fn the_latest_point_survives_even_when_it_is_not_the_days_peak() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.jsonl");
    // 枠がリセットされ，その日の最大 (80%) より後に低い値 (3%) を観測した状況．
    let obs: Vec<_> = [
        ("2026-09-09T01:00:00Z", 80.0),
        ("2026-09-09T23:00:00Z", 3.0),
    ]
    .iter()
    .map(|(t, p)| observation(&line_at(t, *p, 10080)))
    .collect();
    append_jsonl(&path, &obs).unwrap();

    let out = build_output(&read_jsonl(&path).unwrap().observations, "now", jst(2026, 9, 10)).unwrap();
    // 画面の «いま 3%» と折れ線の右端が食い違ってはいけない．
    assert_eq!(out.observed_at, "2026-09-09T23:00:00+00:00");
    assert_eq!(
        out.history.last().unwrap().primary_pct,
        Some(3.0),
        "最新の点は，その日の最大でなくても残す"
    );
}

#[test]
fn days_are_cut_on_jst_not_utc() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.jsonl");
    // どちらも JST では 2026-09-09．UTC で切ると 2 日に分かれてしまう．
    let obs: Vec<_> = [
        ("2026-09-08T16:00:00Z", 10.0), // JST 09-09 01:00
        ("2026-09-09T05:00:00Z", 20.0), // JST 09-09 14:00
    ]
    .iter()
    .map(|(t, p)| observation(&line_at(t, *p, 10080)))
    .collect();
    append_jsonl(&path, &obs).unwrap();

    let out = build_output(&read_jsonl(&path).unwrap().observations, "now", jst(2026, 9, 10)).unwrap();
    assert_eq!(out.history.len(), 1, "JST の同じ日は 1 点にまとまる");
    assert_eq!(out.history[0].primary_pct, Some(20.0));
}

#[test]
fn the_whole_rate_limits_object_is_kept_for_later() {
    // 型に無い欄 (`spend_control_reached` など) は，ログが刈られたら二度と取れない．
    let line = r#"{"timestamp":"2026-09-09T01:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","limit_name":"weekly","primary":{"used_percent":1.0,"window_minutes":10080,"resets_at":1789614195},"secondary":null,"plan_type":"prolite","spend_control_reached":true,"rate_limit_reached_type":"weekly"}}}"#;
    let o = observation(line);
    let raw = o.raw.as_ref().expect("生の rate_limits を抱えていない");
    assert_eq!(raw.get("spend_control_reached").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(raw.get("rate_limit_reached_type").and_then(|v| v.as_str()), Some("weekly"));
    assert_eq!(raw.get("limit_name").and_then(|v| v.as_str()), Some("weekly"));

    // JSONL を往復しても失われないこと．
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.jsonl");
    append_jsonl(&path, &[o]).unwrap();
    let back = &read_jsonl(&path).unwrap().observations[0];
    assert_eq!(
        back.raw.as_ref().unwrap().get("spend_control_reached").and_then(|v| v.as_bool()),
        Some(true),
        "長期記録を読み直したら未知の欄が消えていた"
    );
}

// ── 走査済みファイルからの取りこぼし ─────────────────────────
//
// 2026-09-10 の実測で見つかった穴．月次集計だけを先に走らせた state が既にあると，
// «mtime も size も変わっていない» ファイルは読み飛ばされ，そのファイルに入っていた
// 観測値が長期記録へ渡らない (985 点のうち 321 点しか入らなかった)．
// 元のログは刈られるので，取りこぼすと後から復元できない．

#[test]
fn a_file_already_scanned_before_limits_existed_is_read_once_more() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let vault = dir.path().join("vault");
    let sessions = home.join(".codex/sessions/2026/09/09");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    let log = sessions.join("rollout-2026-09-09T01-00-00-test.jsonl");
    std::fs::write(
        &log,
        format!(
            "{}\n{}\n",
            line_at("2026-09-09T01:00:00Z", 10.0, 10080),
            line_at("2026-09-09T02:00:00Z", 20.0, 10080)
        ),
    )
    .unwrap();

    let run = |args: Vec<&str>| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ai-subscription-usage"));
        cmd.env("HOME", &home).env("OBSIDIAN_VAULT", &vault);
        cmd.args(args);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };

    // 1) 利用制限を知らなかった頃と同じ状態を作る — 月次集計だけを走らせ，
    //    そのあと «まだ落としていない» 印を消す (古い state の再現)．
    run(vec!["codex"]);
    let state_path = home.join(".local/share/ai-subscription-usage/codex/state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    for f in state["files"].as_object_mut().unwrap().values_mut() {
        f.as_object_mut().unwrap().remove("limits_harvested");
    }
    std::fs::write(&state_path, serde_json::to_string(&state).unwrap()).unwrap();
    let jsonl = home.join(".local/share/ai-subscription-usage/codex/limits.jsonl");
    std::fs::remove_file(&jsonl).ok();

    // 2) ファイルは 1 バイトも変わっていないが，観測は **2 点とも** 拾えること．
    run(vec!["codex", "limits"]);
    let lines = std::fs::read_to_string(&jsonl).unwrap();
    assert_eq!(
        lines.lines().count(),
        2,
        "読み飛ばして最新 1 点しか拾えていない: {lines}"
    );

    // 3) 2 度目からは読み飛ばしてよい (印が付いたので行は増えない)．
    run(vec!["codex", "limits"]);
    assert_eq!(std::fs::read_to_string(&jsonl).unwrap().lines().count(), 2);
}
