use ai_subscription_usage::claude::limit::Limit;
use ai_subscription_usage::claude::limits_api::{build_output, to_record};
use ai_subscription_usage::claude::windows::{build_opt, reset_in_jst};
use ai_subscription_usage::claude::{aggregate::AggEntry, pricing::Pricing, transcript::Scope};
use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use serde_json::json;

#[test]
fn limits_record_preserves_unknown_keys_and_fetched_at() {
    let body = r#"{"five_hour":{"utilization":29.0},"unknown_meter":{"x":1}}"#;
    let record = to_record(body, "2026-09-10T00:00:00Z").expect("fixture is valid JSON");
    assert_eq!(record["fetched_at"], "2026-09-10T00:00:00Z");
    assert_eq!(record["unknown_meter"]["x"], 1);
}

#[test]
fn limits_output_keeps_recent_history_and_latest_by_timestamp() {
    let records = vec![
        json!({"fetched_at":"2026-07-01T00:00:00+09:00","five_hour":{"utilization":10.0}}),
        json!({"fetched_at":"2026-09-10T01:00:00+09:00","five_hour":{"utilization":20.0}}),
        json!({"fetched_at":"2026-09-09T01:00:00+09:00","five_hour":{"utilization":15.0}}),
    ];
    let now = FixedOffset::east_opt(9 * 3600)
        .unwrap()
        .with_ymd_and_hms(2026, 9, 10, 2, 0, 0)
        .unwrap();
    let output = build_output(&records, "now", now);
    assert_eq!(
        output.latest.as_ref().unwrap()["five_hour"]["utilization"],
        20.0
    );
    assert_eq!(output.history.len(), 2);
    assert_eq!(output.history[0].five_hour_pct, Some(15.0));
}

#[test]
fn windows_build_uses_jst_reset_and_writes_cost_meter() {
    assert_eq!(reset_in_jst(3, 14, 0, 0), (3, 23 * 60));
    let limit: Limit = serde_json::from_value(json!({
        "version": 3,
        "reset": {"weekday": 3, "hour": 14, "minute": 0, "tz": "+09:00"},
        "meters": {"all_models": {"label": "all"}},
        "observations": [{"at": "2026-09-10T10:00:00+09:00", "meter": "all_models", "pct": 10.0}],
        "limit": {"cost_usd_base": 100.0}
    }))
    .unwrap();
    let pricing: Pricing = serde_json::from_value(json!({
        "aliases": {},
        "models": {"claude-sonnet-5": {"input": 1.0, "output": 1.0, "cache_write_5m": 1.0, "cache_write_1h": 1.0, "cache_read": 1.0}}
    })).unwrap();
    let entry = AggEntry {
        date: "2026-09-10".into(),
        hour: Some(10),
        model: "claude-sonnet-5".into(),
        scope: Scope::Main,
        requests: 1,
        input: 0,
        output: 1_000_000,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_read: 0,
    };
    let output = build_opt(
        std::slice::from_ref(&entry),
        &[],
        &pricing,
        Some(&limit),
        NaiveDateTime::parse_from_str("2026-09-10 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap(),
        "now",
    )
    .expect("observation enables windows");
    assert_eq!(output.base_limit.unit, "cost_usd");
    assert_eq!(output.windows.len(), 1);
    assert_eq!(output.windows[0].meters["all_models"].cost_usd, 1.0);
}

#[test]
fn windows_are_skipped_without_observations() {
    let limit: Limit = serde_json::from_value(json!({
        "version": 3, "reset": {"weekday": 3, "hour": 14}, "meters": {"all_models": {}},
        "observations": [], "limit": {"cost_usd_base": 100.0}
    }))
    .unwrap();
    let pricing: Pricing = serde_json::from_value(json!({"aliases":{},"models":{}})).unwrap();
    assert!(build_opt(
        std::iter::empty::<&AggEntry>(),
        &[],
        &pricing,
        Some(&limit),
        NaiveDateTime::parse_from_str("2026-09-10 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap(),
        "now"
    )
    .is_none());
}
