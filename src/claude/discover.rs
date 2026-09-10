//! transcript の走査と state の更新．
//!
//! `main.rs` から切り出してあるのは，「`--all` でも missing のエントリが消えない」
//! といった振る舞いをテストできるようにするため．

use crate::claude::aggregate::aggregate_file;
use crate::common::state::{mtime_ns, State};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct ScanStats {
    /// 実在した transcript の数．
    pub found: usize,
    /// 実際に読み込んだファイル数．
    pub read: usize,
    /// state から再利用したファイル数．
    pub reused: usize,
    /// 読んだ行数．
    pub lines: usize,
    /// JSON として読めなかった行の総数．
    pub broken_lines: usize,
    /// 破損行を含んでいたファイルの一覧 (`パス (N 行)`)．
    pub broken_files: Vec<String>,
    /// 今回新たに実在しなくなったファイル数．
    pub newly_missing: usize,
    /// missing なエントリの総数．
    pub missing_total: usize,
}

/// transcript を走査して state を更新する．
///
/// `force_reread` が真なら，実在するファイルはすべて読み直す (`--all`)．
/// **このとき実在しないファイルの agg は触らない．** transcript は 30 日で消えるので，
/// `--all` で state を作り直してしまうと過去の集計が消える．
pub fn scan_into(paths: &[PathBuf], st: &mut State, force_reread: bool, today: &str) -> ScanStats {
    let mut s = ScanStats {
        found: paths.len(),
        ..Default::default()
    };
    let mut alive: HashSet<String> = HashSet::with_capacity(paths.len());

    for path in paths {
        let key = path.to_string_lossy().to_string();
        alive.insert(key.clone());

        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[WARN] メタデータを読めません: {} ({e})", path.display());
                continue;
            }
        };
        let (mtime, size) = (mtime_ns(&meta), meta.len());

        if !force_reread && st.is_fresh(&key, mtime, size) {
            s.reused += 1;
            continue;
        }

        match aggregate_file(path) {
            Ok(fa) => {
                s.read += 1;
                s.lines += fa.total_lines;
                if fa.broken_lines > 0 {
                    s.broken_lines += fa.broken_lines;
                    s.broken_files
                        .push(format!("{} ({} 行)", path.display(), fa.broken_lines));
                }
                st.put(key, mtime, size, &fa);
            }
            Err(e) => eprintln!("[WARN] 読み込みに失敗しました: {} ({e})", path.display()),
        }
    }

    // 消えた transcript は捨てず，印だけ付けて agg は残す．
    s.newly_missing = st.mark_missing(&alive, today);
    s.missing_total = st.missing_count();
    s
}

