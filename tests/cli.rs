use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn fixture_env() -> (TempDir, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let output = vault.path().join("_logs/_ai-subscription-usage/claude");
    fs::create_dir_all(home.path().join(".claude/projects/example")).unwrap();
    fs::create_dir_all(&output).unwrap();
    fs::write(
        output.join("pricing.json"),
        r#"{"models":{"claude-opus-5":{"input":5.0,"output":25.0,"cache_write_5m":6.25,"cache_write_1h":10.0,"cache_read":0.5}}}"#,
    )
    .unwrap();
    fs::write(
        home.path().join(".claude/projects/example/session.jsonl"),
        r#"{"type":"assistant","timestamp":"2026-08-22T01:00:00Z","message":{"id":"m1","model":"claude-opus-5","usage":{"input_tokens":10,"output_tokens":20}}}"#,
    )
    .unwrap();
    (home, vault)
}

fn write_limit(vault: &TempDir) {
    fs::write(
        vault
            .path()
            .join("_logs/_ai-subscription-usage/claude/limit.json"),
        r#"{"version":3,"reset":{"weekday":3,"hour":14},"meters":{"all_models":{}},"observations":[{"at":"2026-08-22T12:00:00+09:00","meter":"all_models","pct":10}],"limit":{"cost_usd_base":100}}"#,
    )
    .unwrap();
}

#[test]
fn claude_subcommand_accepts_legacy_aggregation_flags() {
    let (home, vault) = fixture_env();
    fs::create_dir_all(home.path().join(".codex/sessions")).unwrap();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args([
            "claude",
            "--all",
            "--month",
            "2026-08",
            "--dry-run",
            "--forget-missing",
        ])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("走査ファイル数"))
        .stdout(predicate::str::contains(
            "--dry-run のため何も書き出していません",
        ));
}

#[test]
fn claude_normal_run_writes_monthly_json_and_state() {
    let (home, vault) = fixture_env();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["claude", "--month", "2026-08"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote"));

    let output = vault
        .path()
        .join("_logs/_ai-subscription-usage/claude/2026-08.json");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(json["provider"], "claude");
    assert_eq!(json["days"][0]["entries"][0]["output"], 20);
    assert!(home
        .path()
        .join(".local/share/ai-subscription-usage/claude/state.json")
        .is_file());
}

#[test]
fn claude_dry_run_does_not_write_state_or_monthly_json() {
    let (home, vault) = fixture_env();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["claude", "--month", "2026-08", "--dry-run"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success();

    assert!(!vault
        .path()
        .join("_logs/_ai-subscription-usage/claude/2026-08.json")
        .exists());
    assert!(!home
        .path()
        .join(".local/share/ai-subscription-usage/claude/state.json")
        .exists());
}

#[test]
fn claude_normal_run_writes_windows_when_limit_is_configured() {
    let (home, vault) = fixture_env();
    write_limit(&vault);
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["claude", "--month", "2026-08"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote"));

    let windows = vault
        .path()
        .join("_logs/_ai-subscription-usage/claude/_windows.json");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(windows).unwrap()).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["base_limit"]["unit"], "cost_usd");
}

#[test]
fn provider_is_required() {
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn codex_subcommand_is_available() {
    let (home, vault) = fixture_env();
    fs::create_dir_all(home.path().join(".codex/sessions")).unwrap();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["codex", "--month", "2026-08", "--dry-run"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success();
}

#[test]
fn claude_migrates_legacy_output_without_overwrite() {
    let (home, vault) = fixture_env();
    let old = vault.path().join("_logs/_claude-usage");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("2026-07.json"), "legacy").unwrap();
    fs::write(old.join("2026-06.json"), "legacy-only").unwrap();
    let new = vault.path().join("_logs/_ai-subscription-usage/claude");
    fs::write(new.join("2026-07.json"), "new").unwrap();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["claude", "--month", "2026-08"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success();
    assert_eq!(fs::read_to_string(new.join("2026-07.json")).unwrap(), "new");
    assert_eq!(
        fs::read_to_string(new.join("2026-06.json")).unwrap(),
        "legacy-only"
    );
}
