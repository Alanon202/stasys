use std::process;
use eyre::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::{timeout, Duration},
};

use crate::{cli::Command, SOCKET_PATH};

/// Handle client commands that communicate with the daemon via socket
pub async fn handle_client_command(cmd: &Command) -> Result<()> {
    match cmd {
        Command::Info { json } => handle_info(*json).await,
        Command::Trigger { step } => handle_trigger(step).await,
        Command::ListActions => handle_list_actions().await,
        Command::Pause { args } => handle_pause(args).await,
        Command::Reload => handle_simple_command("reload", "Configuration reloaded successfully").await,
        Command::Resume => handle_simple_command("resume", "Idle timers resumed").await,
        Command::Stop => handle_simple_command("stop", "Stasis daemon stopped").await,
        Command::ToggleInhibit => handle_toggle_inhibit().await,
        Command::Profile { name } => handle_profile(name).await,
    }
}

async fn handle_info(json: bool) -> Result<()> {
    match timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH)).await {
        Ok(Ok(mut stream)) => {
            let msg = if json { "info --json" } else { "info" };
            let _ = stream.write_all(msg.as_bytes()).await;

            let mut response = Vec::new();
            match timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await {
                Ok(Ok(_)) => println!("{}", String::from_utf8_lossy(&response)),
                Ok(Err(e)) => {
                    if json {
                        println!(r#"{{"text":"", "alt": "not_running", "tooltip":"Read error"}}"#);
                    } else {
                        eprintln!("Failed to read response: {}", e);
                    }
                }
                Err(_) => {
                    if json {
                        println!(r#"{{"text":"", "alt": "not_running", "tooltip":"Connection timeout"}}"#);
                    } else {
                        eprintln!("Timeout reading from Stasis");
                    }
                }
            }
        }
        Ok(Err(_)) | Err(_) => {
            if json {
                println!(r#"{{"text":"", "alt": "not_running", "tooltip":"No running Stasis instance found"}}"#);
            } else {
                eprintln!("No running Stasis instance found");
                process::exit(1);
            }
        }
    }
    Ok(())
}

async fn handle_trigger(step: &str) -> Result<()> {
    match timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH)).await {
        Ok(Ok(mut stream)) => {
            let msg = format!("trigger {}", step);
            let _ = stream.write_all(msg.as_bytes()).await;

            let mut response = Vec::new();
            match timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await {
                Ok(Ok(_)) => {
                    let response_text = String::from_utf8_lossy(&response);
                    if response_text.starts_with("ERROR:") {
                        eprintln!("{}", response_text.trim_start_matches("ERROR:").trim());
                        process::exit(1);
                    } else if !response_text.is_empty() {
                        println!("{}", response_text);
                    } else {
                        println!("Action '{}' triggered", step);
                    }
                }
                Ok(Err(e)) => eprintln!("Failed to read response: {}", e),
                Err(_) => eprintln!("Timeout reading response"),
            }
        }
        Ok(Err(_)) | Err(_) => {
            eprintln!("No running Stasis instance found");
            process::exit(1);
        }
    }
    Ok(())
}

async fn handle_list_actions() -> Result<()> {
    match timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH)).await {
        Ok(Ok(mut stream)) => {
            let _ = stream.write_all(b"list_actions").await;

            let mut response = Vec::new();
            match timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await {
                Ok(Ok(_)) => println!("{}", String::from_utf8_lossy(&response)),
                Ok(Err(e)) => eprintln!("Failed to read response: {}", e),
                Err(_) => eprintln!("Timeout reading response"),
            }
        }
        Ok(Err(_)) | Err(_) => {
            eprintln!("No running Stasis instance found");
            process::exit(1);
        }
    }
    Ok(())
}

