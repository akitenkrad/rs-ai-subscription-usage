//! transcript ファイルの走査と集計．
//!
//! 「ファイル単位の集計結果」を作るところまでがこのモジュールの責務．
//! 月ごとの出力を組み立てるのは [`crate::output`]．

use crate::claude::transcript::{parse_line, ParseOutcome, Scope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// トークン数の合計．
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

impl Totals {
    pub fn add(&mut self, other: &Totals) {
        self.requests += other.requests;
        self.input += other.input;
        self.output += other.output;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_read += other.cache_read;
    }
}

/// 日付 x モデル x scope で束ねた集計の 1 行．
///
/// state.json にそのまま入る形なので，フィールド名を変えると増分更新の互換が切れる．
/// `model` は **生のモデル名**を保持する (エイリアス解決は出力時)．
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggEntry {
    pub date: String,
    /// Asia/Tokyo のローカル時 (0〜23)．
    ///
    /// `None` は «時が分からない» の意味で，state 版 1 から引き継いだ行にだけ現れる．
    /// transcript が既に消えていて読み直せない行なので，日次の集計には入れるが
    /// 窓 (週の使用制限) の集計からは外し，`_windows.json` の `unresolved_days` に数える．
    #[serde(default)]
    pub hour: Option<u8>,
    pub model: String,
    pub scope: Scope,
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

/// 人が打った発話の，時単位の件数．
///
/// 週の制限の単位の候補の 1 つ．`AggEntry` とは鍵が違う (モデルも scope も無い) ので
/// 別の列として持つ．**state 版 3 で追加**．版 2 の state には無いので `#[serde(default)]`．
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptEntry {
    pub date: String,
    pub hour: u8,
    pub count: u64,
}

/// 1 ファイルを集計した結果．
#[derive(Debug, Clone, Default)]
pub struct FileAggregation {
    pub agg: Vec<AggEntry>,
    /// 人が打った発話 (時単位)．
    pub prompts: Vec<PromptEntry>,
    /// JSON として読めなかった行数．
    pub broken_lines: usize,
    /// 読んだ行数 (空行含む)．
    pub total_lines: usize,
}

/// パスが `/subagents/` を含むか．
pub fn path_is_subagent(path: &Path) -> bool {
    path.to_string_lossy().contains("/subagents/")
}

/// transcript ファイル 1 本を集計する．
///
/// 同一ファイル内で同じ `message.id` が複数回現れたら 1 回だけ数える
/// (ストリーミング中の途中経過が同じ id で複数行書かれるため)．
pub fn aggregate_file(path: &Path) -> std::io::Result<FileAggregation> {
    let is_sub = path_is_subagent(path);
    let reader = BufReader::new(File::open(path)?);

    let mut seen: HashSet<String> = HashSet::new();
    let mut map: BTreeMap<(String, Option<u8>, String, Scope), Totals> = BTreeMap::new();
    let mut prompt_map: BTreeMap<(String, u8), u64> = BTreeMap::new();
    let mut broken_lines = 0usize;
    let mut total_lines = 0usize;

    for line in reader.lines() {
        total_lines += 1;
        // 不正な UTF-8 の行は破損として数え，ファイル全体は落とさない．
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                broken_lines += 1;
                continue;
            }
        };
        match parse_line(&line, is_sub) {
            ParseOutcome::Broken => broken_lines += 1,
            ParseOutcome::Skip => {}
            ParseOutcome::Prompt { date, hour } => {
                *prompt_map.entry((date, hour)).or_default() += 1;
            }
            ParseOutcome::Record(r) => {
                if !seen.insert(r.message_id.clone()) {
                    continue;
                }
                let key = (r.date.clone(), Some(r.hour), r.model.clone(), r.scope);
                let e = map.entry(key).or_default();
                e.add(&Totals {
                    requests: 1,
                    input: r.input,
                    output: r.output,
                    cache_write_5m: r.cache_write_5m,
                    cache_write_1h: r.cache_write_1h,
                    cache_read: r.cache_read,
                });
            }
        }
    }

    let agg = map
        .into_iter()
        .map(|((date, hour, model, scope), t)| AggEntry {
            date,
            hour,
            model,
            scope,
            requests: t.requests,
            input: t.input,
            output: t.output,
            cache_write_5m: t.cache_write_5m,
            cache_write_1h: t.cache_write_1h,
            cache_read: t.cache_read,
        })
        .collect();

    let prompts = prompt_map
        .into_iter()
        .map(|((date, hour), count)| PromptEntry { date, hour, count })
        .collect();

    Ok(FileAggregation {
        agg,
        prompts,
        broken_lines,
        total_lines,
    })
}

