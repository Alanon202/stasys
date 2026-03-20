use std::{fmt::{Display, Formatter, Result}, time::Instant};
use regex_lite::Regex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IdleAction {
    Brightness,
    Dpms,
    LockScreen,
    Suspend,
    Custom,
}

impl Display for IdleAction {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        match self {
            IdleAction::Brightness => write!(f, "brightness"),
            IdleAction::Dpms => write!(f, "dpms"),
            IdleAction::LockScreen => write!(f, "lock_screen"),
            IdleAction::Suspend => write!(f, "suspend"),
            IdleAction::Custom => write!(f, "custom"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IdleActionBlock {
    pub name: String,
    pub timeout: u64,
    pub command: String,
    pub kind: IdleAction,
    pub resume_command: Option<String>,
    pub lock_command: Option<String>,
    pub last_triggered: Option<Instant>,
}

impl IdleActionBlock {
    pub fn is_instant(&self) -> bool {
        self.timeout == 0
    }
    
    pub fn has_resume_command(&self) -> bool {
        self.resume_command.is_some()
    }

    pub fn get_lock_command(&self) -> &str {
        if self.command == "loginctl lock-session" {
            self.lock_command.as_deref().unwrap_or(&self.command)
        } else {
            &self.command
        }
    }
}

#[derive(Debug, Clone)]
pub enum AppInhibitPattern {
    Literal(String),
    Regex(Regex),
}

impl Display for AppInhibitPattern {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        match self {
            AppInhibitPattern::Literal(s) => write!(f, "{}", s),
            AppInhibitPattern::Regex(r) => write!(f, "(regex) {}", r.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LidCloseAction {
    Ignore,
    LockScreen,
    Suspend,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LidOpenAction {
    Ignore,
    Wake,
    Custom(String),
}

#[derive(Debug, Clone)]
pub struct StasisConfig {
    pub actions: Vec<IdleActionBlock>,
    pub debounce_seconds: u8,
    pub inhibit_apps: Vec<AppInhibitPattern>,
    pub monitor_media: bool,
    pub ignore_remote_media: bool,
    pub media_blacklist: Vec<String>,
    pub pre_suspend_command: Option<String>,
    pub respect_wayland_inhibitors: bool,
    pub lid_close_action: LidCloseAction,
    pub lid_open_action: LidOpenAction,
    pub notify_on_unpause: bool,
}

impl std::fmt::Display for LidCloseAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LidCloseAction::Ignore => write!(f, "ignore"),
            LidCloseAction::LockScreen => write!(f, "lock_screen"),
            LidCloseAction::Suspend => write!(f, "suspend"),
            LidCloseAction::Custom(cmd) => write!(f, "custom: {}", cmd),
        }
    }
}

impl std::fmt::Display for LidOpenAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LidOpenAction::Wake => write!(f, "wake"),
            LidOpenAction::Ignore => write!(f, "ignore"),
            LidOpenAction::Custom(cmd) => write!(f, "custom: {}", cmd),
        }
    }
}
