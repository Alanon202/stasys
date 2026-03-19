use std::{fs, sync::Arc, time::Duration};
use tokio::sync::Mutex;

use crate::core::manager::Manager;
use crate::log::log_message;
use crate::core::events::handlers::{handle_event, Event};

/// Detect initial AC/Battery state.
/// Returns true if on AC power.
pub async fn detect_initial_power_state(manager: &Arc<Mutex<Manager>>) -> bool {
    let mgr = manager.lock().await;
    if !mgr.state.is_laptop() {
        log_message("Desktop detected, skipping power source check");
        return true;
    }
    drop(mgr);

    let on_ac = is_on_ac_power().await;

    {
        let mut mgr = manager.lock().await;
        mgr.state.set_on_battery(!on_ac);
    }

    let current_block = manager.lock().await.state.current_block.clone();
    log_message(&format!(
        "Initial power detection: {} (active block: {})",
        if on_ac { "AC" } else { "Battery" },
        current_block
    ));
    on_ac
}

/// Check if the device is currently powered by AC.
async fn is_on_ac_power() -> bool {
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply/") {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();

            if let Ok(supply_type) = fs::read_to_string(path.join("type")) {
                if supply_type.trim() == "Mains" {
                    if let Ok(status) = fs::read_to_string(path.join("online")) {
                        if status.trim() == "1" {
                            return true;
                        }
                    }
                }
            }

            // Fallback for legacy names
            let legacy_ac_names = ["AC", "ADP", "ACAD", "AC0", "ADP0"];
            if legacy_ac_names.iter().any(|n| name.starts_with(n)) {
                if let Ok(status) = fs::read_to_string(path.join("online")) {
                    if status.trim() == "1" {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Background monitor for power source changes.
/// Emits ACConnected / ACDisconnected events through the central event system.
pub async fn spawn_power_source_monitor(manager: Arc<Mutex<Manager>>) {
    let mgr = manager.lock().await;
    let last_on_ac = !mgr.state.on_battery().unwrap_or(false);
    drop(mgr);

    let mut last_on_ac = last_on_ac;
    // Poll every 1 second for responsive AC plug/unplug detection
    // Lower values = more responsive but slightly higher CPU usage
    let mut ticker = tokio::time::interval(Duration::from_secs(1));

    loop {
        ticker.tick().await;

        let mgr = manager.lock().await;
        if !mgr.state.is_laptop() {
            continue;
        }
        drop(mgr);

        let on_ac = is_on_ac_power().await;

        if on_ac != last_on_ac {
            last_on_ac = on_ac;
            log_message(&format!(
                "Power source changed: {}",
                if on_ac { "AC" } else { "Battery" }
            ));

            // Emit event instead of mutating state directly
            if on_ac {
                handle_event(&manager, Event::ACConnected).await;
            } else {
                handle_event(&manager, Event::ACDisconnected).await;
            }
        }
    }
}
