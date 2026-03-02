use clap::{Parser, Subcommand};

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
        #[arg(long, default_value = "10000")]
        history: usize,
    },
    /// Create a new session
    New {
        /// Session name (auto-generated if omitted)
        name: Option<String>,
        /// Scrollback history size
        #[arg(long, default_value = "10000")]
        history: usize,
    },
    /// Attach to an existing session
    Attach {
        /// Session name
        name: String,
        /// Scrollback history size (used if session doesn't exist yet)
        #[arg(long, default_value = "10000")]
        history: usize,
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
