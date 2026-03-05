use clap::{Parser, Subcommand};

const DEFAULT_HISTORY: usize = 10000;
const MAX_HISTORY: usize = 1_000_000;

fn parse_history(s: &str) -> Result<usize, String> {
    let val: usize = s.parse().map_err(|e| format!("{e}"))?;
    if val > MAX_HISTORY {
        return Err(format!("history size must be at most {MAX_HISTORY}"));
    }
    Ok(val)
}

#[derive(Parser)]
#[command(name = "retach", version, about = "Terminal multiplexer with native scrollback")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Open a session: attach if it exists, create if not
    Open {
        /// Session name
        name: String,
        /// Scrollback history size (used when creating)
        #[arg(long, default_value_t = DEFAULT_HISTORY, value_parser = parse_history)]
        history: usize,
    },
    /// Create a new session
    New {
        /// Session name (auto-generated if omitted)
        name: Option<String>,
        /// Scrollback history size
        #[arg(long, default_value_t = DEFAULT_HISTORY, value_parser = parse_history)]
        history: usize,
    },
    /// Attach to an existing session
    Attach {
        /// Session name
        name: String,
    },
    /// List active sessions
    List,
    /// Kill a session
    Kill {
        /// Session name
        name: String,
    },
    /// Start the server (internal)
    #[command(hide = true)]
    Server,
}
