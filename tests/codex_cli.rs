use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn codex_fixture() -> (TempDir, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/10");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-test.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-09-10T01:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"thread-main\",\"thread_source\":\"user\"}}\n",
            "{\"timestamp\":\"2026-09-10T01:00:01Z\",\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\"}}\n",
            "{\"timestamp\":\"2026-09-10T01:00:02Z\",\"type\":\"token_usage_record\",\"payload\":{\"session_id\":\"thread-main\",\"response_id\":\"resp-1\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":40,\"output_tokens\":30,\"reasoning_output_tokens\":10}}}\n",
            "{\"timestamp\":\"2026-09-10T01:00:03Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":9999}},\"rate_limits\":{\"primary\":{\"used_percent\":12.5,\"window_minutes\":300,\"resets_at\":1789021600}}}}\n",
            "{\"timestamp\":\"2026-09-10T01:00:04Z\",\"type\":\"token_usage_record\",\"payload\":{\"session_id\":\"thread-main\",\"response_id\":\"resp-1\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":40,\"output_tokens\":30,\"reasoning_output_tokens\":10}}}\n",
            "壊れた行\n"
        )
    ).unwrap();
    (home, vault)
}

#[test]
fn codex_run_writes_monthly_json_and_state() {
    let (home, vault) = codex_fixture();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["codex", "--month", "2026-09"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote"))
        .stderr(predicate::str::contains("1 行"));

    let output = vault
        .path()
        .join("_logs/_ai-subscription-usage/codex/2026-09.json");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(json["provider"], "codex");
    assert_eq!(json["days"][0]["entries"][0]["requests"], 1);
    assert_eq!(json["days"][0]["entries"][0]["input"], 100);
    assert_eq!(json["days"][0]["entries"][0]["cached_input"], 40);
    assert_eq!(json["days"][0]["entries"][0]["reasoning_output"], 10);
    assert!(home
        .path()
        .join(".local/share/ai-subscription-usage/codex/state.json")
        .is_file());
}

#[test]
fn codex_dry_run_does_not_write_state_or_monthly_json() {
    let (home, vault) = codex_fixture();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["codex", "--month", "2026-09", "--dry-run"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "--dry-run のため何も書き出していません",
        ));

    assert!(!vault
        .path()
        .join("_logs/_ai-subscription-usage/codex/2026-09.json")
        .exists());
    assert!(!home
        .path()
        .join(".local/share/ai-subscription-usage/codex/state.json")
        .exists());
}

#[test]
fn codex_second_run_reuses_state_without_double_counting() {
    let (home, vault) = codex_fixture();
    for _ in 0..2 {
        Command::cargo_bin("ai-subscription-usage")
            .unwrap()
            .args(["codex", "--month", "2026-09"])
            .env("HOME", home.path())
            .env("OBSIDIAN_VAULT", vault.path())
            .assert()
            .success();
    }
    let output = vault
        .path()
        .join("_logs/_ai-subscription-usage/codex/2026-09.json");
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(json["days"][0]["entries"][0]["requests"], 1);
    assert_eq!(json["days"][0]["entries"][0]["output"], 30);
}

/// 利用制限の観測値を 1 件も持たないセッションログ．
fn codex_fixture_without_rate_limits() -> (TempDir, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/10");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-test.jsonl"),
        "{\"timestamp\":\"2026-09-10T01:00:02Z\",\"type\":\"token_usage_record\",\"payload\":{\"session_id\":\"s\",\"response_id\":\"resp-1\",\"usage\":{\"input_tokens\":100,\"output_tokens\":30}}}\n",
    )
    .unwrap();
    (home, vault)
}

fn limits_json(vault: &TempDir) -> std::path::PathBuf {
    vault
        .path()
        .join("_logs/_ai-subscription-usage/codex/_limits.json")
}

fn limits_jsonl(home: &TempDir) -> std::path::PathBuf {
    home.path()
        .join(".local/share/ai-subscription-usage/codex/limits.jsonl")
}

#[test]
fn codex_limits_updates_only_the_limits_files() {
    let (home, vault) = codex_fixture();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["codex", "limits"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("_limits.json"));

    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(limits_json(&vault)).unwrap()).unwrap();
    assert_eq!(json["provider"], "codex");
    // 観測値をそのまま記録する — 300 分を «5 時間枠» と読み替えて別の鍵に置いたりしない．
    assert_eq!(json["rate_limits"]["primary"]["window_minutes"], 300);
    assert_eq!(json["rate_limits"]["primary"]["used_percent"], 12.5);
    assert_eq!(json["rate_limits"]["primary"]["resets_at"], 1789021600i64);
    assert_eq!(json["history"].as_array().unwrap().len(), 1);
    assert!(limits_jsonl(&home).is_file());
    // 月次 JSON には触れない．
    assert!(!vault
        .path()
        .join("_logs/_ai-subscription-usage/codex/2026-09.json")
        .exists());
}

