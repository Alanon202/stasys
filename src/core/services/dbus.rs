use std::{collections::{HashMap, HashSet}, sync::Arc, time::Duration};
use futures_util::stream::StreamExt;
use tokio::sync::Mutex;
use zbus::{Connection, fdo::Result as ZbusResult, Proxy, MatchRule};
use zvariant::Value;
use crate::core::events::handlers::{handle_event, Event};
use crate::core::manager::Manager;
use crate::log::log_message;
use crate::core::manager::helpers::{incr_active_inhibitor, decr_active_inhibitor};

// Helper to reconnect to a D-Bus service with exponential backoff
async fn with_reconnect<F, Fut>(service_name: &str, mut handler: F)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ZbusResult<()>> + Send + 'static,
{
    let mut retry_count = 0;
    let max_delay = Duration::from_secs(60);

    loop {
        match handler().await {
            Ok(()) => {
                // Stream ended normally (shouldn't happen for D-Bus signals)
                log_message(&format!("D-Bus {} stream ended, reconnecting...", service_name));
            }
            Err(e) => {
                log_message(&format!("D-Bus {} disconnected: {}, reconnecting...", service_name, e));
            }
        }

        // Exponential backoff
        let delay = Duration::from_secs(2_u64.saturating_pow(retry_count.min(5)))
            .min(max_delay);

        tokio::time::sleep(delay).await;
        retry_count += 1;

        log_message(&format!("Attempting to reconnect to D-Bus {}...", service_name));
    }
}

pub async fn listen_for_suspend_events(idle_manager: Arc<Mutex<Manager>>, connection: Connection) -> ZbusResult<()> {
    with_reconnect("suspend", move || {
        let manager = Arc::clone(&idle_manager);
        let conn = connection.clone();
        async move {
            let proxy = Proxy::new(
                &conn,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager"
            ).await?;

            let mut stream = proxy.receive_signal("PrepareForSleep").await?;
            log_message("Listening for D-Bus suspend events...");

            while let Some(signal) = stream.next().await {
                let going_to_sleep: bool = match signal.body().deserialize() {
                    Ok(val) => val,
                    Err(e) => {
                        log_message(&format!("Failed to parse D-Bus suspend signal: {e:?}"));
                        continue;
                    }
                };

                let manager_arc = Arc::clone(&manager);
                if going_to_sleep {
                    handle_event(&manager_arc, Event::Suspend).await;
                } else {
                    handle_event(&manager_arc, Event::Wake).await;
                }
            }

            Ok(())
        }
    }).await;
    
    Ok(())
}

pub async fn listen_for_lid_events(idle_manager: Arc<Mutex<Manager>>, connection: Connection) -> ZbusResult<()> {
    with_reconnect("lid", move || {
        let manager = Arc::clone(&idle_manager);
        let conn = connection.clone();
        async move {
            log_message("Listening for D-Bus lid events via UPower...");

            let rule = MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")?
                .member("PropertiesChanged")?
                .path("/org/freedesktop/UPower")?
                .build();

            let mut stream = zbus::MessageStream::for_match_rule(
                rule,
                &conn,
                None,
            ).await?;

            while let Some(msg) = stream.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        log_message(&format!("Error receiving message: {e:?}"));
                        continue;
                    }
                };

                let body = msg.body();
                let (iface, changed, _): (String, HashMap<String, Value>, Vec<String>) =
                    match body.deserialize() {
                        Ok(val) => val,
                        Err(e) => {
                            log_message(&format!("Failed to parse lid signal: {e:?}"));
                            continue;
                        }
                    };

                if iface == "org.freedesktop.UPower" {
                    if let Some(val) = changed.get("LidIsClosed") {
                        match val.downcast_ref::<bool>() {
                            Ok(lid_closed) => {
                                let manager_arc = Arc::clone(&manager);
                                if lid_closed {
                                    handle_event(&manager_arc, Event::LidClosed).await;
                                } else {
                                    handle_event(&manager_arc, Event::LidOpened).await;
                                }
                            }
                            Err(e) => {
                                log_message(&format!("Failed to downcast LidIsClosed value: {e:?}"));
                            }
                        }
                    }
                }
            }

            Ok(())
        }
    }).await;
    
    Ok(())
}

