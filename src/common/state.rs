//! 増分更新のための状態ファイル．
//!
//! # これは «キャッシュ» ではなく唯一の長期記録である
//!
//! Claude Code は `~/.claude/projects/**/*.jsonl` を **30 日で自動削除**する．
//! したがって transcript から再計算できるのは直近 30 日分だけで，
//! それより古い日次の集計は **この state にしか残っていない**．
//!
//! そのため:
//!
//! - 置き場所は `~/.local/share/claude-usage/state.json`
//!   (`~/.cache` は OS やツールに消されうるのでデータの置き場所として不適切)
//! - 実在しなくなった transcript の集計結果は **捨てずに `missing: true` を立てて保持**し，
//!   月次 JSON の合算に含め続ける．パスを鍵にしているので，消えた transcript が
//!   復活することはなく，二重計上は起きない
//! - 書き込みは一時ファイル + rename の atomic write．書き込み中に落ちて
//!   state が飛ぶと過去の集計が永久に失われる
//!
//! **state を失うと，生き残っている transcript の分 (直近 30 日) しか復元できない．**

use crate::claude::aggregate::{AggEntry, FileAggregation, PromptEntry};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// 版 1 = 日単位の集計．版 2 で (日付, **時**, モデル, scope) に細かくした．
/// 版 3 で **人が打った発話の件数** を足した (週の制限の単位の候補の 1 つ)．
pub const STATE_VERSION: u32 = 3;

/// 1 ファイル分の状態．
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileState {
    pub mtime_ns: u64,
    pub size: u64,
    /// transcript が実在しなくなったか (30 日での自動削除など)．
    /// `true` でも `agg` は保持し，合算に含め続ける．
    #[serde(default)]
    pub missing: bool,
    /// 実在しないと判定した日 (`YYYY-MM-DD`)．
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_since: Option<String>,
    pub agg: Vec<AggEntry>,
    /// 人が打った発話 (時単位)．**版 3 で追加**．
    ///
    /// 版 2 の state には無い．消えた transcript の分は読み直せないので空のままになる —
    /// つまり **発話数は直近 30 日ぶんしか無い**．窓の推定に使うのは現在の窓なので実害は無いが，
    /// 古い窓について発話数を語ってはいけない．
    #[serde(default)]
    pub prompts: Vec<PromptEntry>,
}

/// state.json 全体．キーは transcript の絶対パス．
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub version: u32,
    pub files: BTreeMap<String, FileState>,
}

impl Default for State {
    fn default() -> Self {
        State {
            version: STATE_VERSION,
            files: BTreeMap::new(),
        }
    }
}

/// [`State::load`] の戻り値．
pub struct Loaded {
    pub state: State,
    /// 実在する transcript をすべて読み直す必要があるか (版の昇格時)．
    pub needs_full_rescan: bool,
}

