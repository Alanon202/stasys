> **⚠️ STASYS - STABLE FORK**
> This is **Stasys** - a stable fork of Stasis v0.7.0, maintained to preserve brightness restoration and stable media detection.
> **Why this fork exists:** Upstream v0.8+ removed automatic brightness restoration and introduced media detection instability.
> **Goal:** Keep v0.7.0 stability with selective bug fixes from later versions.

<p align="center">
  <img src="assets/stasis.png" alt="Stasis Logo" width="200"/>
</p>

<h1 align="center">Stasys</h1>

<p align="center">
  <strong>A modern Wayland idle manager that knows when to step back.</strong>
</p>

<p align="center">
  <b>Stable Fork of Stasis v0.7.0</b> • Preserving brightness restoration and stable media detection
</p>

<p align="center">
  <img src="https://img.shields.io/github/last-commit/saltnpepper97/stasis?style=for-the-badge&color=%2328A745" alt="GitHub last commit"/>
  <img src="https://img.shields.io/badge/License-MIT-E5534B?style=for-the-badge" alt="MIT License"/>
  <img src="https://img.shields.io/badge/Wayland-00BFFF?style=for-the-badge&logo=wayland&logoColor=white" alt="Wayland"/>
  <img src="https://img.shields.io/badge/Rust-1.89+-orange?style=for-the-badge&logo=rust&logoColor=white" alt="Rust"/>
</p>

<p align="center">
  <a href="#-features">Features</a> •
  <a href="#-installation">Installation</a> •
  <a href="#-quick-start">Quick Start</a> •
  <a href="#compositor-support">Compositor Support</a> •
  <a href="#-configuration">Configuration</a> •
  <a href="#-profiles">Profiles</a>
</p>

---

## ✨ Features

Stasys doesn't just lock your screen after a timer—it understands context. Watching a video? Reading a document? Playing music? Stasys detects these scenarios and intelligently manages idle behavior.

