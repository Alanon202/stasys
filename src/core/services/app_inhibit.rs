use std::{collections::HashSet, sync::Arc};
use tokio::sync::Mutex;
use tokio::process::Command;
use serde_json::Value;
use procfs::process::all_processes;

use crate::config::model::StasisConfig;
use crate::core::manager::helpers::{decr_active_inhibitor, incr_active_inhibitor};
use crate::log::log_message;
use crate::core::manager::Manager;

/// Tracks currently running apps to inhibit idle
pub struct AppInhibitor {
    cfg: Arc<StasisConfig>,
    active_apps: HashSet<String>,
    desktop: String,
    manager: Arc<Mutex<Manager>>,
}

impl AppInhibitor {
    pub fn new(cfg: Arc<StasisConfig>, manager: Arc<Mutex<Manager>>) -> Self {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();

        log_message(&format!("XDG_CURRENT_DESKTOP detected: {}", desktop));

        Self {
            cfg,
            active_apps: HashSet::new(),
            desktop,
            manager,
        }
    }

    /// Update the config reference when config is reloaded
    pub async fn update_from_config(&mut self, cfg: &StasisConfig) {
        self.cfg = Arc::new(cfg.clone());
    }

    /// Returns true if any app in inhibit_apps is currently running
    pub async fn is_any_app_running(&mut self) -> bool {
        let mut new_active_apps = HashSet::new();

        let running = match self.check_compositor_windows().await {
            Ok(result_apps) => {
                new_active_apps = result_apps;
                !new_active_apps.is_empty()
            },
            Err(_) => self.check_processes_with_tracking(&mut new_active_apps),
        };

        for app in &new_active_apps {
            if !self.active_apps.contains(app) {
                log_message(&format!("App inhibit active: {}", app));
            }
        }

        self.active_apps = new_active_apps;
        running
    }

    /// Process-based fallback - only refresh what we need
    fn check_processes_with_tracking(&mut self, new_active_apps: &mut HashSet<String>) -> bool {
        let mut any_running = false;

        // all_processes() returns Result<ProcessesIter, ProcError>
        let processes_iter = match all_processes() {
            Ok(iter) => iter,
            Err(_) => return false, // unable to read /proc
        };

        for process in processes_iter {
            let process = match process {
                Ok(p) => p,
                Err(_) => continue, // skip processes that failed
            };

            // Fast: just read /proc/[pid]/comm for process name
            let proc_name = match std::fs::read_to_string(format!("/proc/{}/comm", process.pid)) {
                Ok(name) => name.trim().to_string(),
                Err(_) => continue,
            };

            // Compare against inhibit patterns
            for pattern in &self.cfg.inhibit_apps {
                let matched = match pattern {
                    crate::config::model::AppInhibitPattern::Literal(s) => {
                        proc_name.eq_ignore_ascii_case(s)
                    }
                    crate::config::model::AppInhibitPattern::Regex(r) => r.is_match(&proc_name),
                };

                if matched {
                    new_active_apps.insert(proc_name.clone());
                    any_running = true;
                    break;
                }
            }
        }

        any_running
    }    
    
    /// Check compositor windows via IPC
    async fn check_compositor_windows(&self) -> Result<HashSet<String>, Box<dyn std::error::Error + Send + Sync>> {
        match self.desktop.as_str() {
            "niri" => {
                let app_ids = self.try_niri_ipc().await?;
                Ok(app_ids.into_iter()
                    .filter(|app| self.should_inhibit_for_app(app))
                    .collect())
            }
            "hyprland" => {
                let windows = self.try_hyprland_ipc().await?;
                Ok(windows.into_iter()
                    .filter_map(|win| win.get("app_id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    .filter(|app| self.should_inhibit_for_app(app))
                    .collect())
            }
            _ => Err("No IPC available, fallback to process scan".into())
        }
    }

    async fn try_niri_ipc(&self) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let output = Command::new("niri").args(&["msg", "windows"]).output().await?;
        if !output.status.success() {
            return Err(format!("niri command failed: {}", String::from_utf8_lossy(&output.stderr)).into());
        }
        let text = String::from_utf8(output.stdout)?;
        Ok(text.lines()
            .filter_map(|line| line.strip_prefix("  App ID: "))
            .map(|s| s.trim_matches('"').to_string())
            .collect())
    }

    async fn try_hyprland_ipc(&self) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
        let output = Command::new("hyprctl").args(&["clients", "-j"]).output().await?;
        if !output.status.success() {
            return Err(format!("hyprctl command failed: {}", String::from_utf8_lossy(&output.stderr)).into());
        }

        let clients: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        let windows = clients.into_iter().map(|mut client| {
            if let Some(class) = client.get("class").cloned() {
                client.as_object_mut().unwrap().insert("app_id".to_string(), class);
            }
            client
        }).collect();

        Ok(windows)
    }

    fn should_inhibit_for_app(&self, app_id: &str) -> bool {
        for pattern in &self.cfg.inhibit_apps {
            let matched = match pattern {
                crate::config::model::AppInhibitPattern::Literal(s) => self.app_id_matches(s, app_id),
                crate::config::model::AppInhibitPattern::Regex(r) => r.is_match(app_id),
            };
            if matched { return true; }
        }
        false
    }

    fn app_id_matches(&self, pattern: &str, app_id: &str) -> bool {
        if pattern.eq_ignore_ascii_case(app_id) { return true; }
        if app_id.ends_with(".exe") {
            let name = app_id.strip_suffix(".exe").unwrap_or(app_id);
            if pattern.eq_ignore_ascii_case(name) { return true; }
        }
        if let Some(last) = pattern.split('.').last() {
            if last.eq_ignore_ascii_case(app_id) { return true; }
        }
        false
    }

    /// Gracefully stop the inhibitor
    pub async fn shutdown(&mut self) {
        log_message("Shutting down app inhibitor...");
        self.active_apps.clear();
    }
}

pub async fn spawn_app_inhibit_task(
    manager: Arc<Mutex<Manager>>,
    cfg: Arc<StasisConfig>,
) -> Arc<Mutex<AppInhibitor>> {
    let inhibitor = Arc::new(Mutex::new(AppInhibitor::new(cfg.clone(), Arc::clone(&manager))));

    // If no inhibit apps are configured, sleep forever
    if cfg.inhibit_apps.is_empty() {
        log_message("No inhibit_apps configured, sleeping app inhibitor.");
        tokio::spawn(async move {
            futures_util::future::pending::<()>().await;
        });
        return inhibitor;
    }

    let inhibitor_clone = Arc::clone(&inhibitor);

    tokio::spawn(async move {
        let mut inhibitor_active = false; // track previous inhibitor state locally

        loop {
            let running = {
                let mut guard = inhibitor_clone.lock().await;
                guard.is_any_app_running().await
            };

            if running && !inhibitor_active {
                // App started inhibiting
                let guard = inhibitor_clone.lock().await;
                let mut mgr = guard.manager.lock().await;
                incr_active_inhibitor(&mut mgr).await;
                inhibitor_active = true;
            } else if !running && inhibitor_active {
                // All apps stopped inhibiting
                let guard = inhibitor_clone.lock().await;
                let mut mgr = guard.manager.lock().await;
                decr_active_inhibitor(&mut mgr).await;
                inhibitor_active = false;
            }

            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        }
    });

    inhibitor
}