impl State {
    /// 読み込む．
    ///
    /// 戻り値の [`Loaded::needs_full_rescan`] が真なら，実在する transcript を
    /// すべて読み直す必要がある (版 1 → 2 の昇格など)．
    ///
    /// 無い / 壊れている場合は空の状態を返す．そのとき
    /// **消えた transcript の分は二度と復元できない**ことに注意．
    pub fn load(path: &Path) -> Loaded {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => {
                return Loaded {
                    state: State::default(),
                    needs_full_rescan: false,
                }
            }
        };
        let mut s: State = match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[WARN] state を読めません ({e}) — 空から始めます");
                eprintln!("       消えた transcript の分の集計は復元できません．");
                return Loaded {
                    state: State::default(),
                    needs_full_rescan: false,
                };
            }
        };

        if s.version == STATE_VERSION {
            return Loaded {
                state: s,
                needs_full_rescan: false,
            };
        }
        if s.version == 2 {
            // 版 2 には発話数が無い．実在する transcript を読み直して埋める．
            // 消えている分は読み直せないので空のまま (古い窓の発話数は出せない)．
            eprintln!("[INFO] state を版 2 → {STATE_VERSION} に上げます (人の発話を数えるため)．");
            eprintln!("       実在する transcript をすべて読み直します．");
            eprintln!("       既に消えた transcript の分の発話数は復元できません．");
            s.version = STATE_VERSION;
            return Loaded {
                state: s,
                needs_full_rescan: true,
            };
        }
        if s.version == 1 {
            // 版 1 は日単位なので `hour` が無い (= None のまま読める)．
            // 実在する transcript は読み直して時を埋める．消えている分は
            // 読み直しようがないので hour: null のまま残し，窓の集計から外す．
            let unresolved = s.files.values().filter(|f| f.missing).count();
            eprintln!("[INFO] state を版 1 → {STATE_VERSION} に上げます (日単位 → 時間単位)．");
            eprintln!("       実在する transcript をすべて読み直します．");
            if unresolved > 0 {
                eprintln!("       transcript が既に消えている {unresolved} ファイルの分は時が分からないため，");
                eprintln!("       日次の集計には残しますが窓の集計からは外します．");
            }
            s.version = STATE_VERSION;
            return Loaded {
                state: s,
                needs_full_rescan: true,
            };
        }

        eprintln!(
            "[WARN] state の版が違います (期待 {STATE_VERSION}, 実際 {}) — 空から始めます",
            s.version
        );
        eprintln!("       消えた transcript の分の集計は復元できません．");
        Loaded {
            state: State::default(),
            needs_full_rescan: false,
        }
    }

    /// 一時ファイル + rename で書き出す．
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::common::atomic_write::write_atomic(path, &serde_json::to_vec(self)?)
    }

    /// mtime と size がどちらも一致すれば，そのファイルは読み飛ばせる．
    pub fn is_fresh(&self, key: &str, mtime_ns: u64, size: u64) -> bool {
        self.files
            .get(key)
            .map(|f| !f.missing && f.mtime_ns == mtime_ns && f.size == size)
            .unwrap_or(false)
    }

    /// ファイル単位の集計結果を入れる (同じパスなら置き換え，加算ではない)．
    pub fn put(&mut self, key: String, mtime_ns: u64, size: u64, agg: &FileAggregation) {
        self.files.insert(
            key,
            FileState {
                mtime_ns,
                size,
                // 実在するものを読んだので missing は解除する．
                missing: false,
                missing_since: None,
                agg: agg.agg.clone(),
                prompts: agg.prompts.clone(),
            },
        );
    }

    /// 実在しなくなったファイルに `missing` を立てる．**agg は捨てない．**
    ///
    /// 戻り値は，今回新たに missing になった件数．
    pub fn mark_missing(&mut self, alive: &HashSet<String>, today: &str) -> usize {
        let mut newly = 0usize;
        for (key, f) in self.files.iter_mut() {
            if alive.contains(key) || f.missing {
                continue;
            }
            f.missing = true;
            f.missing_since = Some(today.to_string());
            newly += 1;
        }
        newly
    }

    /// missing なエントリを本当に捨てる (`--forget-missing`)．
    ///
    /// 捨てた分の集計は二度と戻らない．
    pub fn forget_missing(&mut self) -> usize {
        let before = self.files.len();
        self.files.retain(|_, f| !f.missing);
        before - self.files.len()
    }

    /// missing なエントリの件数．
    pub fn missing_count(&self) -> usize {
        self.files.values().filter(|f| f.missing).count()
    }

    /// すべてのファイルの集計行 (missing なものも含む)．
    pub fn all_entries(&self) -> impl Iterator<Item = &AggEntry> {
        self.files.values().flat_map(|f| f.agg.iter())
    }

    /// すべてのファイルの発話行 (missing なものも含む)．
    pub fn all_prompts(&self) -> impl Iterator<Item = &PromptEntry> {
        self.files.values().flat_map(|f| f.prompts.iter())
    }
}

