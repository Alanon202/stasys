use std::{sync::Arc, time::{Duration, Instant}};

use tokio::sync::Notify;

use crate::{
    config::model::{IdleAction, IdleActionBlock, StasisConfig}, log::log_message
};
use crate::core::utils::{detect_chassis, ChassisKind};

#[derive(Debug)]
pub struct ManagerState {
    pub ac_actions: Vec<IdleActionBlock>,
    pub action_index: usize,
    pub active_flags: ActiveFlags,
    pub active_inhibitor_count: u32,
    pub app_inhibit_debounce: Option<Instant>,
    pub battery_actions: Vec<IdleActionBlock>,
    pub brightness_device: Option<String>,
    pub cfg: Option<Arc<StasisConfig>>,
    pub chassis: ChassisType, 
    pub compositor_managed: bool,
    pub current_block: String,
    pub dbus_inhibit_active: bool,
    pub debounce: Option<Instant>,
    pub default_actions: Vec<IdleActionBlock>,
    pub instants_triggered: bool,
    pub last_activity: Instant,
    pub lock_state: LockState,
    pub lock_notify: Arc<Notify>,
    pub manually_paused: bool,
    pub max_brightness: Option<u32>,
    pub media_blocking: bool,
    pub media_playing: bool,
    pub notify: Arc<Notify>,
    pub paused: bool,
    pub previous_brightness: Option<u32>,
    pub pre_suspend_command: Option<String>,
    pub resume_queue: Vec<IdleActionBlock>,
    pub resume_commands_fired: bool,
    pub shutdown_flag: Arc<Notify>,
    pub start_time: Instant,
    pub suspend_occured: bool,
}

impl Default for ManagerState {
    fn default() -> Self {
        let now = Instant::now();

        Self {
            ac_actions: Vec::new(),
            action_index: 0,
            active_flags: ActiveFlags::default(),
            active_inhibitor_count: 0,
            app_inhibit_debounce: None,
            battery_actions: Vec::new(),
            brightness_device: None,
            cfg: None,
            chassis: ChassisType::Desktop(DesktopState),
            compositor_managed: false,
            current_block: "default".to_string(),
            dbus_inhibit_active: false,
            debounce: None,
            default_actions: Vec::new(),
            instants_triggered: false,
            last_activity: now, 
            lock_state: LockState::default(),
            manually_paused: false,
            max_brightness: None,
            media_blocking: false,
            media_playing: false,
            notify: Arc::new(Notify::new()),
            lock_notify: Arc::new(Notify::new()),
            paused: false,
            previous_brightness: None,
            pre_suspend_command: None,
            resume_queue: Vec::new(),
            resume_commands_fired: false,
            shutdown_flag: Arc::new(Notify::new()),
            start_time: now,
            suspend_occured: false,
        }
    }
}

impl ManagerState {
    pub fn new(cfg: Arc<StasisConfig>) -> Self { 
        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| !a.name.starts_with("ac.") && !a.name.starts_with("battery."))
            .cloned()
            .collect();

