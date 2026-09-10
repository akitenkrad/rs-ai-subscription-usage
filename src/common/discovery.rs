use std::path::{Path, PathBuf};
pub fn recursively_find_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    paths.sort();
    paths
}
