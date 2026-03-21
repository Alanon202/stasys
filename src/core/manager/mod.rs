pub mod actions;
pub mod helpers;
pub mod idle_loops;
pub mod state;
pub mod tasks;

use std::{sync::Arc, time::{Duration, Instant}};
use tokio::{
    task::JoinHandle, 
    time::sleep
};
use zbus::Connection;

pub use self::state::ManagerState;
use crate::{
    config::model::{IdleAction, StasisConfig}, 
    core::manager::{
        actions::{is_process_running, run_command_detached},
        helpers::{restore_brightness, run_action},
    }, 
    log::log_message
};

pub struct Manager {
    pub state: ManagerState,
    pub spawned_tasks: Vec<JoinHandle<()>>,
    pub idle_task_handle: Option<JoinHandle<()>>,
    pub lock_task_handle: Option<JoinHandle<()>>,
    pub media_task_handle: Option<JoinHandle<()>>,
    pub input_task_handle: Option<JoinHandle<()>>,
}

impl Manager {
    pub fn new(cfg: Arc<StasisConfig>) -> Self {
        Self {
            state: ManagerState::new(cfg),
            spawned_tasks: Vec::new(),
            idle_task_handle: None,
            lock_task_handle: None,
            media_task_handle: None,
            input_task_handle: None,
        }
    }

    pub async fn trigger_instant_actions(&mut self) {
        if self.state.instants_triggered {
            return;
        }

        let instant_actions = self.state.get_active_instant_actions();
        let instant_action_names: Vec<String> = instant_actions.iter().map(|a| a.name.clone()).collect();

        log_message("Triggering instant actions at startup...");
        for action in instant_actions {
            run_action(self, &action).await;
        }

        let now = Instant::now();
        for actions in [&mut self.state.default_actions, &mut self.state.ac_actions, &mut self.state.battery_actions] {
            for action in actions.iter_mut() {
                if instant_action_names.contains(&action.name) {
                    action.last_triggered = Some(now);
                }
            }
        }

        self.state.instants_triggered = true;
    }

    pub fn reset_instant_actions(&mut self) {
        self.state.instants_triggered = false;
        log_message("Instant actions reset; they can trigger again");
    }

    // Called when libinput service resets (on user activity)
    pub async fn reset(&mut self) {
        let cfg = match &self.state.cfg {
            Some(cfg) => Arc::clone(cfg),
            None => {
                log_message("No configuration available, skipping reset");
                return;
            }
        };

        // Restore brightness if needed
        if self.state.previous_brightness.is_some() {
            if let Err(e) = restore_brightness(&mut self.state).await {
                log_message(&format!("Failed to restore brightness: {}", e));
            }
        }

        let now = Instant::now();
        let debounce = Duration::from_secs(cfg.debounce_seconds as u64);
        self.state.debounce = Some(now + debounce);
        self.state.last_activity = now;

        // Store values we need before borrowing
        let is_locked = self.state.lock_state.is_locked;
        let cmd_to_check = self.state.lock_state.command.clone();

        for actions in [&mut self.state.default_actions, &mut self.state.ac_actions, &mut self.state.battery_actions] {
            let mut past_lock = false;
            for a in actions.iter_mut() {
                if matches!(a.kind, crate::config::model::IdleAction::LockScreen) {
                    past_lock = true;
                }
                if is_locked && past_lock {
                    continue;
                }
                a.last_triggered = None;
            }
        }

        let (is_instant, lock_index) = {
            let actions = self.state.get_active_actions_mut();

            let index = actions.iter()
                .position(|a| a.last_triggered.is_none())
                .unwrap_or(actions.len().saturating_sub(1));

            let is_instant = !actions.is_empty() && actions[index].is_instant();

            let lock_index = if is_locked {
                actions.iter().position(|a| matches!(a.kind, crate::config::model::IdleAction::LockScreen))
            } else {
                None
            };

            (is_instant, lock_index)
        }; // Borrow ends here

        // Reset action_index to start of action list (but preserve last_triggered timestamps)
        if !is_locked {
            self.state.action_index = 0;
        }

        if is_instant {
            return;
        }

        if is_locked {
            if let Some(lock_index) = lock_index {
                // Check if lock process is still running
                let still_active = if let Some(cmd) = cmd_to_check {
                    is_process_running(&cmd).await
                } else {
                    true // Assume lock is active if no command is specified
                };

                if still_active {
                    // Always advance to one past lock when locked
                    self.state.action_index = lock_index.saturating_add(1);
                    
                    let debounce_end = now + debounce;
                    let new_action_index = self.state.action_index;
                    let actions = self.state.get_active_actions_mut();
                    if new_action_index < actions.len() {
                        actions[new_action_index].last_triggered = Some(debounce_end); 
                    } else {
                        // If at the end, reset last_triggered for the last action
                        if lock_index < actions.len() {
                            actions[lock_index].last_triggered = Some(debounce_end);
                        } 
                    }
                    
                    self.state.lock_state.post_advanced = true;
                } 
            } 
        }
        
        self.fire_resume_queue().await;
        self.state.notify.notify_one();

        // Trim memory after reset to reclaim heap space immediately
        crate::log::trim_memory();
    }