pub async fn listen_for_lock_events(idle_manager: Arc<Mutex<Manager>>, connection: Connection) -> ZbusResult<()> {
    with_reconnect("lock", move || {
        let manager = Arc::clone(&idle_manager);
        let conn = connection.clone();
        async move {
            log_message("Listening for D-Bus lock/unlock events...");

            let session_path = get_current_session_path(&conn).await?;
            log_message(&format!("Monitoring session: {}", session_path.as_str()));

            let proxy = Proxy::new(
                &conn,
                "org.freedesktop.login1",
                session_path.clone(),
                "org.freedesktop.login1.Session"
            ).await?;

            let mut lock_stream = proxy.receive_signal("Lock").await?;
            let manager_for_lock = Arc::clone(&manager);

            let mut unlock_stream = proxy.receive_signal("Unlock").await?;
            let manager_for_unlock = Arc::clone(&manager);

            let lock_task = tokio::spawn(async move {
                while let Some(_signal) = lock_stream.next().await {
                    log_message("Received Lock signal from loginctl");
                    handle_event(&manager_for_lock, Event::LoginctlLock).await;
                }
            });

            let unlock_task = tokio::spawn(async move {
                while let Some(_signal) = unlock_stream.next().await {
                    log_message("Received Unlock signal from loginctl");
                    handle_event(&manager_for_unlock, Event::LoginctlUnlock).await;
                }
            });

            let _ = tokio::try_join!(lock_task, unlock_task);
            Ok(())
        }
    }).await;
    
    Ok(())
}

async fn get_current_session_path(connection: &Connection) -> ZbusResult<zvariant::OwnedObjectPath> {
    let proxy = Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager"
    ).await?;
    
    // Method 1: Try XDG_SESSION_ID environment variable (most reliable for graphical sessions)
    if let Ok(session_id) = std::env::var("XDG_SESSION_ID") {
        log_message(&format!("Attempting to use XDG_SESSION_ID: {}", session_id));
        let result: Result<zvariant::OwnedObjectPath, zbus::Error> = proxy.call("GetSession", &(session_id.as_str(),)).await;
        match result {
            Ok(path) => {
                log_message(&format!("Using session from XDG_SESSION_ID: {}", path.as_str()));
                return Ok(path);
            }
            Err(e) => {
                log_message(&format!("XDG_SESSION_ID lookup failed: {}, trying other methods", e));
            }
        }
    }
    
    // Method 2: Find the active graphical session for current UID
    let uid = unsafe { libc::getuid() };
    log_message(&format!("Looking for sessions with UID: {}", uid));
    
    let sessions: Vec<(String, u32, String, String, zvariant::OwnedObjectPath)> = 
        proxy.call("ListSessions", &()).await?;
    
    // First pass: try to find an active graphical session on seat0
    for (session_id, session_uid, username, seat, path) in &sessions {
        if *session_uid == uid {
            log_message(&format!(
                "Found session '{}' for user '{}' (UID {}) on seat '{}'",
                session_id, username, session_uid, seat
            ));
            
            // Check if this is a graphical session
            if let Ok(session_proxy) = Proxy::new(
                connection,
                "org.freedesktop.login1",
                path.clone(),
                "org.freedesktop.login1.Session"
            ).await {
                if let Ok(session_type) = session_proxy.get_property::<String>("Type").await {
                    log_message(&format!("Session '{}' type: {}", session_id, session_type));
                    
                    // Prefer wayland or x11 sessions on seat0
                    if (session_type == "wayland" || session_type == "x11") && seat == "seat0" {
                        log_message(&format!(
                            "Selected graphical session '{}' (type: {}, seat: {})",
                            session_id, session_type, seat
                        ));
                        return Ok(path.clone());
                    }
                }
            }
        }
    }
    
    // Second pass: just use the first session matching our UID
    for (session_id, session_uid, _username, _seat, path) in sessions {
        if session_uid == uid {
            log_message(&format!("Using first available session '{}' for UID {}", session_id, uid));
            return Ok(path);
        }
    }
    
    // Method 3: Fallback to PID method (least reliable)
    log_message("No session found by UID, trying PID method");
    let pid = std::process::id();
    let result: Result<zvariant::OwnedObjectPath, zbus::Error> = proxy.call("GetSessionByPID", &(pid,)).await;
    match result {
        Ok(path) => {
            log_message(&format!("Using session from PID {}: {}", pid, path.as_str()));
            Ok(path)
        }
        Err(e) => {
            Err(zbus::fdo::Error::Failed(format!(
                "Could not find current session (tried XDG_SESSION_ID, UID match, and PID): {}",
                e
            )))
        }
    }
}

