use std::time::Duration;

pub enum ChassisKind {
    Laptop,
    Desktop,
}

pub fn detect_chassis() -> ChassisKind {
    // Try reading from sysfs
    if let Ok(data) = std::fs::read_to_string("/sys/class/dmi/id/chassis_type") {
        let laptop_chassis = vec![
            "8",  // Portable
            "9",  // Laptop
            "10", // Notebook
            "14", // Sub Notebook
            "30", // Tablet
            "31", // Convertible
            "32", // Detachable
        ];
        if laptop_chassis.contains(&data.trim()) {
            return ChassisKind::Laptop;
        }
    }

    ChassisKind::Desktop
}

pub fn format_duration(dur: Duration) -> String {
    let secs = dur.as_secs();

    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let minutes = secs / 60;
        let seconds = secs % 60;
        format!("{}m {}s", minutes, seconds)
    } else {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        format!("{}h {}m", hours, minutes)
    }
}
