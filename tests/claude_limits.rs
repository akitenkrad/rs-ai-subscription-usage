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

#[cfg(unix)]
mod expired_credentials {
    use assert_cmd::Command;
    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new(oauth: Value) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("bin");
            fs::create_dir_all(&bin).unwrap();
            let security = bin.join("security");
            fs::write(
                &security,
                "#!/bin/sh\n/bin/cat \"$TEST_CLAUDE_CREDENTIALS\"\n",
            )
            .unwrap();
            fs::set_permissions(&security, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                dir.path().join("credentials.json"),
                json!({"claudeAiOauth": oauth}).to_string(),
            )
            .unwrap();
            Self { dir }
        }

        fn output_dir(&self) -> std::path::PathBuf {
            self.dir
                .path()
                .join("vault/_logs/_ai-subscription-usage/claude")
        }

        fn history_path(&self) -> std::path::PathBuf {
            self.dir
                .path()
                .join("home/.local/share/ai-subscription-usage/claude/limits.jsonl")
        }

        fn command(&self) -> Command {
            let mut command = Command::cargo_bin("ai-subscription-usage").unwrap();
            command
                .args(["claude", "limits"])
                .env("HOME", self.dir.path().join("home"))
                .env("OBSIDIAN_VAULT", self.dir.path().join("vault"))
                .env("PATH", self.dir.path().join("bin"))
                .env(
                    "TEST_CLAUDE_CREDENTIALS",
                    self.dir.path().join("credentials.json"),
                );
            command
        }
    }

    fn refreshable_oauth() -> Value {
        json!({
            "accessToken": "dummy-access-SECRET",
            "expiresAt": 1_000,
            "refreshToken": "dummy-refresh-SECRET",
            "refreshTokenExpiresAt": i64::MAX,
        })
    }

    #[test]
    fn temporary_expiration_succeeds_on_stdout_and_preserves_previous_files() {
        for unknown_refresh_expiry in [false, true] {
            let mut oauth = refreshable_oauth();
            if unknown_refresh_expiry {
                oauth
                    .as_object_mut()
                    .unwrap()
                    .remove("refreshTokenExpiresAt");
            }
            let fixture = Fixture::new(oauth);
            let output_dir = fixture.output_dir();
            let output_path = output_dir.join("_limits.json");
            let history_path = fixture.history_path();
            let previous_output = b"{\"latest\":{\"five_hour\":{\"utilization\":29.0}}}\n";
            let previous_history = b"{\"fetched_at\":\"2026-09-10T00:00:00Z\"}\n";
            fs::create_dir_all(&output_dir).unwrap();
            fs::create_dir_all(history_path.parent().unwrap()).unwrap();
            fs::write(&output_path, previous_output).unwrap();
            fs::write(&history_path, previous_history).unwrap();
            let output_modified = fs::metadata(&output_path).unwrap().modified().unwrap();
            let history_modified = fs::metadata(&history_path).unwrap().modified().unwrap();

            for _ in 0..2 {
                fixture.command().assert().success().stderr("").stdout(
                    "アクセストークンが失効しています (1970-01-01T09:00:01+09:00)．Claude Code を起動すると更新されます．今回の実測取得は見送り，前回の _limits.json をそのまま残します．\n",
                );
                assert_eq!(fs::read(&output_path).unwrap(), previous_output);
                assert_eq!(fs::read(&history_path).unwrap(), previous_history);
                assert_eq!(
                    fs::metadata(&output_path).unwrap().modified().unwrap(),
                    output_modified
                );
                assert_eq!(
                    fs::metadata(&history_path).unwrap().modified().unwrap(),
                    history_modified
                );
                assert_eq!(fs::read_dir(&output_dir).unwrap().count(), 1);
                assert_eq!(
                    fs::read_dir(history_path.parent().unwrap())
                        .unwrap()
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn temporary_expiration_does_not_create_output_or_history() {
        let fixture = Fixture::new(refreshable_oauth());
        fixture.command().assert().success().stderr("");
        assert!(!fixture.output_dir().exists());
        assert!(!fixture.history_path().parent().unwrap().exists());
    }

    #[test]
    fn expiration_without_usable_refresh_token_fails_on_stderr() {
        let cases = [
            json!({"accessToken":"dummy-access-SECRET", "expiresAt":1_000}),
            json!({"accessToken":"dummy-access-SECRET", "expiresAt":1_000, "refreshToken":""}),
            json!({"accessToken":"dummy-access-SECRET", "expiresAt":1_000, "refreshToken":"dummy-refresh-SECRET", "refreshTokenExpiresAt":1_000}),
        ];
        for oauth in cases {
            let fixture = Fixture::new(oauth);
            fixture.command().assert().code(1).stdout("").stderr(
                "[ERROR] Claudeの認証情報の有効期限が切れています (1970-01-01T09:00:01+09:00)．リフレッシュトークンも使えないため，Claude Code を起動して /login し直してください．\n",
            );
            assert!(!fixture.output_dir().exists());
            assert!(!fixture.history_path().parent().unwrap().exists());
        }
    }

    #[test]
    fn unrepresentable_expiration_does_not_panic() {
        let mut oauth = refreshable_oauth();
        oauth["expiresAt"] = json!(i64::MIN);
        let fixture = Fixture::new(oauth);
        fixture
            .command()
            .assert()
            .success()
            .stderr("")
            .stdout(predicates::str::contains(
                "アクセストークンが失効しています",
            ));
        assert!(!fixture.output_dir().exists());
        assert!(!fixture.history_path().parent().unwrap().exists());
    }
}
