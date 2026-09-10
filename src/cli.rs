use clap::{Args, Parser, Subcommand};
#[derive(Debug, Parser)]
#[command(
    name = "ai-subscription-usage",
    version,
    about = "AIサブスクリプション利用量を集計する"
)]
pub struct Cli {
    #[command(subcommand)]
    pub provider: ProviderCommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    Claude(ProviderArgs),
    Codex(ProviderArgs),
    All(ProviderArgs),
}
#[derive(Debug, Clone, Args)]
pub struct ProviderArgs {
    #[command(subcommand)]
    pub command: Option<ProviderOperation>,
    #[arg(long)]
    pub all: bool,
    #[arg(long, value_name = "YYYY-MM")]
    pub month: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub forget_missing: bool,
}
#[derive(Debug, Clone, Subcommand)]
pub enum ProviderOperation {
    Limits,
}
