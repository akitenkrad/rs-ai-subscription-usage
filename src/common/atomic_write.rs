use std::path::Path;
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let ext = path
        .extension()
        .map(|v| v.to_string_lossy())
        .unwrap_or_default();
    let tmp = path.with_extension(format!("{ext}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}
