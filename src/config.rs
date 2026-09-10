use std::path::{Path, PathBuf};
#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub vault: PathBuf,
}
impl Config {
    pub fn from_env() -> Self {
        Self {
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/")),
            vault: std::env::var_os("OBSIDIAN_VAULT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/Users/akitenkrad/Documents/Obsidian")),
        }
    }
    pub fn claude_output_dir(&self) -> PathBuf {
        self.vault.join("_logs/_ai-subscription-usage/claude")
    }
    pub fn claude_projects_dir(&self) -> PathBuf {
        self.home.join(".claude/projects")
    }
    pub fn codex_sessions_dir(&self) -> PathBuf {
        self.home.join(".codex/sessions")
    }
    pub fn codex_output_dir(&self) -> PathBuf {
        self.vault.join("_logs/_ai-subscription-usage/codex")
    }
    pub fn state_dir(&self, provider: &str) -> PathBuf {
        self.home
            .join(".local/share/ai-subscription-usage")
            .join(provider)
    }
    pub fn validate_month(month: &str) -> bool {
        month.len() == 7
            && month.as_bytes()[4] == b'-'
            && month[..4].bytes().all(|b| b.is_ascii_digit())
            && month[5..].bytes().all(|b| b.is_ascii_digit())
    }
    pub fn pricing_path(output_dir: &Path) -> PathBuf {
        output_dir.join("pricing.json")
    }

    /// 旧CLIの出力を新しいprovider別ディレクトリへ一度だけ移行する．
    /// 既に新パスに存在するファイルは上書きしない．
    pub fn migrate_legacy_output(&self, provider: &str) -> std::io::Result<usize> {
        let old = self.vault.join("_logs").join(format!("_{provider}-usage"));
        let new = self
            .vault
            .join("_logs/_ai-subscription-usage")
            .join(provider);
        if !old.is_dir() {
            return Ok(0);
        }
        std::fs::create_dir_all(&new)?;
        let mut copied = 0;
        for entry in std::fs::read_dir(&old)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let dst = new.join(entry.file_name());
            if dst.exists() {
                continue;
            }
            std::fs::copy(entry.path(), dst)?;
            copied += 1;
        }
        Ok(copied)
    }
}