async fn handle_pause(args: &[String]) -> Result<()> {
    // Check for help flags before sending to daemon
    if !args.is_empty() {
        let first_arg = args[0].as_str();
        if first_arg == "help" || first_arg == "--help" || first_arg == "-h" {
            println!("{}", crate::ipc::pause::PAUSE_HELP_MESSAGE);
            return Ok(());
        }
    }

    // Build the pause command from args
    let msg = if args.is_empty() {
        "pause".to_string()
    } else {
        format!("pause {}", args.join(" "))
    };

    match timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH)).await {
        Ok(Ok(mut stream)) => {
            let _ = stream.write_all(msg.as_bytes()).await;

            let mut response = Vec::new();
            let _ = timeout(Duration::from_millis(500), stream.read_to_end(&mut response)).await;
            
            let response_text = String::from_utf8_lossy(&response);
            if response_text.starts_with("ERROR:") {
                let error_msg = response_text.trim_start_matches("ERROR:").trim();
                // If it's a help message, print it directly without error formatting
                if error_msg.contains("Usage:") || error_msg.contains("Duration format:") {
                    println!("{}", error_msg);
                } else {
                    eprintln!("{}", error_msg);
                    process::exit(1);
                }
            } else if !response_text.is_empty() {
                println!("{}", response_text);
            } else {
                println!("Idle timers paused");
            }
        }
        Ok(Err(_)) | Err(_) => {
            eprintln!("No running Stasis instance found");
            process::exit(1);
        }
    }
    Ok(())
}

async fn handle_toggle_inhibit() -> Result<()> {
    match timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH)).await {
        Ok(Ok(mut stream)) => {
            let _ = stream.write_all(b"toggle_inhibit").await;

            let mut response = Vec::new();
            match timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await {
                Ok(Ok(_)) => println!("{}", String::from_utf8_lossy(&response)),
                Ok(Err(e)) => eprintln!("Failed to read response: {}", e),
                Err(_) => eprintln!("Timeout reading toggle response"),
            }
        }
        Ok(Err(_)) | Err(_) => {
            eprintln!("No running Stasis instance found");
            process::exit(1);
        }
    }
    Ok(())
}

async fn handle_simple_command(command: &str, success_msg: &str) -> Result<()> {
    match timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH)).await {
        Ok(Ok(mut stream)) => {
            let _ = stream.write_all(command.as_bytes()).await;

            let mut response = Vec::new();
            match timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await {
                Ok(Ok(_)) => {
                    let response_text = String::from_utf8_lossy(&response);
                    if response_text.starts_with("ERROR:") {
                        eprintln!("{}", response_text.trim_start_matches("ERROR:").trim());
                        process::exit(1);
                    } else if !response_text.is_empty() {
                        // Print the actual response from the daemon
                        println!("{}", response_text);
                    } else {
                        // Fallback to success message if response is empty
                        println!("{}", success_msg);
                    }
                }
                Ok(Err(e)) => eprintln!("Failed to read response: {}", e),
                Err(_) => {
                    // On timeout, still show success message
                    println!("{}", success_msg);
                }
            }
        }
        Ok(Err(_)) | Err(_) => {
            eprintln!("No running Stasis instance found");
            process::exit(1);
        }
    }
    Ok(())
}

async fn handle_profile(name: &str) -> Result<()> {
    match timeout(Duration::from_secs(3), UnixStream::connect(SOCKET_PATH)).await {
        Ok(Ok(mut stream)) => {
            let msg = format!("profile {}", name);
            let _ = stream.write_all(msg.as_bytes()).await;

            let mut response = Vec::new();
            match timeout(Duration::from_secs(2), stream.read_to_end(&mut response)).await {
                Ok(Ok(_)) => {
                    let response_text = String::from_utf8_lossy(&response);
                    if response_text.starts_with("ERROR:") {
                        eprintln!("{}", response_text.trim_start_matches("ERROR:").trim());
                        process::exit(1);
                    } else if !response_text.is_empty() {
                        println!("{}", response_text);
                    }
                }
                Ok(Err(e)) => eprintln!("Failed to read response: {}", e),
                Err(_) => eprintln!("Timeout reading from Stasis"),
            }
        }
        Ok(Err(_)) | Err(_) => {
            eprintln!("No running Stasis instance found");
            process::exit(1);
        }
    }
    Ok(())
}
