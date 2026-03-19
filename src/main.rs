pub mod cli;
pub mod client;
pub mod config;
pub mod core;
pub mod daemon;
pub mod ipc;
pub mod log;

use std::{env::var, fs, process::exit};
use clap::Parser;
use eyre::Result;
use tokio::net::{UnixListener, UnixStream};

use crate::{
    cli::Args,
    log::log_error_message,
};

pub const SOCKET_PATH: &str = "/tmp/stasys.sock";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    if var("WAYLAND_DISPLAY").is_err() {
        eprintln!("Warn: Stasis requires wayland to run.");
        exit(1);
    }

    // --- Handle subcommands via socket ---
    if let Some(cmd) = &args.command {
        return client::handle_client_command(cmd).await;
    }
    
    // --- Single Instance enforcement ---
    let just_help_or_version = std::env::args().any(|a| matches!(a.as_str(), "-V" | "--version" | "-h" | "--help" | "help"));
    if UnixStream::connect(SOCKET_PATH).await.is_ok() {
        if !just_help_or_version {
            eprintln!("Another instance of Stasis is already running");
        }
        log_error_message("Another instance is already running.");
        return Ok(());
    }
    
    let _ = fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH).map_err(|_| {
        eyre::eyre!("Failed to bind control socket. Another instance may be running.")
    })?;

    // --- Ensure user config exists ---
    if let Err(e) = config::bootstrap::ensure_user_config_exists() {
        eprintln!("Could not initialize config: {}", e);
    }
    
    // --- Run daemon ---
    daemon::run_daemon(listener, args.verbose).await
}
