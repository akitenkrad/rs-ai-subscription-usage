use ai_subscription_usage::cli::{Cli, ProviderCommand};
use clap::Parser;

fn main() {
    let result = match Cli::parse().provider {
        ProviderCommand::Claude(args) => ai_subscription_usage::claude::run(args),
        ProviderCommand::Codex(args) => ai_subscription_usage::codex::run(args),
        ProviderCommand::All(_) => Err(ai_subscription_usage::Error::Message(
            "all は未実装です".into(),
        )),
    };
    if let Err(error) = result {
        eprintln!("[ERROR] {error}");
        std::process::exit(1);
    }
}