/// 走査対象を列挙する (`aggregate::find_transcripts` の薄い別名)．
pub fn find(root: &Path) -> Vec<PathBuf> {
    crate::claude::aggregate::find_transcripts(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::aggregate::{tests::tmpdir, FileAggregation};

    fn line(id: &str, ts: &str, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"{ts}","message":{{"id":"{id}","model":"claude-opus-5","usage":{{"input_tokens":0,"output_tokens":{out}}}}}}}"#
        )
    }

    #[test]
    fn 実在するファイルを読み消えたファイルに印を付ける() {
        let dir = tmpdir("scan-basic");
        let live = dir.join("live.jsonl");
        std::fs::write(&live, line("m1", "2026-08-22T01:00:00Z", 10)).unwrap();

        let mut st = State::default();
        // 30 日前に消えた transcript の集計が state に残っている状況を作る．
        st.put(
            "/gone.jsonl".to_string(),
            1,
            1,
            &FileAggregation {
                agg: vec![crate::claude::aggregate::AggEntry {
                    date: "2026-07-30".to_string(),
                    hour: None,
                    model: "claude-opus-5".to_string(),
                    scope: crate::claude::transcript::Scope::Main,
                    requests: 1,
                    input: 0,
                    output: 99,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    cache_read: 0,
                }],
                ..Default::default()
            },
        );

        let paths = find(&dir);
        let s = scan_into(&paths, &mut st, false, "2026-08-29");
        assert_eq!(s.found, 1);
        assert_eq!(s.read, 1);
        assert_eq!(s.newly_missing, 1);
        assert_eq!(s.missing_total, 1);

        // 消えたぶんも合算に残っている．
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 109);

        // 2 回目は state から再利用され，合計は変わらない (二重計上しない)．
        let s2 = scan_into(&paths, &mut st, false, "2026-08-30");
        assert_eq!(s2.read, 0);
        assert_eq!(s2.reused, 1);
        assert_eq!(s2.newly_missing, 0);
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 109);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn all_で全再読しても_missing_のエントリは消えない() {
        let dir = tmpdir("scan-all-keeps-missing");
        let live = dir.join("live.jsonl");
        std::fs::write(&live, line("m1", "2026-08-22T01:00:00Z", 10)).unwrap();

        let mut st = State::default();
        st.put(
            "/gone.jsonl".to_string(),
            1,
            1,
            &FileAggregation {
                agg: vec![crate::claude::aggregate::AggEntry {
                    date: "2026-07-30".to_string(),
                    hour: None,
                    model: "claude-opus-5".to_string(),
                    scope: crate::claude::transcript::Scope::Main,
                    requests: 1,
                    input: 0,
                    output: 99,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    cache_read: 0,
                }],
                ..Default::default()
            },
        );

        let paths = find(&dir);
        // force_reread = true (--all)．
        let s = scan_into(&paths, &mut st, true, "2026-08-29");
        assert_eq!(s.read, 1);
        assert_eq!(s.reused, 0);

        // 消えた transcript のぶんは残ったまま．
        assert!(st.files.contains_key("/gone.jsonl"));
        assert!(st.files["/gone.jsonl"].missing);
        assert!(st.all_entries().any(|e| e.date == "2026-07-30"));
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 109);

        // --all を何度走らせても増えない．
        scan_into(&paths, &mut st, true, "2026-08-30");
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 109);

        // --forget-missing を明示したときだけ消える．
        assert_eq!(st.forget_missing(), 1);
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 10);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn forget_missing_は走査の後に効かせる() {
        // 走査の前に捨てると，「今回初めて消えたと分かったファイル」が対象から漏れる．
        let dir = tmpdir("scan-forget-order");
        std::fs::write(
            dir.join("live.jsonl"),
            line("m1", "2026-08-22T01:00:00Z", 10),
        )
        .unwrap();

        let mut st = State::default();
        st.put(
            "/gone.jsonl".to_string(),
            1,
            1,
            &FileAggregation {
                agg: vec![crate::claude::aggregate::AggEntry {
                    date: "2026-06-15".to_string(),
                    hour: None,
                    model: "claude-opus-5".to_string(),
                    scope: crate::claude::transcript::Scope::Main,
                    requests: 1,
                    input: 0,
                    output: 99,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                    cache_read: 0,
                }],
                ..Default::default()
            },
        );

        // この時点ではまだ missing が立っていないので，先に捨てても何も落ちない．
        assert_eq!(st.forget_missing(), 0);

        let paths = find(&dir);
        scan_into(&paths, &mut st, true, "2026-08-29");
        // 走査の後なら落ちる．
        assert_eq!(st.forget_missing(), 1);
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 10);
        assert!(!st.all_entries().any(|e| e.date == "2026-06-15"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ファイルが追記されたら読み直す() {
        let dir = tmpdir("scan-append");
        let live = dir.join("live.jsonl");
        std::fs::write(&live, line("m1", "2026-08-22T01:00:00Z", 10)).unwrap();

        let mut st = State::default();
        let paths = find(&dir);
        scan_into(&paths, &mut st, false, "2026-08-29");
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 10);

        std::fs::write(
            &live,
            format!(
                "{}\n{}",
                line("m1", "2026-08-22T01:00:00Z", 10),
                line("m2", "2026-08-22T02:00:00Z", 20)
            ),
        )
        .unwrap();

        let s = scan_into(&paths, &mut st, false, "2026-08-29");
        assert_eq!(s.read, 1);
        // 30 (置き換え)．40 になったら二重計上している．
        assert_eq!(st.all_entries().map(|e| e.output).sum::<u64>(), 30);

        std::fs::remove_dir_all(&dir).ok();
    }
}
