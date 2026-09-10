use ai_subscription_usage::codex::{aggregate_file, parse_line, ParseOutcome, Scope};

#[test]
fn fixture_counts_only_usage_records_and_deduplicates_response_id() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/basic.jsonl");
    let result = aggregate_file(&path).unwrap();
    assert_eq!(result.broken_lines, 1);
    assert_eq!(result.records.len(), 1);
    let record = &result.records[0];
    assert_eq!(record.event_id, "resp-1");
    assert_eq!(record.session_id, "thread-main");
    assert_eq!(record.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(record.scope, Scope::Main);
    assert_eq!(record.tokens.input, 100);
    assert_eq!(record.tokens.cached_input, 40);
    assert_eq!(record.tokens.output, 30);
    assert_eq!(record.tokens.reasoning_output, 10);
    assert_eq!(record.tokens.cache_writes.len(), 1);
    assert_eq!(record.tokens.cache_writes.values().next(), Some(&0));
}

#[test]
fn usage_inner_tokens_are_exposed_as_exclusive_breakdown() {
    let line = r#"{"timestamp":"2026-09-10T01:00:00Z","type":"token_usage_record","payload":{"session_id":"s","response_id":"r","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":30,"reasoning_output_tokens":10}}}"#;
    let ParseOutcome::Record(record) = parse_line(line, None, None) else {
        panic!("expected a record");
    };
    assert_eq!(record.tokens.input - record.tokens.cached_input, 60);
    assert_eq!(record.tokens.output - record.tokens.reasoning_output, 20);
}

#[test]
fn underflow_is_reported_but_raw_values_are_retained() {
    let line = r#"{"timestamp":"2026-09-10T01:00:00Z","type":"token_usage_record","payload":{"session_id":"s","response_id":"r","usage":{"input_tokens":2,"cached_input_tokens":5,"output_tokens":3,"reasoning_output_tokens":8}}}"#;
    let ParseOutcome::Anomalous { record, .. } = parse_line(line, None, None) else {
        panic!("expected an anomalous record");
    };
    assert_eq!(record.tokens.input, 2);
    assert_eq!(record.tokens.cached_input, 5);
}

#[test]
fn unknown_model_and_subagent_source_are_preserved() {
    let line = r#"{"timestamp":"2026-09-10T01:00:00Z","type":"token_usage_record","payload":{"session_id":"s","response_id":"r","source":"subagent","usage":{"input_tokens":1,"output_tokens":2}}}"#;
    let ParseOutcome::Record(record) = parse_line(line, None, None) else {
        panic!("expected a record");
    };
    assert_eq!(record.model, None);
    assert_eq!(record.scope, Scope::Subagent);
}

#[test]
fn event_msg_token_count_is_not_a_usage_record() {
    let line = r#"{"timestamp":"2026-09-10T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":200}}}}"#;
    assert!(!matches!(
        parse_line(line, None, None),
        ParseOutcome::Record(_) | ParseOutcome::Anomalous { .. }
    ));
}
