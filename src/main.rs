//! retach — terminal multiplexer with native scrollback passthrough.
//!
//! Instead of intercepting scrollback (like tmux/screen), retach sends completed
//! lines directly to stdout, letting the client terminal handle scrolling natively.

mod cli;
mod client;
mod protocol;
mod pty;
mod screen;
mod server;
mod session;

use clap::Parser;
use cli::{Cli, Command};
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("retach=info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let rt = tokio::runtime::Runtime::new()?;

    let result = match cli.command {
        Command::Open { name, history } => {
            rt.block_on(client::connect(&name, history, protocol::ConnectMode::CreateOrAttach))
        }
        Command::New { name, history } => {
            let name = name.unwrap_or_else(generate_name);
            rt.block_on(client::connect(&name, history, protocol::ConnectMode::CreateOnly))
        }
        Command::Attach { name } => {
            rt.block_on(client::connect(&name, 0, protocol::ConnectMode::AttachOnly))
        }
        Command::List => {
            rt.block_on(client::list_sessions())
        }
        Command::Kill { name } => {
            rt.block_on(client::kill_session(&name))
        }
        Command::Server => {
            rt.block_on(server::run_server())
        }
    };

    // Shut down runtime with timeout to avoid hanging on blocked stdin thread
    rt.shutdown_timeout(std::time::Duration::from_millis(100));

    result
}

fn generate_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState::new() seeds from OS randomness, giving real uniqueness
    // without adding external dependencies.
    let hash = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    format!("s-{:016x}", hash)
}
