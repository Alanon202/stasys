use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};
use serde_json::Value;

use crate::core::manager::{
    helpers::{incr_active_inhibitor, decr_active_inhibitor},
    Manager
};
use crate::log::{log_message, log_error_message};

const BRIDGE_SOCKET: &str = "/tmp/mpris_bridge.sock";
const POLL_INTERVAL_MS: u64 = 1000; // Poll every 1 second

/// Query the browser media state from the Python bridge
async fn query_browser_status() -> Result<BrowserMediaState, String> {
    let mut stream = UnixStream::connect(BRIDGE_SOCKET).await
        .map_err(|e| format!("Failed to connect: {}", e))?;
    
    stream.write_all(b"status").await
        .map_err(|e| format!("Failed to send: {}", e))?;
    
    let mut buffer = vec![0u8; 4096];
    let size = stream.read(&mut buffer).await
        .map_err(|e| format!("Failed to read: {}", e))?;
    
    if size == 0 {
        return Err("Empty response".to_string());
    }
    
    let resp_str = String::from_utf8_lossy(&buffer[..size]);
    let json: Value = serde_json::from_str(&resp_str)
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;
    
    Ok(BrowserMediaState {
        playing: json["playing"].as_bool().unwrap_or(false),
        tab_count: json["tab_count"].as_u64().unwrap_or(0) as usize,
        playing_tabs: json["playing_tabs"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_i64()).map(|i| i as i32).collect())
            .unwrap_or_default(),
    })
}

#[derive(Debug, Clone)]
struct BrowserMediaState {
    playing: bool,
    tab_count: usize,
    playing_tabs: Vec<i32>,
}

/// Spawn a background task that polls the browser media bridge
pub async fn spawn_browser_media_monitor(manager: Arc<Mutex<Manager>>) {
    tokio::spawn(async move {
        let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
        let mut last_state: Option<BrowserMediaState> = None;
        let mut connected = false;
        
        log_message("Browser media monitor started");
        
        loop {
            poll_interval.tick().await;
            
            match query_browser_status().await {
                Ok(state) => {
                    if !connected {
                        log_message("Connected to MPRIS bridge");
                        connected = true;
                    }
                    
                    // Check if state changed 
                    let state_changed = last_state.as_ref().map(|last| {
                        last.playing != state.playing ||
                        last.tab_count != state.tab_count ||
                        last.playing_tabs != state.playing_tabs
                    }).unwrap_or(true);

                    
                    if state_changed {
                        handle_browser_media_state(manager.clone(), &state).await;
                        
                        if state.playing {
                            log_message(&format!(
                                "Browser media active: {}/{} tabs playing (IDs: {:?})",
                                state.playing_tabs.len(),
                                state.tab_count,
                                state.playing_tabs
                            ));
                        } else if state.tab_count > 0 {
                            log_message(&format!(
                                "Browser media stopped ({} tabs with media, none playing)",
                                state.tab_count
                            ));
                        } else if last_state.is_some() {
                            log_message("Browser media stopped (no tabs with media)");
                        }
                    }
                    
                    last_state = Some(state);
                }
                Err(_e) => {
                    if connected {
                        log_error_message("Lost connection to Firefox MPRIS bridge");
                        connected = false;
                        
                        // Treat as "no media playing"
                        let empty_state = BrowserMediaState {
                            playing: false,
                            tab_count: 0,
                            playing_tabs: vec![],
                        };
                        handle_browser_media_state(manager.clone(), &empty_state).await;
                        last_state = None;
                    }
                }
            }
        }
    });
}

/// Handle state changes from the browser
async fn handle_browser_media_state(
    manager: Arc<Mutex<Manager>>,
    state: &BrowserMediaState,
) {
    let mut mgr = manager.lock().await;

    // Track number of tabs playing
    let prev_tab_count = mgr.state.browser_playing_tab_count;
    let new_tab_count = state.playing_tabs.len();

    mgr.state.browser_playing_tab_count = new_tab_count;

    // If new tabs started playing, increment inhibitor per new tab
    if new_tab_count > prev_tab_count {
        let diff = new_tab_count - prev_tab_count;
        for _ in 0..diff {
            incr_active_inhibitor(&mut mgr).await;
        }
        mgr.state.media_playing = true;
        mgr.state.media_blocking = true;
    }
    // If tabs stopped playing, decrement inhibitor per stopped tab
    else if new_tab_count < prev_tab_count {
        let diff = prev_tab_count - new_tab_count;
        for _ in 0..diff {
            decr_active_inhibitor(&mut mgr).await;
        }
        if new_tab_count == 0 {
            mgr.state.media_playing = false;
            mgr.state.media_blocking = false;
        }
    }

    // Update browser media playing flag
    mgr.state.browser_media_playing = new_tab_count > 0;
}
