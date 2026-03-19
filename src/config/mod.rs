use std::{path::PathBuf};
use eyre::Result;

pub mod bootstrap;
pub mod info;
pub mod model;
pub mod parser;

/// Determine default config path
pub async fn get_config_path() -> Result<PathBuf> {
    if let Some(mut path) = dirs::home_dir() {
        path.push(".config/stasys/stasys.rune");
        if path.exists() {
            return Ok(path);
        }
    }
    let fallback = PathBuf::from("/etc/stasys/stasys.rune");
    if fallback.exists() {
        return Ok(fallback);
    }
    Err(eyre::eyre!("Could not find stasys configuration file"))
}
