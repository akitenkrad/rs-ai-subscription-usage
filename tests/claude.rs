use ai_subscription_usage::claude::{
    aggregate::aggregate_file, output::build_months, pricing::Pricing,
};

#[test]
fn claude_fixture_matches_legacy_monthly_contract() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude");
    let aggregation = aggregate_file(&fixture.join("transcript.jsonl")).unwrap();
    assert_eq!(aggregation.broken_lines, 1);
    assert_eq!(aggregation.agg.len(), 2);

    let pricing = Pricing::load(&fixture.join("pricing.json")).unwrap();
    let months = build_months(&aggregation.agg, &pricing, "2026-09-10T12:00:00+09:00");
    let output = serde_json::to_value(&months["2026-08"]).unwrap();

    assert_eq!(output["version"], 1);
    assert_eq!(output["provider"], "claude");
    assert_eq!(output["month"], "2026-08");
    assert_eq!(output["days"][0]["date"], "2026-08-22");
    assert_eq!(output["days"][0]["entries"][0]["requests"], 1);
    assert_eq!(output["days"][0]["entries"][0]["input"], 100);
    assert_eq!(output["days"][0]["entries"][0]["cache_write_5m"], 40);
    assert_eq!(output["days"][0]["entries"][0]["cache_write_1h"], 50);
    assert_eq!(output["days"][1]["entries"][0]["scope"], "subagent");
}

#[test]
fn stable_message_ids_make_reaggregation_idempotent() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude/transcript.jsonl");
    let first = aggregate_file(&fixture).unwrap();
    let second = aggregate_file(&fixture).unwrap();
    assert_eq!(first.agg, second.agg);
}