/// `~/.claude/projects` 以下の `*.jsonl` を再帰的に列挙する．
///
/// `<session-id>/subagents/agent-*.jsonl` も含む (費用の大半はここにある)．
pub fn find_transcripts(root: &Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Write;

    /// テスト用に一時ディレクトリを作る (外部 crate を足さないため自前)．
    pub fn tmpdir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("claude-usage-test-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn line(id: &str, ts: &str, model: &str, out: u64) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"{ts}","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":1,"output_tokens":{out},"cache_read_input_tokens":3,"cache_creation":{{"ephemeral_5m_input_tokens":4,"ephemeral_1h_input_tokens":5}}}}}}}}"#
        )
    }

    #[test]
    fn 同一_message_id_は一度だけ数える() {
        let dir = tmpdir("dedup");
        let path = dir.join("s.jsonl");
        let body = [
            line("msg_a", "2026-08-22T01:00:00Z", "claude-opus-5", 10),
            // ストリーミングの途中経過で同じ id が再度書かれる．
            line("msg_a", "2026-08-22T01:00:01Z", "claude-opus-5", 10),
            line("msg_b", "2026-08-22T01:00:02Z", "claude-opus-5", 20),
        ]
        .join("\n");
        write(&path, &body);

        let r = aggregate_file(&path).unwrap();
        assert_eq!(r.agg.len(), 1);
        assert_eq!(r.agg[0].requests, 2);
        assert_eq!(r.agg[0].output, 30);
        assert_eq!(r.broken_lines, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 壊れた行を数えつつファイル全体は読み切る() {
        let dir = tmpdir("broken");
        let path = dir.join("s.jsonl");
        let body = [
            line("msg_a", "2026-08-22T01:00:00Z", "claude-opus-5", 10),
            "{ 壊れた行".to_string(),
            line("msg_b", "2026-08-22T01:00:02Z", "claude-opus-5", 20),
        ]
        .join("\n");
        write(&path, &body);

        let r = aggregate_file(&path).unwrap();
        assert_eq!(r.broken_lines, 1);
        assert_eq!(r.agg[0].requests, 2);
        assert_eq!(r.agg[0].output, 30);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn subagents_配下は_scope_が_subagent_になる() {
        let dir = tmpdir("scope");
        let path = dir.join("sess").join("subagents").join("agent-x.jsonl");
        write(
            &path,
            &line("msg_a", "2026-08-22T01:00:00Z", "claude-sonnet-5", 10),
        );

        let r = aggregate_file(&path).unwrap();
        assert_eq!(r.agg[0].scope, Scope::Subagent);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 日付とモデルで分かれる() {
        let dir = tmpdir("split");
        let path = dir.join("s.jsonl");
        let body = [
            line("m1", "2026-08-22T01:00:00Z", "claude-opus-5", 1),
            line("m2", "2026-08-22T15:00:00Z", "claude-opus-5", 1), // JST では 08-23
            line("m3", "2026-08-22T01:00:00Z", "claude-sonnet-5", 1),
        ]
        .join("\n");
        write(&path, &body);

        let r = aggregate_file(&path).unwrap();
        assert_eq!(r.agg.len(), 3);
        let dates: Vec<_> = r.agg.iter().map(|e| e.date.as_str()).collect();
        assert!(dates.contains(&"2026-08-23"));
        // 時も鍵に入っている．
        assert!(r.agg.iter().all(|e| e.hour.is_some()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 同じ日でも時が違えば別の行になる() {
        let dir = tmpdir("hour-split");
        let path = dir.join("s.jsonl");
        let body = [
            // JST 13:59 と 14:00 — 週の制限の窓が切り替わる境目．
            line("m1", "2026-08-26T04:59:00Z", "claude-opus-5", 1),
            line("m2", "2026-08-26T05:00:00Z", "claude-opus-5", 2),
            line("m3", "2026-08-26T05:30:00Z", "claude-opus-5", 4),
        ]
        .join("\n");
        write(&path, &body);

        let r = aggregate_file(&path).unwrap();
        assert_eq!(r.agg.len(), 2);
        let h13 = r.agg.iter().find(|e| e.hour == Some(13)).unwrap();
        let h14 = r.agg.iter().find(|e| e.hour == Some(14)).unwrap();
        assert_eq!(h13.output, 1);
        assert_eq!(h13.requests, 1);
        assert_eq!(h14.output, 6);
        assert_eq!(h14.requests, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn jsonl_だけを列挙する() {
        let dir = tmpdir("find");
        write(&dir.join("a.jsonl"), "");
        write(&dir.join("sess/subagents/agent-b.jsonl"), "");
        write(&dir.join("note.md"), "");
        let found = find_transcripts(&dir);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|p| path_is_subagent(p)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