        let ac_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("ac."))
            .cloned()
            .collect();

        let battery_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("battery."))
            .cloned()
            .collect();

        let now = Instant::now();
        let debounce = Some(now + Duration::from_secs(cfg.debounce_seconds as u64));

        let chassis = match detect_chassis() {
            ChassisKind::Laptop => ChassisType::Laptop(LaptopState { on_battery: false }),
            ChassisKind::Desktop => ChassisType::Desktop(DesktopState),
        };

        // Initial block - will be updated by power detection
        let current_block = match &chassis {
            ChassisType::Desktop(_) => "default".to_string(),
            ChassisType::Laptop(_) => "ac".to_string(), // Default to AC, will be corrected by power detection
        };

        let state = Self {
            ac_actions,
            action_index: 0,
            active_flags: ActiveFlags::default(),
            active_inhibitor_count: 0,
            app_inhibit_debounce: None,
            battery_actions,
            brightness_device: None,
            cfg: Some(cfg.clone()),
            chassis,
            compositor_managed: false,
            current_block,
            dbus_inhibit_active: false,
            debounce,
            default_actions,
            instants_triggered: false,
            last_activity: now,
            lock_state: LockState::from_config(&cfg),
            manually_paused: false,
            max_brightness: None,
            media_blocking: false,
            media_playing: false,
            notify: Arc::new(Notify::new()),
            lock_notify: Arc::new(Notify::new()),
            paused: false,
            previous_brightness: None,
            pre_suspend_command: cfg.pre_suspend_command.clone(),
            resume_queue: Vec::new(),
            resume_commands_fired: false,
            shutdown_flag: Arc::new(Notify::new()),
            start_time: now,
            suspend_occured: false,
        };

        state
    }

    pub fn is_laptop(&self) -> bool {
        matches!(self.chassis, ChassisType::Laptop(_))
    }

    pub fn on_battery(&self) -> Option<bool> {
        match &self.chassis {
            ChassisType::Laptop(l) => Some(l.on_battery),
            ChassisType::Desktop(_) => None,
        }
    }

    pub fn set_on_battery(&mut self, value: bool) {
        if let ChassisType::Laptop(l) = &mut self.chassis {
            l.on_battery = value;
            // Update current_block when power state changes
            self.update_current_block();
        }
    }

    /// Update current_block based on chassis type and power state
    pub fn update_current_block(&mut self) {
        let new_block = match &self.chassis {
            ChassisType::Desktop(_) => "default".to_string(),
            ChassisType::Laptop(state) => {
                if state.on_battery {
                    if !self.battery_actions.is_empty() {
                        "battery".to_string()
                    } else {
                        "default".to_string()
                    }
                } else {
                    if !self.ac_actions.is_empty() {
                        "ac".to_string()
                    } else {
                        "default".to_string()
                    }
                }
            }
        };

        if new_block != self.current_block {
            let old_block = self.current_block.clone();
            self.current_block = new_block;
            log_message(&format!(
                "Switched active block: {} -> {}",
                old_block, self.current_block
            ));
            
            // Reset state when switching blocks
            self.action_index = 0;
            self.instants_triggered = false;
            self.notify.notify_one();
        }
    }

    /// Get the currently active action list based on current_block
    pub fn get_active_actions(&self) -> &[IdleActionBlock] {
        match self.current_block.as_str() {
            "ac" => &self.ac_actions,
            "battery" => &self.battery_actions,
            "default" => &self.default_actions,
            _ => &self.default_actions,
        }
    }

    /// Get mutable reference to the currently active action list
    pub fn get_active_actions_mut(&mut self) -> &mut Vec<IdleActionBlock> {
        match self.current_block.as_str() {
            "ac" => &mut self.ac_actions,
            "battery" => &mut self.battery_actions,
            "default" => &mut self.default_actions,
            _ => &mut self.default_actions,
        }
    }

    /// Get all instant actions from the currently active action list
    pub fn get_active_instant_actions(&self) -> Vec<IdleActionBlock> {
        self.get_active_actions()
            .iter()
            .filter(|a| a.is_instant())
            .cloned()
            .collect()
    }

    pub async fn update_from_config(&mut self, cfg: &StasisConfig) {
        self.active_flags = ActiveFlags::default();
        self.previous_brightness = None;
        self.pre_suspend_command = cfg.pre_suspend_command.clone();

        // Split actions into blocks
        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| !a.name.starts_with("ac.") && !a.name.starts_with("battery."))
            .cloned()
            .collect();

        let ac_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("ac."))
            .cloned()
            .collect();

        let battery_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("battery."))
            .cloned()
            .collect();

        // Replace the old state vectors
        self.default_actions = default_actions;
        self.ac_actions = ac_actions;
        self.battery_actions = battery_actions;

        // Reset last_triggered for all actions
        for actions in [&mut self.default_actions, &mut self.ac_actions, &mut self.battery_actions] {
            for a in actions.iter_mut() {
                a.last_triggered = None;
            }
        }

        // Update current_block based on new config
        self.update_current_block();

        // Reset instant trigger flag
        self.instants_triggered = false;
        
        // Reset debounce according to new cfg
        let debounce = Duration::from_secs(cfg.debounce_seconds as u64);
        self.debounce = Some(Instant::now() + debounce);

        self.cfg = Some(Arc::new(cfg.clone()));
        self.lock_state = LockState::from_config(cfg);
        self.last_activity = Instant::now();

        // Reset action index
        self.action_index = 0;

        // Wake idle task to recalc immediately
        self.notify.notify_one();

        log_message(&format!(
            "Idle timers reloaded from config (active block: {})",
            self.current_block
        ));
    }
}

#[derive(Debug)]
pub enum ChassisType {
    Laptop(LaptopState),
    Desktop(DesktopState),
}

#[derive(Debug)]
pub struct LaptopState {
    pub on_battery: bool,
}

#[derive(Debug)]
pub struct DesktopState;

impl Default for ChassisType {
    fn default() -> Self {
        ChassisType::Desktop(DesktopState)
    }
}

#[derive(Debug)]
pub struct LockState {
    pub is_locked: bool,
    pub pid: Option<u32>,
    pub command: Option<String>,
    pub last_advanced: Option<Instant>,
    pub post_advanced: bool,
}

impl Default for LockState {
    fn default() -> Self {
        Self {
            is_locked: false,
            pid: None,
            command: None,
            last_advanced: None,
            post_advanced: false,
        }
    }
}

impl LockState {
    pub fn from_config(cfg: &StasisConfig) -> Self {
        // Find the first LockScreen action (there should usually be one)
        let lock_action = cfg.actions.iter().find(|a| a.kind == IdleAction::LockScreen);

        let command = lock_action.map(|a| {
            // If lock_command is set, check for that process (hyprlock, etc)
            // Otherwise check for the regular command process
            if let Some(ref lock_cmd) = a.lock_command {
                lock_cmd.clone()
            } else {
                a.command.clone()
            }
        });


        Self {
            is_locked: false,
            pid: None,
            command,
            last_advanced: None,
            post_advanced: false,
        }
    }
}

#[derive(Debug)]
pub struct ActiveFlags {
    pub pre_suspend_triggered: bool,
    pub brightness_captured: bool,
}

impl Default for ActiveFlags {
    fn default() -> Self {
        Self {
            pre_suspend_triggered: false,
            brightness_captured: false,
        }
    }
}