- **🧠 Smart idle detection** with configurable timeouts
- **🎵 Media-aware idle handling** – automatically detects media playback
- **🚫 Application-specific inhibitors** – prevent idle when specific apps are running
- **⏸️ Idle inhibitor respect** – honors Wayland idle inhibitor protocols
- **🛌 Lid events via DBus** – detect laptop lid open/close events
- **⚙️ Flexible action system** – supports named action blocks and custom commands
- **🔍 Regex pattern matching** – powerful app filtering with regular expressions
- **📝 Clean configuration** – uses the intuitive [RUNE](https://github.com/saltnpepper97/rune-cfg) configuration language
- **⚡ Live reload** – update configuration without restarting the daemon
- **💡 Automatic brightness restoration** – captures and restores brightness on resume

## 📦 Installation

### From Releases

- Download the appropriate archive from Releases
- Extract `stasys` binary somewhere in your $PATH (ex. .local/bin)
- Adjust systemd service accordingly or add `stasys` to your DE’s startup menu

### From Source

Build and install manually:

```bash
# Clone and build
git clone https://github.com/Alanon202/stasys
cd stasys
cargo build --release --locked

# Install system-wide
sudo install -Dm755 target/release/stasys /usr/local/bin/stasys

# Or install to user directory
install -Dm755 target/release/stasys ~/.local/bin/stasys
```

### Systemd Service

A systemd user service file is provided in `systemd/stasys.service`. Copy it to your systemd user directory:

```bash
mkdir -p ~/.config/systemd/user
cp systemd/stasys.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now stasys.service
```

Edit the service file if you installed stasys to a different location than `/usr/local/bin/stasys`.

## 🚀 Quick Start

### 1. Add User to Required Groups

Stasys requires access to input devices and brightness controls:

```bash
sudo usermod -aG input,video $USER
```

**Log out and back in** for group changes to take effect.

### 2. Create Configuration

On first run, Stasys automatically generates a default configuration at `~/.config/stasys/stasys.rune`. You can also create it manually:

```bash
mkdir -p ~/.config/stasys
cp examples/stasys.rune ~/.config/stasys/stasys.rune
```

Edit the configuration to your needs.

### 3. Start Stasys

```bash
# Start the daemon
stasys daemon

# Or use systemd (recommended)
systemctl --user start stasys
```

### 4. Verify It's Running

```bash
stasys info
```

## Compositor Support

Stasys integrates with each compositor's native IPC protocol for optimal app detection and inhibition.

| Compositor | Support Status | Notes |
|------------|---------------|-------|
| **Niri** | ✅ Full Support | Tested and working perfectly |
| **Hyprland** | ✅ Full Support | Native IPC integration |
| **labwc** | ⚠️ Limited | Process-based fallback |
| **River** | ⚠️ Limited | Process-based fallback |

### River & labwc Compatibility Notes

Both River and labwc have IPC protocol limitations:

- **Limited window enumeration** – Can't get complete window lists via IPC
- **Fallback mode** – Uses process-based detection (sysinfo) for app inhibition
- **Pattern adjustments** – Executable names may differ from app IDs

> **💡 Tip:** When using River or labwc, include both exact executable names and flexible regex patterns in your `inhibit_apps` configuration.

## 🔧 Configuration

Stasys uses **[RUNE](https://github.com/saltnpepper97/rune-cfg)**—a purpose-built configuration language.

### Config Location

- **User config:** `~/.config/stasys/stasys.rune`
- **System config:** `/etc/stasys/stasys.rune`
- **Logs:** `~/.cache/stasys/stasys.log`

### Example Configuration

```rune
stasis:
  pre_suspend_command "hyprlock"
  monitor_media true
  ignore_remote_media true
  respect_idle_inhibitors true
  
  # Laptop lid events
  lid_close_action "lock-screen"
  lid_open_action "wake"
  
  # Desktop idle actions
  lock_screen:
    timeout 300  # 5 minutes
    command "loginctl lock-session"
    resume-command "notify-send 'Welcome Back!'"
    lock-command "swaylock"
  end

  dpms:
    timeout 60  # 1 minute
    command "niri msg action power-off-monitors"
    resume-command "niri msg action power-on-monitors"
  end

  suspend:
    timeout 1800  # 30 minutes
    command "systemctl suspend"
  end
end
```

### CLI Usage

```bash
# Show current state
stasys info

# Trigger action manually
stasys trigger lock-screen

# Pause idle detection
stasys pause for 1h
stasys resume

# Toggle idle inhibition (Waybar-friendly)
stasys toggle-inhibit

# Reload config
stasys reload

# View recent logs
stasys dump 50

# Stop daemon
stasys stop
```

## 📋 Profiles

Profiles let you switch between different configurations for different scenarios (work, gaming, presentations, etc.).

### How Profiles Work

- Each profile is a **standalone config file** in `~/.config/stasys/profiles/`
- Switching profiles **completely replaces** your current config
- Actions **not defined** in a profile are **disabled** during that profile, unless your system provides fallbacks
- Profile state persists across restarts

### Creating Profiles

- Create a profiles directory in `~/.config/stasys/profiles`
- Create a profile (e.g., work.rune) manually or use one of the examples in the repo

### Switching Profiles

```bash
# List available profiles
stasys profile list

# Switch to a profile
stasys profile work

# Return to base config
stasys profile none

# Check current profile
stasys info
```

### Example Profiles

Three example profiles are included in `examples/profiles/`:

| Profile | Purpose | Key Changes |
|---------|---------|-------------|
| **work.rune** | Office work | Longer timeouts, video call apps inhibited |
| **gaming.rune** | Gaming | Gaming apps inhibited, no brightness auto-dim |
| **presentation.rune** | Presentations | No idle, max brightness, inhibitors disabled |

### Waybar Integration

Add profile switching to your Waybar module:

```json
"custom/stasys": {
  "exec": "stasys info --json",
  "format": "{icon}",
  "format-icons": {
    "idle_active": "󰾆",
    "idle_inhibited": "󰅶",
    "manually_inhibited": "󰅶",
    "not_running": "󰒲"
  },
  "tooltip": true,
  "on-click": "stasys toggle-inhibit",
  "on-click-right": "stasys profile cycle",
  "on-click-middle": "stasys info",
  "interval": 2,
  "return-type": "json"
}
```

**Profile cycling:** Right-click cycles through: `none` → `work` → `gaming` → `presentation` → `none`...

Alternatively, you can simply use one fallback profile and invoke it directly, i.e. "stasys profile work"

### Advanced: Custom Profile Cycle Script

Create a script for custom profile cycling:

```bash
#!/bin/bash
# ~/.local/bin/stasys-profile-cycle

PROFILES=("none" "work" "gaming" "presentation")
CURRENT=$(cat ~/.config/stasys/active_profile 2>/dev/null || echo "none")

# Find current index
for i in "${!PROFILES[@]}"; do
    if [[ "${PROFILES[$i]}" == "$CURRENT" ]]; then
        NEXT="${PROFILES[$(( (i + 1) % ${#PROFILES[@]} ))]}"
        stasys profile "$NEXT"
        notify-send "Stasys Profile" "Switched to: $NEXT"
        exit 0
    fi
done

# Fallback
stasys profile none
```

Make it executable and update Waybar:
```bash
chmod +x ~/.local/bin/stasys-profile-cycle
```

```json
"custom/stasys": {
  "on-click-right": "~/.local/bin/stasys-profile-cycle"
}
```

## 🤝 Contributing

Contributions are welcome! Here's how you can help:

- 🐛 **Report bugs** – Open an issue with reproduction steps
- 💡 **Suggest features** – Share your use cases and ideas
- 🔧 **Submit PRs** – Fix bugs, add features, or improve code
- 📖 **Improve docs** – Better explanations, examples, and guides

## 📄 License

Released under the [MIT License](LICENSE) – free to use, modify, and distribute.

---

<p align="center">
  <sub>Built with ❤️ for the Wayland community</sub><br>
  <sub><i>Keeping your session in perfect balance between active and idle</i></sub>
</p>
