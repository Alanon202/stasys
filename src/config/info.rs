use std::{collections::BTreeSet, time::Duration};
use crate::{config::model::StasisConfig, core::utils};

impl StasisConfig {
    pub fn pretty_print(
        &self,
        idle_time: Option<Duration>,
        uptime: Option<Duration>,
        is_paused: Option<bool>,
        is_manually_paused: Option<bool>,
        app_blocking: Option<bool>,
        media_blocking: Option<bool>,
        active_profile: Option<&str>,
    ) -> String {
        let mut out = String::new();

        // Calculate the global pipe position
        // Find the longest label across all sections
        let status_labels = vec!["Active Profile", "Idle Time", "Uptime", "Paused", "Manually Paused", "App Blocking", "Media Blocking"];
        let config_labels = vec!["PreSuspendCommand", "MonitorMedia", "IgnoreRemoteMedia",
                                 "RespectInhibitors", "NotifyOnUnpause", "DebounceSeconds",
                                 "LidCloseAction", "LidOpenAction", "InhibitApps"];
        let action_labels = vec!["Timeout", "Kind", "Command", "LockCommand", "ResumeCommand"];

        let max_label = status_labels.iter()
            .chain(config_labels.iter())
            .chain(action_labels.iter())
            .map(|s| s.len())
            .max()
            .unwrap_or(0);

        // Status section
        out.push_str("◆ STATUS\n");

        // Show active profile
        let profile_display = active_profile.unwrap_or("base config");
        out.push_str(&format!("  {:<width$} │ {}\n", "Active Profile", profile_display, width = max_label));

        if let Some(idle) = idle_time {
            out.push_str(&format!("  {:<width$} │ {}\n", "Idle Time", utils::format_duration(idle), width = max_label));
        }
        if let Some(up) = uptime {
            out.push_str(&format!("  {:<width$} │ {}\n", "Uptime", utils::format_duration(up), width = max_label));
        }
        if let Some(paused) = is_paused {
            let indicator = if paused { "●" } else { "○" };
            out.push_str(&format!("  {:<width$} │ {} {}\n", "Paused", indicator, paused, width = max_label));
        }
        if let Some(manually_paused) = is_manually_paused {
            let indicator = if manually_paused { "●" } else { "○" };
            out.push_str(&format!("  {:<width$} │ {} {}\n", "Manually Paused", indicator, manually_paused, width = max_label));
        }
        if let Some(app_paused) = app_blocking {
            let indicator = if app_paused { "●" } else { "○" };
            out.push_str(&format!("  {:<width$} │ {} {}\n", "App Blocking", indicator, app_paused, width = max_label));
        }
        if let Some(media_paused) = media_blocking {
            let indicator = if media_paused { "●" } else { "○" };
            out.push_str(&format!("  {:<width$} │ {} {}\n", "Media Blocking", indicator, media_paused, width = max_label));
        }
        
        out.push('\n');
        
        // Config section
        out.push_str("◆ CONFIGURATION\n");
        out.push_str(&format!(
            "  {:<width$} │ {}\n",
            "PreSuspendCommand",
            self.pre_suspend_command.as_deref().unwrap_or("none"),
            width = max_label
        ));
        out.push_str(&format!(
            "  {:<width$} │ {}\n",
            "MonitorMedia",
            if self.monitor_media { "✓ enabled" } else { "✗ disabled" },
            width = max_label
        ));
        out.push_str(&format!(
            "  {:<width$} │ {}\n", 
            "IgnoreRemoteMedia",
            if self.ignore_remote_media { "✓ enabled" } else { "✗ disabled" },
            width = max_label
        ));
        out.push_str(&format!(
            "  {:<width$} │ {}\n",
            "RespectInhibitors",
            if self.respect_wayland_inhibitors { "✓ enabled" } else { "✗ disabled" },
            width = max_label
        ));
        out.push_str(&format!(
            "  {:<width$} │ {}\n",
            "NotifyOnUnpause",
            if self.notify_on_unpause { "✓ enabled" } else { "✗ disabled" },
            width = max_label
        ));
        out.push_str(&format!("  {:<width$} │ {}s\n", "DebounceSeconds", self.debounce_seconds, width = max_label));
        out.push_str(&format!("  {:<width$} │ {}\n", "LidCloseAction", self.lid_close_action, width = max_label));
        out.push_str(&format!("  {:<width$} │ {}\n", "LidOpenAction", self.lid_open_action, width = max_label));
        
        let apps = if self.inhibit_apps.is_empty() {
            "none".to_string()
        } else {
            self.inhibit_apps
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!("  {:<width$} │ {}\n", "InhibitApps", apps, width = max_label));
        
        out.push('\n');
        
        // Actions section
        out.push_str("◆ ACTIONS\n");
        
        let mut seen_groups = BTreeSet::new();
        let mut action_counter = 1;
        
        for action in &self.actions {
            let group = if action.name.starts_with("ac.") {
                "AC Power"
            } else if action.name.starts_with("battery.") {
                "Battery Power"
            } else {
                "Desktop"
            };
            
            if seen_groups.insert(group) {
                out.push_str(&format!("  [{group}]\n"));
            }
            
            let display_name = action.name
                .strip_prefix("ac.")
                .or_else(|| action.name.strip_prefix("battery."))
                .unwrap_or(&action.name);
            
            // Action header - number outside, name only
            out.push_str(&format!(
                "  {}. {}\n",
                action_counter,
                display_name
            ));
            
            // Action details - indented to align pipes
            out.push_str(&format!("     {:<width$} │ {}s\n", "Timeout", action.timeout, width = max_label - 3));
            out.push_str(&format!("     {:<width$} │ {}\n", "Kind", action.kind, width = max_label - 3));
            out.push_str(&format!("     {:<width$} │ {}\n", "Command", action.command, width = max_label - 3));
            
            if let Some(lock_cmd) = &action.lock_command {
                out.push_str(&format!("     {:<width$} │ {}\n", "LockCommand", lock_cmd, width = max_label - 3));
            }
            if let Some(resume_cmd) = &action.resume_command {
                out.push_str(&format!("     {:<width$} │ {}\n", "ResumeCommand", resume_cmd, width = max_label - 3));
            }
            
            action_counter += 1;
        }
        
        out
    }
}
