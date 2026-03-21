use std::{
    sync::Arc,
    time::Duration,
};
use tokio::time::sleep;
use tokio::sync::Mutex;
use time::{OffsetDateTime, Time};

use crate::{
    core::manager::Manager,
    log::log_message,
};

pub const PAUSE_HELP_MESSAGE: &str = r#"Pause all timers indefinitely or for a specific duration/time

Usage:
  stasys pause                  Pause indefinitely until 'resume' is called
  stasys pause for <DURATION>   Pause for a specific duration, then auto-resume
  stasys pause until <TIME>     Pause until a specific time, then auto-resume

Duration format:
  You can specify durations using combinations of:
    - s, sec, seconds (e.g., 30s)
    - m, min, minutes (e.g., 5m)
    - h, hr, hours    (e.g., 2h)

Time format (12-hour):
  - 1:30pm, 1:30 pm, 1:30PM
  - 130pm, 130 pm
  - 1pm, 1 pm

Time format (24-hour):
  - 13:30, 13:00
  - 1330, 1300
  - 13

Examples:
  stasys pause                  Pause indefinitely
  stasys pause for 5m           Pause for 5 minutes
  stasys pause for 1h 30m       Pause for 1 hour and 30 minutes
  stasys pause for 2h 15m 30s   Pause for 2 hours, 15 minutes, and 30 seconds
  stasys pause until 1:30pm     Pause until 1:30 PM today
  stasys pause until 130pm      Pause until 1:30 PM today
  stasys pause until 13:30      Pause until 13:30 (1:30 PM) today
  stasys pause until 1330       Pause until 13:30 (1:30 PM) today

Use 'stasys resume' to manually resume before the timer expires."#;

/// Parse a duration string like "5m", "1h", "30s", or "1h 30m 15s"
fn parse_duration(s: &str) -> Result<Duration, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("Duration must be greater than 0".to_string());
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let mut total_secs = 0u64;

    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        // Find where the number ends and unit begins
        let split_pos = part
            .chars()
            .position(|c| !c.is_ascii_digit())
            .ok_or_else(|| format!("Invalid duration format: '{}' (missing unit)", part))?;

        let (num_str, unit) = part.split_at(split_pos);
        let num: u64 = num_str
            .parse()
            .map_err(|_| format!("Invalid number: '{}'", num_str))?;

        let multiplier = match unit.to_lowercase().as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            _ => return Err(format!("Unknown time unit: '{}' (use s, m, or h)", unit)),
        };

        total_secs += num * multiplier;
    }

    if total_secs == 0 {
        return Err("Duration must be greater than 0".to_string());
    }

    Ok(Duration::from_secs(total_secs))
}

/// Parse a time string and return duration until that time
/// Supports formats like: 1:30pm, 130pm, 1pm, 13:30, 1330, 13
fn parse_time_until(s: &str) -> Result<Duration, String> {
    let s = s.trim().to_lowercase().replace(" ", "");
    
    // Check for AM/PM
    let (time_str, is_pm) = if s.ends_with("pm") {
        (s.trim_end_matches("pm"), true)
    } else if s.ends_with("am") {
        (s.trim_end_matches("am"), false)
    } else {
        (s.as_str(), false) // Assume 24-hour format
    };

    // Parse the time
    let (hour, minute) = if time_str.contains(':') {
        // Format: 1:30, 13:30
        let parts: Vec<&str> = time_str.split(':').collect();
        if parts.len() != 2 {
            return Err(format!("Invalid time format: '{}' (use HH:MM or H:MM)", s));
        }
        let h: u32 = parts[0].parse()
            .map_err(|_| format!("Invalid hour: '{}'", parts[0]))?;
        let m: u32 = parts[1].parse()
            .map_err(|_| format!("Invalid minute: '{}'", parts[1]))?;
        (h, m)
    } else {
        // Format: 130, 1330, 1, 13
        let num: u32 = time_str.parse()
            .map_err(|_| format!("Invalid time format: '{}'", s))?;
        
        if num >= 100 {
            // 130 -> 1:30, 1330 -> 13:30
            let h = num / 100;
            let m = num % 100;
            (h, m)
        } else {
            // 1 -> 1:00, 13 -> 13:00
            (num, 0)
        }
    };

    // Convert to 24-hour format if PM
    let hour_24 = if is_pm && hour < 12 {
        hour + 12
    } else if !is_pm && hour == 12 {
        0 // 12am = 00:00
    } else {
        hour
    };

    // Validate
    if hour_24 >= 24 {
        return Err(format!("Invalid hour: {} (must be 0-23)", hour_24));
    }
    if minute >= 60 {
        return Err(format!("Invalid minute: {} (must be 0-59)", minute));
    }

    // Get target time
    let target_time = Time::from_hms(hour_24 as u8, minute as u8, 0)
        .map_err(|_| format!("Invalid time: {}:{:02}", hour_24, minute))?;

    // Get current time
    let now = OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    let current_time = Time::from_hms(now.hour(), now.minute(), now.second()).unwrap();

    // Calculate duration until target time
    let duration = if target_time > current_time {
        // Target is later today
        let target_secs = target_time.hour() as u64 * 3600 + target_time.minute() as u64 * 60 + target_time.second() as u64;
        let current_secs = current_time.hour() as u64 * 3600 + current_time.minute() as u64 * 60 + current_time.second() as u64;
        Duration::from_secs(target_secs - current_secs)
    } else {
        // Target is tomorrow (already passed today)
        let target_secs = target_time.hour() as u64 * 3600 + target_time.minute() as u64 * 60 + target_time.second() as u64;
        let current_secs = current_time.hour() as u64 * 3600 + current_time.minute() as u64 * 60 + current_time.second() as u64;
        let secs_until_midnight = 86400 - current_secs;
        Duration::from_secs(secs_until_midnight + target_secs)
    };

    Ok(duration)
}