    // Check whether we have been idle enough to elapse one of the timeouts
    pub async fn check_timeouts(&mut self) {
        if self.state.paused || self.state.manually_paused {
            return;
        }

        let now = Instant::now();

        // Store values we need before borrowing actions
        let action_index = self.state.action_index;
        let is_locked = self.state.lock_state.is_locked;
        let last_activity = self.state.last_activity;
        let debounce = self.state.debounce;

        // Get reference to the right actions Vec using helper method
        let actions = self.state.get_active_actions_mut();

        if actions.is_empty() {
            return;
        }

        let index = action_index.min(actions.len() - 1);

        // Skip lock if already locked
        if matches!(actions[index].kind, IdleAction::LockScreen) && is_locked {
            return;
        }

        // Skip instant actions - only triggered by power state changes
        if actions[index].is_instant() {
            self.state.action_index += 1;
            return;
        }

        let timeout = Duration::from_secs(actions[index].timeout as u64);
        let next_fire = if let Some(last_trig) = actions[index].last_triggered {
            last_trig + timeout
        } else if index > 0 {
            if let Some(prev_trig) = actions[index - 1].last_triggered {
                prev_trig + timeout
            } else {
                last_activity + timeout
            }
        } else {
            // First action: apply debounce + timeout from last_activity
            let base = debounce.unwrap_or(last_activity);
            base + timeout
        };

        if now < next_fire {
            // Not ready yet
            return;
        }

        // Action is ready: clone and mark triggered
        let (action_clone, actions_len) = {
            let actions = self.state.get_active_actions_mut();
            let action_clone = actions[index].clone();
            actions[index].last_triggered = Some(now);
            (action_clone, actions.len())
        }; // Borrow ends here

        // Advance index
        self.state.action_index += 1;
        if self.state.action_index < actions_len {
            // Only mark next action triggered after it actually fires
            self.state.resume_commands_fired = false;
        } else {
            self.state.action_index = actions_len - 1;
        }

        // Add to resume queue if needed
        if !matches!(action_clone.kind, IdleAction::LockScreen) && action_clone.resume_command.is_some() {
            self.state.resume_queue.push(action_clone.clone());
        }

        // Fire the action
        run_action(self, &action_clone).await;
    }

    pub async fn fire_resume_queue(&mut self) {
        if self.state.resume_queue.is_empty() {
            return;
        }

        log_message(&format!("Firing {} queued resume command(s)...", self.state.resume_queue.len()));

        for action in self.state.resume_queue.drain(..) {
            if let Some(resume_cmd) = &action.resume_command {
                log_message(&format!("Running resume command for action: {}", action.name));
                if let Err(e) = run_command_detached(resume_cmd).await {
                    log_message(&format!("Failed to run resume command '{}': {}", resume_cmd, e));
                }
            }
        }

        self.state.resume_queue.clear();
    }


    pub fn next_action_instant(&self) -> Option<Instant> {
        if self.state.paused || self.state.manually_paused {
            return None;
        }

        // Use helper method to get active actions
        let actions = self.state.get_active_actions();

        if actions.is_empty() {
            return None;
        }

        let mut min_time: Option<Instant> = None;

        for (i, action) in actions.iter().enumerate() {
            // Skip lock if already locked
            if matches!(action.kind, IdleAction::LockScreen) && self.state.lock_state.is_locked {
                continue;
            }

            // Calculate next fire time for this action
            let timeout = Duration::from_secs(action.timeout as u64);
            let next_time = if let Some(last_trig) = action.last_triggered {
                // Already triggered: timeout from when it last fired
                last_trig + timeout
            } else if i > 0 {
                // Not first action: fire relative to previous action
                if let Some(prev_trig) = actions[i - 1].last_triggered {
                    prev_trig + timeout
                } else {
                    // Previous hasn't fired yet, shouldn't happen but fallback
                    self.state.last_activity + timeout
                }
            } else {
                // First action: use debounce + timeout
                let base = self.state.debounce.unwrap_or(self.state.last_activity);
                base + timeout
            };

            min_time = Some(match min_time {
                None => next_time,
                Some(current_min) => current_min.min(next_time),
            });
        }

        min_time
    }



