use clap::{Parser, Subcommand};

const DEFAULT_HISTORY: usize = 10000;

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
        #[arg(long, default_value_t = DEFAULT_HISTORY)]
        history: usize,
    },
    /// Create a new session
    New {
        /// Session name (auto-generated if omitted)
        name: Option<String>,
        /// Scrollback history size
        #[arg(long, default_value_t = DEFAULT_HISTORY)]
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
