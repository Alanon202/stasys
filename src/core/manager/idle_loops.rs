use std::{sync::Arc, time::{Duration, Instant}};
use tokio::{
    sync::Mutex, 
    task::JoinHandle, 
    time::{Instant as TokioInstant, sleep, sleep_until}
};

use crate::{
    core::{manager::{Manager, actions::{is_process_running, run_command_detached}}}, 
    log::log_message
};

pub fn spawn_idle_task(manager: Arc<Mutex<Manager>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Grab both the next timeout and the notify handles
            let (next_instant, notify, shutdown) = {
                let mgr = manager.lock().await;
                (
                    mgr.next_action_instant(),
                    mgr.state.notify.clone(),
                    mgr.state.shutdown_flag.clone(),
                )
            };

            // Compute how long we should sleep           
            let sleep_deadline = match next_instant {
                Some(instant) => {
                    let now = Instant::now();
                    let max_sleep = Duration::from_secs(60); // wake periodically
                    if instant <= now {
                        now + Duration::from_millis(50)
                    } else if instant - now > max_sleep {
                        now + max_sleep
                    } else {
                        instant
                    }
                }
                None => Instant::now() + Duration::from_secs(60),
            };


            tokio::select! {
                _ = sleep_until(TokioInstant::from_std(sleep_deadline)) => {},
                _ = notify.notified() => {
                    // Woken up by external event (reset, AC change, playback)
                    continue; // recalc immediately
                }
                _ = shutdown.notified() => {
                    break; // exit loop cleanly
                }
            }

            // Now check timeouts only once after wake
            let mut mgr = manager.lock().await;
            if !mgr.state.paused && !mgr.state.manually_paused {
                mgr.check_timeouts().await;
            }
        }

        log_message("Idle loop shutting down...");
    })
}

pub async fn spawn_lock_watcher(manager: Arc<Mutex<Manager>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Grab shutdown notify handle outside
            let shutdown = {
                let mgr = manager.lock().await;
                mgr.state.shutdown_flag.clone()
            };

            // Wait until lock actually becomes active
            {
                let mut mgr = manager.lock().await;
                while !mgr.state.lock_state.is_locked {
                    let lock_notify = mgr.state.lock_notify.clone();
                    drop(mgr);
                    tokio::select! {
                        _ = lock_notify.notified() => {},
                        _ = shutdown.notified() => {
                            log_message("Lock watcher shutting down...");
                            return;
                        }
                    }
                    mgr = manager.lock().await;
                }
            }

            log_message("Lock detected — entering lock watcher");

            // Lock is active — monitor it until it ends or shutdown
            loop {
                // Snapshot relevant info
                let (maybe_cmd, was_locked, shutdown, lock_notify) = {
                    let mgr = manager.lock().await;
                    (
                        mgr.state.lock_state.command.clone(),
                        mgr.state.lock_state.is_locked,
                        mgr.state.shutdown_flag.clone(),
                        mgr.state.lock_notify.clone(),
                    )
                };

                if !was_locked {
                    break;
                }

                // Check if process is still running (if we have a command)
                let still_active = if let Some(cmd) = maybe_cmd {
                    is_process_running(&cmd).await
                } else {
                    sleep(Duration::from_millis(500)).await;
                    true
                };

                if !still_active {
                    let mut mgr = manager.lock().await;

                    if !mgr.state.lock_state.is_locked {
                        break;  // Already unlocked, don't do it again
                    }

                    if let Some(lock_action) = mgr.state.default_actions.iter()
                        .chain(mgr.state.ac_actions.iter())
                        .chain(mgr.state.battery_actions.iter())
                        .find(|a| matches!(a.kind, crate::config::model::IdleAction::LockScreen))
                    {
                        if let Some(resume_cmd) = &lock_action.resume_command {
                            log_message("Firing lockscreen resume command");
                            if let Err(e) = run_command_detached(resume_cmd).await {
                                log_message(&format!("Failed to run lock resume command: {}", e));
                            }
                        }
                    }

                    mgr.state.lock_state.pid = None;
                    mgr.state.lock_state.is_locked = false;

                    // For loginctl locks, don't reset action_index (no PID to monitor)
                    if mgr.state.lock_state.command.is_some() {
                        mgr.state.action_index = 0;
                        mgr.reset().await;
                    }

                    log_message("Lockscreen ended — exiting lock watcher");
                    break;
                }

                // Wait a bit or for external change / shutdown
                tokio::select! {
                    _ = lock_notify.notified() => {},
                    _ = sleep(Duration::from_millis(500)) => {},
                    _ = shutdown.notified() => {
                        log_message("Lock watcher shutting down during active lock...");
                        return;
                    }
                }
            }
        }
    })
}
