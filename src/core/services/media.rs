use std::sync::Arc;
use eyre::Result;
use futures_util::stream::StreamExt;
use tokio::task;
use zbus::{Connection, MatchRule, MessageStream, Proxy};

use crate::core::manager::{helpers::{decr_active_inhibitor, incr_active_inhibitor}, Manager};

// Players that are always considered local (browsers, local video players)
const ALWAYS_LOCAL_PLAYERS: &[&str] = &[
    "firefox",
    "zen",
    "floorp",
    "chrome",
    "chromium",
    "brave",
    "opera",
    "vivaldi",
    "edge",
    "safari",
    "mpv",
    "vlc",
    "totem",
    "celluloid",
];

pub async fn spawn_media_monitor_dbus(manager: Arc<tokio::sync::Mutex<Manager>>, conn: Connection) -> Result<()> {
    // Check if media monitoring is enabled in config
    let monitor_media = {
        let mgr = manager.lock().await;
        mgr.state.cfg
            .as_ref()
            .map(|c| c.monitor_media)
            .unwrap_or(true)
    };

    if !monitor_media {
        crate::log::log_message("Media monitoring disabled in config, skipping media monitor startup");
        return Ok(());
    }

    crate::log::log_message("Starting MPRIS media monitor");

    task::spawn(async move {
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus.Properties")
            .unwrap()
            .member("PropertiesChanged")
            .unwrap()
            .path_namespace("/org/mpris/MediaPlayer2")
            .unwrap()
            .build();

        let mut stream = MessageStream::for_match_rule(rule, &conn, None).await.unwrap();

        // Initial check
        update_media_state(&manager, &conn).await;

        loop {
            if let Some(_msg) = stream.next().await {
                update_media_state(&manager, &conn).await;
            }
        }
    });
    Ok(())
}

async fn update_media_state(manager: &Arc<tokio::sync::Mutex<Manager>>, conn: &Connection) {
    let (ignore_remote_media, media_blacklist) = {
        let mgr = manager.lock().await;
        let ignore = mgr.state.cfg.as_ref().map(|c| c.ignore_remote_media).unwrap_or(false);
        let blacklist = mgr.state.cfg.as_ref().map(|c| c.media_blacklist.clone()).unwrap_or_default();
        (ignore, blacklist)
    };

    let any_playing = check_media_playing_zbus(conn, ignore_remote_media, &media_blacklist).await;
    let mut mgr = manager.lock().await;
    if any_playing && !mgr.state.media_playing {
        incr_active_inhibitor(&mut mgr).await;
        mgr.state.media_playing = true;
        mgr.state.media_blocking = true;
    } else if !any_playing && mgr.state.media_playing {
        decr_active_inhibitor(&mut mgr).await;
        mgr.state.media_playing = false;
        mgr.state.media_blocking = false;
    }
}

pub async fn check_media_playing_zbus(conn: &Connection, ignore_remote_media: bool, media_blacklist: &[String]) -> bool {
    let dbus_proxy = Proxy::new(conn, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus").await.unwrap();
    let names: Vec<String> = dbus_proxy.call("ListNames", &()).await.unwrap_or_default();

    let mpris_names: Vec<_> = names.into_iter().filter(|n| n.starts_with("org.mpris.MediaPlayer2.")).collect();

    if mpris_names.is_empty() {
        return false;
    }

    // First pass: Check all players for Playing status
    let mut found_playing_player = false;
    let mut playing_players: Vec<(String, String)> = Vec::new();

    for name in &mpris_names {
        let player_proxy = match Proxy::new(conn, name.as_str(), "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player").await {
            Ok(p) => p,
            Err(_) => continue,
        };

        let playback_status: String = player_proxy.get_property("PlaybackStatus").await.unwrap_or_else(|_| "Stopped".to_string());

        if playback_status != "Playing" {
            continue;
        }

        let mpris_proxy = match Proxy::new(conn, name.as_str(), "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2").await {
            Ok(p) => p,
            Err(_) => continue,
        };
        let identity: String = mpris_proxy.get_property("Identity").await.unwrap_or_else(|_| name.clone());

        playing_players.push((name.clone(), identity));
        found_playing_player = true;
    }

    if !found_playing_player {
        return false;
    }

    for (name, identity) in &playing_players {
        let identity_lower = identity.to_lowercase();
        let bus_name_lower = name.to_lowercase();
        let combined = format!("{} {}", identity_lower, bus_name_lower);

        let is_blacklisted = media_blacklist.iter().any(|b| {
            let b_lower = b.to_lowercase();
            combined.contains(&b_lower)
        });

        if is_blacklisted {
            continue;
        }

        let is_always_local = ALWAYS_LOCAL_PLAYERS.iter().any(|local| {
            combined.contains(local)
        });

        if is_always_local {
            return true;
        }

        if !has_any_media_playing().await {
            continue; // No audio detected for this player, check next
        }

        // Audio detected - now check user preference
        if ignore_remote_media {
            // User wants to ignore remote media
            // Verify audio is actually going to a running sink
            if has_running_sink().await {
                return true; // Local audio output confirmed
            }
            // No running sink, so this is likely remote - check next player
            continue;
        } else {
            // User doesn't want to ignore remote media
            // Any playing media counts
            return true;
        }
    }

    // All playing players were filtered out
    // Double-check with pactl to catch race conditions (e.g., browser MPRIS lag)
    has_any_media_playing().await
}

async fn has_any_media_playing() -> bool {
    // Small delay to allow audio buffers to sync (matching original logic)
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    
    is_alsa_pcm_running()
}

async fn has_running_sink() -> bool {
    // In ALSA terms, a running PCM playback device is a running sink.
    is_alsa_pcm_running()
}

fn is_alsa_pcm_running() -> bool {
    let proc_asound = std::path::Path::new("/proc/asound");
    if !proc_asound.exists() {
        return false;
    }

    if let Ok(entries) = std::fs::read_dir(proc_asound) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();

            // Only look into card directories (card0, card1, etc.)
            if name.starts_with("card") && entry.path().is_dir() {
                if let Ok(pcm_entries) = std::fs::read_dir(entry.path()) {
                    for pcm_entry in pcm_entries.flatten() {
                        let pcm_name = pcm_entry.file_name().to_string_lossy().into_owned();

                        // Look for playback devices (ending in 'p')
                        if pcm_name.starts_with("pcm") && pcm_name.ends_with('p') {
                            // Check all subdevices (sub0, sub1, etc.)
                            if let Ok(sub_entries) = std::fs::read_dir(pcm_entry.path()) {
                                for sub_entry in sub_entries.flatten() {
                                    if sub_entry.file_name().to_string_lossy().starts_with("sub") {
                                        let mut status_path = sub_entry.path();
                                        status_path.push("status");

                                        if let Ok(status) = std::fs::read_to_string(&status_path) {
                                            if status.contains("state: RUNNING") {
                                                return true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}