/// Format a duration into a human-readable string
fn format_duration_readable(duration: Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let remaining_secs = secs % 60;

    if hours > 0 && mins > 0 && remaining_secs > 0 {
        format!("{}h {}m {}s", hours, mins, remaining_secs)
    } else if hours > 0 && mins > 0 {
        format!("{}h {}m", hours, mins)
    } else if hours > 0 && remaining_secs > 0 {
        format!("{}h {}s", hours, remaining_secs)
    } else if mins > 0 && remaining_secs > 0 {
        format!("{}m {}s", mins, remaining_secs)
    } else if hours > 0 {
        format!("{}h", hours)
    } else if mins > 0 {
        format!("{}m", mins)
    } else {
        format!("{}s", remaining_secs)
    }
}

/// Pause the manager for a specific duration, then automatically resume
async fn pause_for_duration(
    manager: Arc<Mutex<Manager>>,
    duration: Duration,
    reason: String,
    notification_msg: String,
) -> Result<String, String> {
    // Pause immediately
    {
        let mut mgr = manager.lock().await;
        mgr.pause(true).await;
    }

    log_message(&format!("Idle manager paused {}", reason));

    // Clone for the spawned task
    let reason_clone = reason.clone();
    let notification_msg_clone = notification_msg.clone();
    
    // Spawn a task to auto-resume after duration
    tokio::spawn(async move {
        sleep(duration).await;
        
        let mut mgr = manager.lock().await;
        
        // Only clear the manual pause flag
        if mgr.state.manually_paused {
            mgr.state.manually_paused = false;
            
            // Check if we should actually unpause based on inhibitor count
            let should_notify = if let Some(cfg) = &mgr.state.cfg {
                cfg.notify_on_unpause
            } else {
                false
            };

            if mgr.state.active_inhibitor_count == 0 {
                mgr.state.paused = false;
                log_message(&format!("Auto-resuming after {}", reason_clone));
           
                // Send notification - manual pause lifted and fully resumed
                if should_notify {
                    send_notification(
                        "Stasis resumed",
                        &notification_msg_clone
                    ).await;
                }
            } else {
                log_message(&format!(
                    "Auto-resume timer expired after {} but {} inhibitor(s) still active - timers remain paused",
                    reason_clone,
                    mgr.state.active_inhibitor_count
                ));
                
                // Send notification - manual pause lifted but still inhibited
                if should_notify {
                    let inhibitor_word = if mgr.state.active_inhibitor_count == 1 {
                        "inhibitor"
                    } else {
                        "inhibitors"
                    };
                    send_notification(
                        "Stasis - manual pause expired",
                        &format!(
                            "Manual pause timer expired, but {} {} still active. Timers remain paused.",
                            mgr.state.active_inhibitor_count,
                            inhibitor_word
                        )
                    ).await;
                }
            }
        }
    });

    Ok(format!("Paused {}", reason))
}

