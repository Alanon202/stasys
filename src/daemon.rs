use std::sync::Arc;
use eyre::Result;
use tokio::{
    net::UnixListener,
    sync::Mutex,
    task::LocalSet,
    time::Duration,
};

use crate::{
    config::parser::load_config,
    core::{
        manager::{idle_loops::{spawn_idle_task, spawn_lock_watcher}, Manager},
        services::{
            app_inhibit::{AppInhibitor, spawn_app_inhibit_task},
            dbus::listen_for_power_events,
            input::spawn_input_task,
            media::spawn_media_monitor_dbus,
            power_detection::{detect_initial_power_state, spawn_power_source_monitor},
            wayland::setup as setup_wayland,
        }
    },
    log::{log_error_message, log_message},
    ipc,
    SOCKET_PATH,
};

/// Spawn the daemon with all its background services
pub async fn run_daemon(listener: UnixListener, verbose: bool) -> Result<()> {
    // --- Load config ---
    if verbose {
        log_message("Verbose mode enabled");
        crate::log::set_verbose(true);
    }
    
    let cfg = Arc::new(load_config()?);
    let manager = Manager::new(Arc::clone(&cfg));
    let manager = Arc::new(Mutex::new(manager));

    // --- Spawn background tasks ---
    let idle_handle = spawn_idle_task(Arc::clone(&manager));
    let lock_handle = spawn_lock_watcher(Arc::clone(&manager)).await;
    let input_handle = spawn_input_task(Arc::clone(&manager));
    
    // Store handles in manager - CRITICAL: Use explicit scope to drop lock
    {
        let mut mgr = manager.lock().await;
        mgr.idle_task_handle = Some(idle_handle);
        mgr.lock_task_handle = Some(lock_handle);
        mgr.input_task_handle = Some(input_handle);
    } // Lock is dropped here before continuing
    
    // --- Spawn suspend event listener ---
    let dbus_manager = Arc::clone(&manager);
    tokio::spawn(async move {
        if let Err(e) = listen_for_power_events(dbus_manager).await {
            log_error_message(&format!("D-Bus suspend event listener failed: {}", e));
        }
    });

    // --- AC/Battery Detection (DETECT FIRST, synchronously) ---
    // Use explicit scope like in original working code
    {
        detect_initial_power_state(&manager).await;
    }
    
    // --- AC/Battery Detection ---
    let laptop_manager = Arc::clone(&manager);
    tokio::spawn(spawn_power_source_monitor(laptop_manager));

    // Immediately trigger instants at startup
    {
        let mut mgr = manager.lock().await;
        mgr.trigger_instant_actions().await;
    } // Lock dropped here
    
    // --- Spawn app inhibit task ---
    let app_inhibitor = spawn_app_inhibit_task(
        Arc::clone(&manager),
        Arc::clone(&cfg)
    ).await;
   
    // --- Spawn media monitor task ---
    if cfg.monitor_media {
        if let Err(e) = spawn_media_monitor_dbus(Arc::clone(&manager)).await {
            log_error_message(&format!("Failed to spawn media monitor: {}", e));
        }
    }
    
    // --- Wayland setup ---
    let wayland_manager = Arc::clone(&manager);
    let _ = setup_wayland(wayland_manager, cfg.respect_wayland_inhibitors).await?;

    // -- IPC Control Socket ---
    ipc::spawn_ipc_socket_with_listener(
        Arc::clone(&manager),
        Arc::clone(&app_inhibitor),
        listener,
    ).await;

    setup_shutdown_handler(
        Arc::clone(&manager),
        Arc::clone(&app_inhibitor),
    ).await;

    // Monitor Wayland compositor connection
    spawn_wayland_monitor(
        Arc::clone(&manager),
        Arc::clone(&app_inhibitor),
    ).await;
    
    // --- Log startup message ---
    log_message(&format!("Running. Idle actions loaded: {}", cfg.actions.len()));

    // --- Spawn periodic memory trim task ---
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(600)); // Every 10 minutes
        loop {
            interval.tick().await;
            crate::log::trim_memory();
        }
    });
    
    // --- Run main async tasks ---
    let local = LocalSet::new();
    local.run_until(async {
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok::<(), eyre::Report>(())
    }).await
}

/// Async shutdown handler (Ctrl+C / SIGTERM)
async fn setup_shutdown_handler(
    idle_timer: Arc<Mutex<Manager>>,
    app_inhibitor: Arc<Mutex<AppInhibitor>>,
) {
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();

    tokio::spawn({
        let manager = Arc::clone(&idle_timer);
        let app_inhibitor = Arc::clone(&app_inhibitor);
        async move {
            tokio::select! {
                _ = sigint.recv() => {
                    log_message("Received SIGINT, shutting down...");
                },
                _ = sigterm.recv() => {
                    log_message("Received SIGTERM, shutting down...");
                },
                _ = sighup.recv() => {
                    log_message("Received SIGHUP, shutting down...");
                },
            }

            // Shutdown idle timer
            manager.lock().await.shutdown().await;

            // Shutdown app inhibitor
            app_inhibitor.lock().await.shutdown().await;

            let _ = std::fs::remove_file(SOCKET_PATH);
            log_message("Shutdown complete, goodbye!");
            std::process::exit(0);
        }
    });
}

async fn spawn_wayland_monitor(
    manager: Arc<Mutex<Manager>>,
    app_inhibitor: Arc<Mutex<AppInhibitor>>,
) {
    use tokio::net::UnixStream;
    
    // Capture env vars once
    let wayland_display = match std::env::var("WAYLAND_DISPLAY") {
        Ok(display) => display,
        Err(_) => return,
    };
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
    let socket_path = format!("{}/{}", xdg_runtime, wayland_display);

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Try connecting to the Wayland socket
            if UnixStream::connect(&socket_path).await.is_err() {
                log_message("Wayland compositor is no longer responding, shutting down...");

                // Shutdown idle timer
                manager.lock().await.shutdown().await;

                // Shutdown app inhibitor
                app_inhibitor.lock().await.shutdown().await;

                let _ = std::fs::remove_file(SOCKET_PATH);
                log_message("Shutdown complete, goodbye!");
                std::process::exit(0);
            }
        }
    });
}
