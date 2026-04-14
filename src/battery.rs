/// Battery status detection for adaptive rate limiting
use std::process::Command;

/// Tri-state battery status — distinguishes "on AC" from "detection unavailable"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryState {
    OnBattery,
    AC,
    Unknown,
}

/// Detect current battery state
#[cfg(target_os = "macos")]
pub fn battery_state() -> BatteryState {
    if let Ok(output) = Command::new("pmset")
        .arg("-g")
        .arg("batt")
        .output()
    {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            return if stdout.contains("AC Power") {
                BatteryState::AC
            } else {
                BatteryState::OnBattery
            };
        }
    }
    BatteryState::Unknown
}

#[cfg(target_os = "linux")]
pub fn battery_state() -> BatteryState {
    if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("BAT")) {
                let status_file = path.join("status");
                if let Ok(status) = std::fs::read_to_string(&status_file) {
                    return if status.trim() == "Discharging" {
                        BatteryState::OnBattery
                    } else {
                        BatteryState::AC
                    };
                }
            }
        }
    }
    BatteryState::Unknown
}

#[cfg(target_os = "windows")]
pub fn battery_state() -> BatteryState {
    BatteryState::Unknown
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn battery_state() -> BatteryState {
    BatteryState::Unknown
}

/// Convenience wrapper for existing callers (knowledge_index.rs)
pub fn is_on_battery() -> bool {
    battery_state() == BatteryState::OnBattery
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_battery_detection_runs() {
        let state = battery_state();
        // Just verify it runs without panicking and returns a valid variant
        match state {
            BatteryState::OnBattery | BatteryState::AC | BatteryState::Unknown => {}
        }
        // Convenience wrapper should be consistent
        assert_eq!(is_on_battery(), state == BatteryState::OnBattery);
    }
}