/// ファイルの mtime をナノ秒で取る．取れなければ 0 (= 常に読み直す)．
pub fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// 旧パス (`~/.cache/claude-usage/state.json`) から新パスへ 1 回だけ移す．
///
/// - 新パスが既にあれば旧は**無視**する (黙って両方読むと二重計上の温床になる)
/// - 移行したら `true` を返す
pub fn migrate_state(old: &Path, new: &Path) -> Result<bool, String> {
    if new.exists() {
        if old.exists() {
            eprintln!("[WARN] 旧 state が残っています: {}", old.display());
            eprintln!(
                "       新 state ({}) を使うので旧は無視します．不要なら手で消してください．",
                new.display()
            );
        }
        return Ok(false);
    }
    if !old.exists() {
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("state の移行先を作れません: {} ({e})", parent.display()))?;
    }
    // 別ボリューム間だと rename が失敗しうるので，そのときはコピー + 削除に落とす．
    if std::fs::rename(old, new).is_err() {
        std::fs::copy(old, new).map_err(|e| {
            format!(
                "state をコピーできません: {} → {} ({e})",
                old.display(),
                new.display()
            )
        })?;
        std::fs::remove_file(old).ok();
    }
    println!(
        "state を移行しました: {} → {}",
        old.display(),
        new.display()
    );
    Ok(true)
}

/// state の既定パス (`~/.local/share/claude-usage/state.json`)．
pub fn default_state_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("claude-usage")
        .join("state.json")
}

