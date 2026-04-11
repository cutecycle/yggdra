/// Battery status detection for adaptive rate limiting
use std::process::Command;

/// Check if system is running on battery power
/// Returns true if on battery, false if on AC power
/// Returns false if detection fails (assume AC to prevent throttling issues)
#[cfg(target_os = "macos")]
pub fn is_on_battery() -> bool {
    // macOS: check `pmset -g batt` for "AC Power" status
    if let Ok(output) = Command::new("pmset")
        .arg("-g")
        .arg("batt")
        .output()
    {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            // If output contains "AC Power", we're on AC (not on battery)
            return !stdout.contains("AC Power");
        }
    }
    false // Assume AC power if detection fails
}

#[cfg(target_os = "linux")]
pub fn is_on_battery() -> bool {
    // Linux: check /sys/class/power_supply/BAT*/status
    // or /sys/class/power_supply/BAT*/uevent
    if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("BAT")) {
                let status_file = path.join("status");
                if let Ok(status) = std::fs::read_to_string(&status_file) {
                    if status.trim() == "Discharging" {
                        return true;
                    }
                }
            }
        }
    }
    false // Assume AC power if no battery found
}

#[cfg(target_os = "windows")]
pub fn is_on_battery() -> bool {
    // Windows: use `powercfg /status` or WMI
    // For now, assume AC (most Windows users have stable power)
    false
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn is_on_battery() -> bool {
    // Unknown platform: assume AC power (safe default)
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_battery_detection_runs() {
        // Just verify the function runs without panicking
        let _ = is_on_battery();
    }
}