// Combined listener that handles suspend, lid, and lock events
pub async fn listen_for_power_events(idle_manager: Arc<Mutex<Manager>>, connection: Connection) -> ZbusResult<()> {
    let suspend_manager = Arc::clone(&idle_manager);
    let lid_manager = Arc::clone(&idle_manager);
    let lock_manager = Arc::clone(&idle_manager);
    
    let conn_suspend = connection.clone();
    let conn_lid = connection.clone();
    let conn_lock = connection.clone();
    
    let suspend_handle = tokio::spawn(async move {
        if let Err(e) = listen_for_suspend_events(suspend_manager, conn_suspend).await {
            log_message(&format!("Suspend listener error: {e:?}"));
        }
    });
    
    let lid_handle = tokio::spawn(async move {
        if let Err(e) = listen_for_lid_events(lid_manager, conn_lid).await {
            log_message(&format!("Lid listener error: {e:?}"));
        }
    });
    
    let lock_handle = tokio::spawn(async move {
        if let Err(e) = listen_for_lock_events(lock_manager, conn_lock).await {
            log_message(&format!("Lock listener error: {e:?}"));
        }
    });
    
    let _ = tokio::try_join!(suspend_handle, lid_handle, lock_handle);
    Ok(())
}

// Monitor D-Bus for Inhibit calls (ScreenSaver and Portal).
pub async fn spawn_inhibit_monitor(manager: Arc<Mutex<Manager>>, connection: Connection) {
    log_message("Starting D-Bus inhibitor monitor (Spy Mode)...");

    let rules = vec![
        "type='method_call',interface='org.freedesktop.ScreenSaver',member='Inhibit'".to_string(),
        "type='method_call',interface='org.freedesktop.ScreenSaver',member='UnInhibit'".to_string(),
        "type='method_call',interface='org.freedesktop.portal.Inhibit',member='Inhibit'".to_string(),
        "type='method_call',interface='org.gnome.SessionManager',member='Inhibit'".to_string(),
        "type='method_call',interface='org.gnome.SessionManager',member='UnInhibit'".to_string(),
        "type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged'".to_string(),
    ];

    let _ = connection.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus.Monitoring"),
        "BecomeMonitor",
        &(rules, 0u32)
    ).await;

    // Track active senders by unique name to correctly count
    let mut active_senders: HashSet<String> = HashSet::new();
    let mut stream = zbus::MessageStream::from(connection);

    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };
        let header = msg.header();
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        let member = header.member().map(|m| m.to_string()).unwrap_or_default();

        if sender.is_empty() { continue; }

        match header.message_type() {
            zbus::message::Type::MethodCall => {
                if member == "Inhibit" {
                    if active_senders.insert(sender.clone()) {
                        log_message(&format!("External inhibitor detected from: {}", sender));
                        let mut mgr = manager.lock().await;
                        incr_active_inhibitor(&mut mgr).await;
                    }
                } else if member == "UnInhibit" {
                    if active_senders.remove(&sender) {
                        log_message(&format!("External inhibitor cleared from: {}", sender));
                        let mut mgr = manager.lock().await;
                        decr_active_inhibitor(&mut mgr).await;
                    }
                }
            },
            zbus::message::Type::Signal => {
                if member == "NameOwnerChanged" {
                    if let Ok((name, _, new_owner)) = msg.body().deserialize::<(String, String, String)>() {
                        if new_owner.is_empty() && active_senders.remove(&name) {
                            log_message(&format!("External inhibitor removed (app exited): {}", name));
                            let mut mgr = manager.lock().await;
                            decr_active_inhibitor(&mut mgr).await;
                        }
                    }
                }
            },
            _ => {}
        }
    }
}