/// 旧 state のパス (`~/.cache/claude-usage/state.json`)．
pub fn legacy_state_path(home: &Path) -> PathBuf {
    home.join(".cache").join("claude-usage").join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::transcript::Scope;

    fn entry(date: &str, out: u64) -> AggEntry {
        AggEntry {
            date: date.to_string(),
            hour: Some(12),
            model: "claude-opus-5".to_string(),
            scope: Scope::Main,
            requests: 1,
            input: 1,
            output: out,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: 0,
        }
    }

    fn fa(entries: Vec<AggEntry>) -> FileAggregation {
        FileAggregation {
            agg: entries,
            ..Default::default()
        }
    }

    fn alive(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 往復して同じ内容になる() {
        let dir = crate::claude::aggregate::tests::tmpdir("state-roundtrip");
        let path = dir.join("state.json");

        let mut s = State::default();
        s.put(
            "/a/b.jsonl".to_string(),
            12_345,
            678,
            &fa(vec![entry("2026-08-22", 10)]),
        );
        s.save(&path).unwrap();

        let loaded = State::load(&path).state;
        assert_eq!(loaded, s);
        assert!(loaded.is_fresh("/a/b.jsonl", 12_345, 678));
        assert!(!loaded.is_fresh("/a/b.jsonl", 12_346, 678));
        assert!(!loaded.is_fresh("/a/b.jsonl", 12_345, 679));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 増分更新で二重計上しない() {
        let mut s = State::default();
        s.put(
            "/a/b.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-08-22", 10)]),
        );
        s.put(
            "/a/b.jsonl".to_string(),
            2,
            2,
            &fa(vec![entry("2026-08-22", 10)]),
        );
        assert_eq!(s.files.len(), 1);
        assert_eq!(s.all_entries().map(|e| e.output).sum::<u64>(), 10);

        // 追記されて集計が増えた場合は，新しい agg で丸ごと置き換わる．
        s.put(
            "/a/b.jsonl".to_string(),
            3,
            3,
            &fa(vec![entry("2026-08-22", 10), entry("2026-08-23", 20)]),
        );
        assert_eq!(s.all_entries().map(|e| e.output).sum::<u64>(), 30);
    }

    #[test]
    fn 消えたファイルの_agg_は合算に残り続ける() {
        let mut s = State::default();
        s.put(
            "/live.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-08-22", 10)]),
        );
        s.put(
            "/gone.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-07-30", 99)]),
        );

        // transcript が 30 日で消えた．
        assert_eq!(s.mark_missing(&alive(&["/live.jsonl"]), "2026-08-29"), 1);

        // エントリは残っている．
        assert_eq!(s.files.len(), 2);
        assert!(s.files["/gone.jsonl"].missing);
        assert_eq!(
            s.files["/gone.jsonl"].missing_since.as_deref(),
            Some("2026-08-29")
        );
        assert!(!s.files["/live.jsonl"].missing);
        assert_eq!(s.missing_count(), 1);

        // 合算にも入り続ける．
        let total: u64 = s.all_entries().map(|e| e.output).sum();
        assert_eq!(total, 109);
        assert!(s.all_entries().any(|e| e.date == "2026-07-30"));

        // 2 回目以降の実行で missing_since が上書きされないこと．
        assert_eq!(s.mark_missing(&alive(&["/live.jsonl"]), "2026-09-05"), 0);
        assert_eq!(
            s.files["/gone.jsonl"].missing_since.as_deref(),
            Some("2026-08-29")
        );

        // 保存して読み直しても残る．
        let dir = crate::claude::aggregate::tests::tmpdir("state-missing");
        let path = dir.join("state.json");
        s.save(&path).unwrap();
        let loaded = State::load(&path).state;
        assert_eq!(loaded, s);
        assert_eq!(loaded.all_entries().map(|e| e.output).sum::<u64>(), 109);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_なファイルは_fresh_扱いにしない() {
        // 同じパスに新しい transcript が現れた場合は読み直す．
        let mut s = State::default();
        s.put(
            "/x.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-08-22", 10)]),
        );
        s.mark_missing(&alive(&[]), "2026-08-29");
        assert!(!s.is_fresh("/x.jsonl", 1, 1));

        // 読み直すと missing は解除される．
        s.put(
            "/x.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-08-22", 10)]),
        );
        assert!(!s.files["/x.jsonl"].missing);
        assert_eq!(s.files["/x.jsonl"].missing_since, None);
        assert!(s.is_fresh("/x.jsonl", 1, 1));
    }

    #[test]
    fn forget_missing_でだけ捨てられる() {
        let mut s = State::default();
        s.put(
            "/live.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-08-22", 10)]),
        );
        s.put(
            "/gone.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-07-30", 99)]),
        );
        s.mark_missing(&alive(&["/live.jsonl"]), "2026-08-29");

        assert_eq!(s.forget_missing(), 1);
        assert_eq!(s.files.len(), 1);
        assert!(s.files.contains_key("/live.jsonl"));
        assert_eq!(s.all_entries().map(|e| e.output).sum::<u64>(), 10);
    }

    #[test]
    fn 壊れた_state_は空扱いにする() {
        let dir = crate::claude::aggregate::tests::tmpdir("state-broken");
        let path = dir.join("state.json");
        std::fs::write(&path, "{ 壊れている").unwrap();
        assert!(State::load(&path).state.files.is_empty());
        assert!(State::load(&dir.join("no-such.json"))
            .state
            .files
            .is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_を持たない旧形式の_state_も読める() {
        let dir = crate::claude::aggregate::tests::tmpdir("state-legacy-format");
        let path = dir.join("state.json");
        std::fs::write(
            &path,
            r#"{"version":3,"files":{"/a.jsonl":{"mtime_ns":1,"size":2,"agg":[
              {"date":"2026-07-30","hour":3,"model":"claude-opus-5","scope":"main","requests":1,
               "input":1,"output":2,"cache_write_5m":0,"cache_write_1h":0,"cache_read":0}]}}}"#,
        )
        .unwrap();
        let loaded = State::load(&path);
        assert!(!loaded.needs_full_rescan);
        assert_eq!(loaded.state.files.len(), 1);
        assert!(!loaded.state.files["/a.jsonl"].missing);
        assert_eq!(loaded.state.all_entries().count(), 1);
        // prompts が無い形でも読める (版 3 で足したフィールド)．
        assert_eq!(loaded.state.all_prompts().count(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 版2の_state_は読めるが全再走査を要求する() {
        // 版 2 には人の発話が無い．実在する transcript を読み直して埋める．
        let dir = crate::claude::aggregate::tests::tmpdir("state-v2-upgrade");
        let path = dir.join("state.json");
        std::fs::write(
            &path,
            r#"{"version":2,"files":{"/a.jsonl":{"mtime_ns":1,"size":2,"missing":true,"agg":[
              {"date":"2026-07-30","hour":3,"model":"claude-opus-5","scope":"main","requests":1,
               "input":1,"output":2,"cache_write_5m":0,"cache_write_1h":0,"cache_read":0}]}}}"#,
        )
        .unwrap();
        let loaded = State::load(&path);
        assert!(loaded.needs_full_rescan, "版 2 なら読み直しを要求する");
        assert_eq!(loaded.state.version, STATE_VERSION);
        // **消えたファイルの集計は捨てない．**
        assert_eq!(loaded.state.all_entries().count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 版1の_state_は読めるが全再走査を要求する() {
        let dir = crate::claude::aggregate::tests::tmpdir("state-v1-upgrade");
        let path = dir.join("state.json");
        // 版 1 は hour を持たない．missing なファイルは読み直せないので hour: null のまま残る．
        std::fs::write(
            &path,
            r#"{"version":1,"files":{
              "/alive.jsonl":{"mtime_ns":1,"size":2,"agg":[
                {"date":"2026-07-30","model":"claude-opus-5","scope":"main","requests":1,
                 "input":0,"output":10,"cache_write_5m":0,"cache_write_1h":0,"cache_read":0}]},
              "/gone.jsonl":{"mtime_ns":1,"size":2,"missing":true,"agg":[
                {"date":"2026-06-15","model":"claude-opus-5","scope":"main","requests":1,
                 "input":0,"output":99,"cache_write_5m":0,"cache_write_1h":0,"cache_read":0}]}}}"#,
        )
        .unwrap();

        let loaded = State::load(&path);
        // 実在する transcript を読み直させる．
        assert!(loaded.needs_full_rescan);
        let st = loaded.state;
        assert_eq!(st.version, STATE_VERSION);
        // 集計は失われない．
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 109);
        // 時は分からないので None のまま．
        assert!(st.all_entries().all(|e| e.hour.is_none()));
        // 消えていた分はそのまま missing．
        assert!(st.files["/gone.jsonl"].missing);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 知らない版の_state_は空扱いにする() {
        let dir = crate::claude::aggregate::tests::tmpdir("state-v99");
        let path = dir.join("state.json");
        std::fs::write(&path, r#"{"version":99,"files":{}}"#).unwrap();
        let loaded = State::load(&path);
        assert!(!loaded.needs_full_rescan);
        assert!(loaded.state.files.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 旧パスから新パスへ一度だけ移行する() {
        let dir = crate::claude::aggregate::tests::tmpdir("state-migrate");
        let old = dir.join("cache/claude-usage/state.json");
        let new = dir.join("share/claude-usage/state.json");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();

        let mut s = State::default();
        s.put(
            "/a.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-07-30", 42)]),
        );
        s.save(&old).unwrap();

        assert!(migrate_state(&old, &new).unwrap());
        assert!(new.exists());
        assert!(!old.exists());
        assert_eq!(
            State::load(&new)
                .state
                .all_entries()
                .map(|e| e.output)
                .sum::<u64>(),
            42
        );

        // 2 回目は何もしない．
        assert!(!migrate_state(&old, &new).unwrap());

        // 新パスが既にあれば旧は無視する (両方読まない)．
        let mut old2 = State::default();
        old2.put(
            "/b.jsonl".to_string(),
            1,
            1,
            &fa(vec![entry("2026-07-30", 7)]),
        );
        old2.save(&old).unwrap();
        assert!(!migrate_state(&old, &new).unwrap());
        assert_eq!(
            State::load(&new)
                .state
                .all_entries()
                .map(|e| e.output)
                .sum::<u64>(),
            42
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 旧パスも新パスも無ければ何もしない() {
        let dir = crate::claude::aggregate::tests::tmpdir("state-migrate-none");
        assert!(!migrate_state(&dir.join("old.json"), &dir.join("new.json")).unwrap());
        assert!(!dir.join("new.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 既定のパスは_local_share_の下() {
        let home = Path::new("/Users/x");
        assert_eq!(
            default_state_path(home),
            PathBuf::from("/Users/x/.local/share/claude-usage/state.json")
        );
        assert_eq!(
            legacy_state_path(home),
            PathBuf::from("/Users/x/.cache/claude-usage/state.json")
        );
    }
}
