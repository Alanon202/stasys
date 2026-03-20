pub mod commands;
pub mod pause;

use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    time::{Duration, timeout},
};

use crate::{
    config, core::{
        manager::{helpers::{get_manual_inhibit, set_manual_inhibit, trigger_all_idle_actions}, Manager}, 
        services::app_inhibit::AppInhibitor,
        utils::format_duration,
    }, 
    ipc::commands::trigger_action_by_name, 
    log::{log_error_message, log_message}, 
    SOCKET_PATH
};

/// Spawn the IPC control socket task using a pre-bound listener.
pub async fn spawn_ipc_socket_with_listener(
    manager: Arc<tokio::sync::Mutex<Manager>>,
    app_inhibitor: Arc<tokio::sync::Mutex<AppInhibitor>>,
    listener: UnixListener,
) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut stream, _addr)) => {
                    let manager = Arc::clone(&manager);
                    let app_inhibitor = Arc::clone(&app_inhibitor);

                    tokio::spawn(async move {
                        let result = timeout(Duration::from_secs(10), async {
                            let mut buf = vec![0u8; 256];
                            match stream.read(&mut buf).await {
                                Ok(n) if n > 0 => {
                                    let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                                    if !cmd.contains("--json") {
                                        log_message(&format!("Received IPC command: {}", cmd));
                                    }

                                    let response = match cmd.as_str() {
                                        // === CONFIG ===
                                        "reload" => {
                                            match config::parser::load_config() {
                                                Ok(new_cfg) => {
                                                    let mut mgr = manager.lock().await;
                                                    mgr.state.update_from_config(&new_cfg).await;
                                                    mgr.recheck_media().await;
                                                    mgr.trigger_instant_actions().await;

                                                    let idle_time = mgr.state.last_activity.elapsed();
                                                    let uptime = mgr.state.start_time.elapsed();
                                                    let manually_inhibited = mgr.state.manually_paused;
                                                    let paused = mgr.state.paused;
                                                    let media_blocking = mgr.state.media_blocking;
                                                    let cfg_clone = mgr.state.cfg.clone();

                                                    drop(mgr);

                                                    {
                                                        let mut inhibitor = app_inhibitor.lock().await;
                                                        inhibitor.update_from_config(&new_cfg).await;
                                                    }

                                                    let app_blocking = match timeout(
                                                        Duration::from_millis(100),
                                                        async {
                                                            let mut inhibitor = app_inhibitor.lock().await;
                                                            inhibitor.is_any_app_running().await
                                                        }
                                                    ).await {
                                                        Ok(result) => result,
                                                        Err(_) => false,
                                                    };

                                                    log_message("Config reloaded successfully");

                                                    // Load active profile name
                                                    let active_profile = dirs::home_dir()
                                                        .map(|mut p| {
                                                            p.push(".config/stasys/active_profile");
                                                            p
                                                        })
                                                        .and_then(|path| std::fs::read_to_string(&path).ok())
                                                        .map(|s| s.trim().to_string());
                                                    
                                                    let profile_display = active_profile.as_deref().filter(|s| !s.is_empty() && *s != "none");

                                                    if let Some(cfg) = &cfg_clone {
                                                        format!(
                                                            "Config reloaded successfully\n\n{}",
                                                            cfg.pretty_print(
                                                                Some(idle_time),
                                                                Some(uptime),
                                                                Some(paused),
                                                                Some(manually_inhibited),
                                                                Some(app_blocking),
                                                                Some(media_blocking),
                                                                profile_display
                                                            )
                                                        )
                                                    } else {
                                                        "Config reloaded successfully".to_string()
                                                    }
                                                }
                                                Err(e) => {
                                                    log_error_message(&format!("Failed to reload config: {}", e));
                                                    format!("ERROR: Failed to reload config: {e}")
                                                }
                                            }
                                        }

                                        // === PROFILES ===
                                        cmd if cmd.starts_with("profile ") => {
                                            let profile_name = cmd.strip_prefix("profile ").unwrap_or("").trim();
                                            if profile_name.is_empty() {
                                                "ERROR: Profile name required. Use 'stasys profile list' to see available profiles".to_string()
                                            } else {
                                                match commands::switch_profile(manager.clone(), profile_name).await {
                                                    Ok(msg) => msg,
                                                    Err(e) => format!("ERROR: {}", e),
                                                }
                                            }
                                        }

                                        // === PAUSE/RESUME ===
                                        cmd if cmd.starts_with("pause") => {
                                            let args = cmd.strip_prefix("pause").unwrap_or("").trim();

                                            if args.eq_ignore_ascii_case("help") 
                                                || args == "-h" 
                                                || args == "--help" {
                                                pause::PAUSE_HELP_MESSAGE.to_string()
                                            } else {
                                                match pause::handle_pause_command(manager.clone(), args).await {
                                                    Ok(msg) => msg,
                                                    Err(e) => format!("ERROR: {}", e),
                                                }
                                            }
                                        }

                                        "resume" => {
                                            let mut mgr = manager.lock().await;
                                            mgr.resume(true).await;
                                            "Idle manager resumed".to_string()
                                        }

                                        // === TRIGGER ACTIONS ===
                                        cmd if cmd.starts_with("trigger ") => {
                                            let step = cmd.strip_prefix("trigger ").unwrap_or("").trim();

                                            if step.is_empty() {
                                                log_error_message("Trigger command missing action name");
                                                "ERROR: No action name provided".to_string()
                                            } else if step == "all" {
                                                let mut mgr = manager.lock().await;
                                                trigger_all_idle_actions(&mut mgr).await;
                                                log_message("Triggered all idle actions");
                                                "All idle actions triggered".to_string()
                                            } else {
                                                match trigger_action_by_name(manager.clone(), step).await {
                                                    Ok(action) => format!("Action '{}' triggered successfully", action),
                                                    Err(e) => format!("ERROR: {e}"),
                                                }
                                            }
                                        }

                                        // === CONTROL ===
                                        "stop" => {
                                            log_message("Received stop command — shutting down gracefully");
                                            let manager_clone = Arc::clone(&manager);
                                            tokio::spawn(async move {
                                                let mut mgr = manager_clone.lock().await;
                                                mgr.shutdown().await;
                                                log_message("Manager shutdown complete, exiting process");
                                                let _ = std::fs::remove_file(SOCKET_PATH);
                                                std::process::exit(0);
                                            });
                                            "Stopping Stasis...".to_string()
                                        }

                                        "toggle_inhibit" => {
                                            let mut mgr = manager.lock().await;
                                            let currently_inhibited = get_manual_inhibit(&mut mgr.state);

                                            if currently_inhibited {
                                                set_manual_inhibit(&mut mgr, false).await;
                                                log_message("Manual inhibit disabled (toggle)");
                                            } else {
                                                set_manual_inhibit(&mut mgr, true).await;
                                                log_message("Manual inhibit enabled (toggle)");
                                            }

                                            let response = if currently_inhibited {
                                                serde_json::json!({
                                                    "text": "Active",
                                                    "alt": "idle_active",
                                                    "tooltip": "Idle inhibition cleared"
                                                })
                                            } else {
                                                serde_json::json!({
                                                    "text": "Inhibited",
                                                    "alt": "manually_inhibited",
                                                    "tooltip": "Idle inhibition active"
                                                })
                                            };
                                            
                                            response.to_string()
                                        }

                                        // === INFO ===
                                        "info" | "info --json" => {
                                            let as_json = cmd.contains("--json");
                                            let mut retry_count = 0;
                                            let max_retries = 5;

                                            loop {
                                                match manager.try_lock() {
                                                    Ok(mgr) => {
                                                        let idle_time = mgr.state.last_activity.elapsed();
                                                        let uptime = mgr.state.start_time.elapsed();
                                                        let manually_inhibited = mgr.state.manually_paused;
                                                        let paused = mgr.state.paused;
                                                        let media_blocking = mgr.state.media_blocking;
                                                        let cfg_clone = mgr.state.cfg.clone();

                                                        drop(mgr);

                                                        let app_blocking = match timeout(
                                                            Duration::from_millis(100),
                                                            async {
                                                                let mut inhibitor = app_inhibitor.lock().await;
                                                                inhibitor.is_any_app_running().await
                                                            }
                                                        ).await {
                                                            Ok(result) => result,
                                                            Err(_) => false,
                                                        };

                                                        let idle_inhibited = paused || app_blocking || manually_inhibited;

                                                        break if as_json {
                                                            // Load active profile for JSON tooltip
                                                            let active_profile = dirs::home_dir()
                                                                .map(|mut p| {
                                                                    p.push(".config/stasys/active_profile");
                                                                    p
                                                                })
                                                                .and_then(|path| std::fs::read_to_string(&path).ok())
                                                                .map(|s| s.trim().to_string());

                                                            let profile_display = active_profile.as_deref().filter(|s| !s.is_empty() && *s != "none").unwrap_or("base config");

                                                            let (text, icon) = if manually_inhibited {
                                                                ("Inhibited", "manually_inhibited")
                                                            } else if idle_inhibited {
                                                                ("Blocked", "idle_inhibited")
                                                            } else {
                                                                ("Active", "idle_active")
                                                            };

                                                            serde_json::json!({
                                                                "text": text,
                                                                "alt": icon,
                                                                "tooltip": format!(
                                                                    "{}\nProfile: {}\nIdle time: {}\nUptime: {}\nPaused: {}\nManually paused: {}\nApp blocking: {}\nMedia blocking: {}",
                                                                    if idle_inhibited { "Idle inhibited" } else { "Idle active" },
                                                                    profile_display,
                                                                    format_duration(idle_time),
                                                                    format_duration(uptime),
                                                                    paused,
                                                                    manually_inhibited,
                                                                    app_blocking,
                                                                    media_blocking
                                                                )
                                                            })
                                                            .to_string()
                                                        } else if let Some(cfg) = &cfg_clone {
                                                            // Load active profile for pretty print
                                                            let active_profile = dirs::home_dir()
                                                                .map(|mut p| {
                                                                    p.push(".config/stasys/active_profile");
                                                                    p
                                                                })
                                                                .and_then(|path| std::fs::read_to_string(&path).ok())
                                                                .map(|s| s.trim().to_string());

                                                            let profile_display = active_profile.as_deref().filter(|s| !s.is_empty() && *s != "none");

                                                            cfg.pretty_print(
                                                                Some(idle_time),
                                                                Some(uptime),
                                                                Some(idle_inhibited),
                                                                Some(manually_inhibited),
                                                                Some(app_blocking),
                                                                Some(media_blocking),
                                                                profile_display
                                                            )
                                                        } else {
                                                            "No configuration loaded".to_string()
                                                        };
                                                    }
                                                    Err(_) => {
                                                        retry_count += 1;
                                                        if retry_count >= max_retries {
                                                            break if as_json {
                                                                serde_json::json!({
                                                                    "text": "",
                                                                    "alt": "not_running",
                                                                    "tooltip": "Busy, try again"
                                                                }).to_string()
                                                            } else {
                                                                "Manager is busy, try again".to_string()
                                                            };
                                                        }
                                                        tokio::time::sleep(Duration::from_millis(20)).await;
                                                    }
                                                }
                                            }
                                        }

                                        "list_actions" => {
                                            match crate::ipc::commands::list_available_actions(manager.clone()).await.as_slice() {
                                                [] => "No actions available".to_string(),
                                                actions => actions.join(", "),
                                            }
                                        }

                                        _ => {
                                            log_error_message(&format!("Unknown IPC command: {}", cmd));
                                            format!("ERROR: Unknown command '{}'", cmd)
                                        }
                                    };

                                    if let Err(e) = stream.write_all(response.as_bytes()).await {
                                        log_error_message(&format!("Failed to write IPC response: {e}"));
                                    } else {
                                        let _ = stream.flush().await;
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    log_error_message(&format!("Failed to read IPC command: {e}"));
                                }
                            }
                        }).await;

                        if result.is_err() {
                            log_error_message("IPC connection timed out after 10 seconds");
                        }

                        let _ = stream.shutdown().await;
                    });
                }

                Err(e) => log_error_message(&format!("Failed to accept IPC connection: {}", e)),
            }
        }
    });
}