    pub async fn advance_past_lock(&mut self) {
        log_message("Advancing state past lock stage...");
        
        let now = Instant::now();
        self.state.lock_state.post_advanced = true;
        self.state.lock_state.last_advanced = Some(now);
        
        // Get debounce from config
        let debounce = if let Some(cfg) = &self.state.cfg {
            Duration::from_secs(cfg.debounce_seconds as u64)
        } else {
            Duration::from_secs(5) // fallback
        };
        
        // Reset timing state
        self.state.last_activity = now;
        self.state.debounce = Some(now + debounce);

        for actions in [
            &mut self.state.default_actions,
            &mut self.state.ac_actions,
            &mut self.state.battery_actions
        ] {
            for a in actions.iter_mut() {
                a.last_triggered = None;
            }
        }
        
        // Determine active block
        let active_block = if !self.state.ac_actions.is_empty() 
            || !self.state.battery_actions.is_empty() 
        {
            match self.state.on_battery() {
                Some(true) => "battery",
                Some(false) => "ac",
                None => "default",
            }
        } else {
            "default"
        };
        
        // Get mutable reference to active actions
        let actions = match active_block {
            "ac" => &mut self.state.ac_actions,
            "battery" => &mut self.state.battery_actions,
            _ => &mut self.state.default_actions,
        };
        
        // Find lock index and advance past it
        if let Some(lock_index) = actions.iter()
            .position(|a| matches!(a.kind, IdleAction::LockScreen))
        {
            let next_index = lock_index.saturating_add(1);
            self.state.action_index = next_index;
            
            // CRITICAL: Set the next action's last_triggered so timeout calculation works
            let debounce_end = now + debounce;
            if next_index < actions.len() {
                actions[next_index].last_triggered = Some(debounce_end);
                log_message(&format!(
                    "Advanced to action index {} ({}), will fire in {}s",
                    next_index,
                    actions[next_index].name,
                    actions[next_index].timeout
                ));
            } else {
                log_message("Advanced past all actions (at end of chain)");
            }
        } else {
            log_message("No lock action found in active block");
        }
    }

    pub async fn pause(&mut self, manual: bool) {
        if manual {
            self.state.manually_paused = true;
            log_message("Idle timers manually paused");
        } else if !self.state.manually_paused {
            self.state.paused = true;
            log_message("Idle timers automatically paused");
        }
    }

    pub async fn resume(&mut self, manually: bool) {
        if manually {
            if self.state.manually_paused {
                self.state.manually_paused = false;
                
                if self.state.active_inhibitor_count == 0 {
                    self.state.paused = false;
                    log_message("Idle timers manually resumed");
                } else {
                    log_message(&format!(
                        "Manual pause cleared, but {} inhibitor(s) still active - timers remain paused",
                        self.state.active_inhibitor_count
                    ));
                }
            }
        } else if !self.state.manually_paused && self.state.paused {
            // This is called by decr_active_inhibitor when count reaches 0
            self.state.paused = false;
            log_message("Idle timers automatically resumed");
        }
    }

    pub async fn toggle_state(&mut self, inhibit: bool) {
        if inhibit {
            self.pause(true).await;
        } else {
            self.resume(true).await;
        }
    }

    pub async fn recheck_media(&mut self) {
        // read ignore_remote_media + media blacklist from cfg
        let (ignore_remote, media_blacklist) = match &self.state.cfg {
            Some(cfg) => (cfg.ignore_remote_media, cfg.media_blacklist.clone()),
            None => (false, Vec::new()),
        };

        // Get a temporary connection for the check
        let conn = match Connection::session().await {
            Ok(c) => c,
            Err(_) => return,
        };

        // sync check (pactl + mpris).
        let playing = crate::core::services::media::check_media_playing_zbus(&conn, ignore_remote, &media_blacklist).await;

        // Only change state via the helpers so behaviour stays consistent:
        if playing && !self.state.media_playing {
            // call the same helper the monitor uses
            crate::core::manager::helpers::incr_active_inhibitor(self).await;
            self.state.media_playing = true;
        } else if !playing && self.state.media_playing {
            crate::core::manager::helpers::decr_active_inhibitor(self).await;
            self.state.media_playing = false;
        }
    }

    pub async fn shutdown(&mut self) {
        self.state.shutdown_flag.notify_waiters();

        sleep(Duration::from_millis(200)).await;

        if let Some(handle) = self.idle_task_handle.take() {
            handle.abort();
        }

        if let Some(handle) = self.lock_task_handle.take() {
            handle.abort();
        }

        if let Some(handle) = self.input_task_handle.take() {
            handle.abort();
        }

        for handle in self.spawned_tasks.drain(..) {
            handle.abort();
        }
    }
}