#[test]
fn codex_run_updates_limits_alongside_the_monthly_json() {
    let (home, vault) = codex_fixture();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["codex", "--month", "2026-09"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success();
    assert!(vault
        .path()
        .join("_logs/_ai-subscription-usage/codex/2026-09.json")
        .is_file());
    assert!(limits_json(&vault).is_file());
}

#[test]
fn codex_limits_is_idempotent_across_full_rescans() {
    let (home, vault) = codex_fixture();
    for _ in 0..3 {
        Command::cargo_bin("ai-subscription-usage")
            .unwrap()
            .args(["codex", "--all", "limits"])
            .env("HOME", home.path())
            .env("OBSIDIAN_VAULT", vault.path())
            .assert()
            .success();
    }
    let rows = fs::read_to_string(limits_jsonl(&home)).unwrap();
    assert_eq!(rows.lines().filter(|l| !l.trim().is_empty()).count(), 1);
}

#[test]
fn codex_limits_dry_run_writes_nothing() {
    let (home, vault) = codex_fixture();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["codex", "--dry-run", "limits"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("書く予定でした"));

    assert!(!limits_json(&vault).exists());
    assert!(!limits_jsonl(&home).exists());
}

#[test]
fn codex_writes_no_limits_file_when_nothing_was_observed() {
    let (home, vault) = codex_fixture_without_rate_limits();
    Command::cargo_bin("ai-subscription-usage")
        .unwrap()
        .args(["codex", "limits"])
        .env("HOME", home.path())
        .env("OBSIDIAN_VAULT", vault.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("観測値が 1 件も無いため"));
    // «0%» のファイルを置かない．画面は «無い» を正常な状態として扱う．
    assert!(!limits_json(&vault).exists());
}

/// `--month` 無しの実行が **月ごとに 1 ファイル**を書くこと．
///
/// 以前は «いちばん古い月» の名前で 1 ファイルだけを書き，その中へ全期間の日を
/// 入れていた (`2026-08.json` の中に 2026-09 の日が入り，`2026-09.json` は
/// 作られない)．日次の launchd ジョブは `--month` 無しで回るので，画面が読む
/// ファイルが毎朝この形になっていた．
#[test]
fn a_run_without_month_writes_one_file_per_month() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();

    let write_day = |ymd: &str, id: &str| {
        let day = home.join(format!(".codex/sessions/{}/{}/{}", &ymd[..4], &ymd[5..7], &ymd[8..10]));
        std::fs::create_dir_all(&day).unwrap();
        let ts = format!("{ymd}T01:00:00Z");
        std::fs::write(
            day.join(format!("rollout-{ymd}T01-00-00-{id}.jsonl")),
            format!(
                "{{\"timestamp\":\"{ts}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t\",\"model\":\"gpt-5.6-sol\"}}}}\n\
                 {{\"timestamp\":\"{ts}\",\"type\":\"token_usage_record\",\"payload\":{{\"session_id\":\"s{id}\",\"response_id\":\"r{id}\",\"usage\":{{\"input_tokens\":100,\"cached_input_tokens\":40,\"output_tokens\":30,\"reasoning_output_tokens\":10}}}}}}\n"
            ),
        )
        .unwrap();
    };
    write_day("2026-08-19", "a");
    write_day("2026-09-05", "b");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ai-subscription-usage"))
        .env("HOME", &home)
        .env("OBSIDIAN_VAULT", &vault)
        .arg("codex")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let codex_dir = vault.join("_logs/_ai-subscription-usage/codex");
    for (file, day) in [("2026-08.json", "2026-08-19"), ("2026-09.json", "2026-09-05")] {
        let path = codex_dir.join(file);
        assert!(path.exists(), "{file} が書かれていない");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["month"].as_str().unwrap(), &file[..7]);
        let days: Vec<&str> = v["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["date"].as_str().unwrap())
            .collect();
        assert_eq!(days, vec![day], "{file} に他の月の日が混ざっている");
    }
}

/// `--month` は «どの月を書くか» の絞り込みである．
#[test]
fn month_filters_which_files_are_written() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    for (ymd, id) in [("2026-08-19", "a"), ("2026-09-05", "b")] {
        let day = home.join(format!(".codex/sessions/{}/{}/{}", &ymd[..4], &ymd[5..7], &ymd[8..10]));
        std::fs::create_dir_all(&day).unwrap();
        let ts = format!("{ymd}T01:00:00Z");
        std::fs::write(
            day.join(format!("rollout-{ymd}T01-00-00-{id}.jsonl")),
            format!(
                "{{\"timestamp\":\"{ts}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t\",\"model\":\"gpt-5.6-sol\"}}}}\n\
                 {{\"timestamp\":\"{ts}\",\"type\":\"token_usage_record\",\"payload\":{{\"session_id\":\"s{id}\",\"response_id\":\"r{id}\",\"usage\":{{\"input_tokens\":100,\"cached_input_tokens\":40,\"output_tokens\":30,\"reasoning_output_tokens\":10}}}}}}\n"
            ),
        )
        .unwrap();
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ai-subscription-usage"))
        .env("HOME", &home)
        .env("OBSIDIAN_VAULT", &vault)
        .args(["codex", "--month", "2026-09"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let codex_dir = vault.join("_logs/_ai-subscription-usage/codex");
    assert!(codex_dir.join("2026-09.json").exists());
    assert!(
        !codex_dir.join("2026-08.json").exists(),
        "--month で絞ったのに他の月まで書いている"
    );
}