/// Handle pause command with various formats
pub async fn handle_pause_command(
    manager: Arc<Mutex<Manager>>,
    args: &str,
) -> Result<String, String> {
    let args = args.trim();
    
    // Check for help
    if args.eq_ignore_ascii_case("help") || args == "-h" || args == "--help" {
        return Err(PAUSE_HELP_MESSAGE.to_string());
    }

    // No args = indefinite pause
    if args.is_empty() {
        let mut mgr = manager.lock().await;
        mgr.pause(true).await;
        return Ok("Idle manager paused indefinitely".to_string());
    }

    // Parse "for" or "until"
    if let Some(duration_str) = args.strip_prefix("for ").or_else(|| args.strip_prefix("for")) {
        let duration_str = duration_str.trim();
        if duration_str.is_empty() {
            return Err("Missing duration after 'for' (e.g., 'pause for 5m')".to_string());
        }
        
        let duration = parse_duration(duration_str)?;
        let notification = format!("Timers resumed after {} pause", format_duration_readable(duration));
        pause_for_duration(manager, duration, format!("for {}", format_duration_readable(duration)), notification).await
    } else if let Some(time_str) = args.strip_prefix("until ").or_else(|| args.strip_prefix("until")) {
        let time_str = time_str.trim();
        if time_str.is_empty() {
            return Err("Missing time after 'until' (e.g., 'pause until 1:30pm')".to_string());
        }
        
        let duration = parse_time_until(time_str)?;
        let formatted_time = format_duration_readable(duration);
        
        // Format the target time for display
        let display_time = format!("until {} (in {})", time_str, formatted_time);
        let notification = format!("Timers resumed (paused until {})", time_str);
        pause_for_duration(manager, duration, display_time, notification).await
    } else {
        // Legacy support: try parsing as duration without "for"
        match parse_duration(args) {
            Ok(duration) => {
                let notification = format!("Timers resumed after {} pause", format_duration_readable(duration));
                pause_for_duration(manager, duration, format!("for {}", format_duration_readable(duration)), notification).await
            }
            Err(_) => {
                Err(format!(
                    "Invalid pause format. Use:\n  \
                    'pause' (indefinite)\n  \
                    'pause for <duration>' (e.g., 'pause for 5m')\n  \
                    'pause until <time>' (e.g., 'pause until 1:30pm')\n\n\
                    For more help: 'stasis pause help'"
                ))
            }
        }
    }
}

async fn send_notification(summary: &str, body: &str) {
    if let Err(e) = tokio::process::Command::new("notify-send")
        .arg("-a")
        .arg("stasis")
        .arg(summary)
        .arg(body)
        .spawn()
    {
        log_message(&format!("Failed to send notification: {}", e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("1h 30m 15s").unwrap(), Duration::from_secs(5415));
        assert_eq!(parse_duration("2h 15s").unwrap(), Duration::from_secs(7215));
        
        // Test various unit formats
        assert_eq!(parse_duration("5mins").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1hour").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("30seconds").unwrap(), Duration::from_secs(30));
        
        // Test errors
        assert!(parse_duration("").is_err());
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("0m").is_err());
    }

    #[test]
    fn test_parse_time_until() {
        // Note: These tests will vary based on current time
        // Just verify they parse without errors and return reasonable durations
        
        // 12-hour formats
        assert!(parse_time_until("1:30pm").is_ok());
        assert!(parse_time_until("1:30 pm").is_ok());
        assert!(parse_time_until("130pm").is_ok());
        assert!(parse_time_until("1pm").is_ok());
        
        // 24-hour formats
        assert!(parse_time_until("13:30").is_ok());
        assert!(parse_time_until("1330").is_ok());
        assert!(parse_time_until("13").is_ok());
        
        // Edge cases
        assert!(parse_time_until("12am").is_ok()); // Midnight
        assert!(parse_time_until("12pm").is_ok()); // Noon
        
        // Errors
        assert!(parse_time_until("25:00").is_err()); // Invalid hour
        assert!(parse_time_until("12:60").is_err()); // Invalid minute
        assert!(parse_time_until("abc").is_err()); // Invalid format
    }

    #[test]
    fn test_format_duration_readable() {
        assert_eq!(format_duration_readable(Duration::from_secs(300)), "5m");
        assert_eq!(format_duration_readable(Duration::from_secs(3600)), "1h");
        assert_eq!(format_duration_readable(Duration::from_secs(5400)), "1h 30m");
        assert_eq!(format_duration_readable(Duration::from_secs(5415)), "1h 30m 15s");
        assert_eq!(format_duration_readable(Duration::from_secs(30)), "30s");
    }
}
