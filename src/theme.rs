/// Terminal theme detection and color palette
///
/// Queries the terminal background colour via OSC 11, falls back to
/// COLORFGBG env var, then defaults to Light (pastel looks fine everywhere).

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind { Light, Dark }

/// Colours used throughout the TUI, selected per terminal theme.
#[derive(Debug, Clone)]
pub struct Theme {
    pub kind: ThemeKind,
    // Message bands  (no explicit fg — terminal default text colour shows through)
    pub band_a: Color,   // alternating band A
    pub band_b: Color,   // alternating band B
    pub band_spawn: Color,
    // Accent used in command palette / model picker borders + selected rows
    pub accent: Color,   // solarized cyan-ish
    pub violet: Color,   // solarized violet-ish, used in model picker
    // Selected-row fg (always black so it reads on both accent bgs)
    pub selected_fg: Color,
    // Gradient background colors (start/end for vertical fade)
    pub gradient_start: Color,
    pub gradient_end: Color,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            band_a:     Color::Rgb(218, 228, 242), // soft blue
            band_b:     Color::Rgb(220, 238, 220), // soft green
            band_spawn: Color::Rgb(210, 238, 235), // soft teal
            accent:     Color::Rgb(42, 161, 152),  // solarized cyan
            violet:     Color::Rgb(108, 113, 196), // solarized violet
            selected_fg: Color::Black,
            gradient_start: Color::Rgb(218, 230, 242), // soft lavender-blue
            gradient_end:   Color::Rgb(245, 240, 232), // soft warm cream
        }
    }

    pub fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            band_a:     Color::Rgb(32, 50, 80),    // deep blue tint
            band_b:     Color::Rgb(28, 52, 32),    // deep green tint
            band_spawn: Color::Rgb(22, 48, 55),    // deep teal
            accent:     Color::Rgb(42, 161, 152),  // solarized cyan (same — works on dark)
            violet:     Color::Rgb(108, 113, 196), // solarized violet (same)
            selected_fg: Color::Black,
            gradient_start: Color::Rgb(32, 48, 80),   // deep blue
            gradient_end:   Color::Rgb(45, 31, 61),   // deep purple
        }
    }

    /// Detect terminal background and return the appropriate theme.
    /// Tries (in order):
    ///   1. OSC 11 query — most accurate
    ///   2. COLORFGBG env var
    ///   3. Default: Light
    pub fn detect() -> Self {
        if let Some(light) = detect_via_osc11() {
            return if light { Self::light() } else { Self::dark() };
        }
        if let Some(light) = detect_via_env() {
            return if light { Self::light() } else { Self::dark() };
        }
        Self::light() // safe default — pastel reads well everywhere
    }

    /// Safe background theme detection — no terminal manipulation.
    /// Can be called at any time while the TUI is running.
    /// Returns Some(is_light) or None if detection is unavailable.
    pub fn detect_safe() -> Option<bool> {
        // macOS: `defaults read -g AppleInterfaceStyle` returns "Dark" in dark mode,
        // exits with code 1 and empty output in light mode. No terminal involvement.
        #[cfg(target_os = "macos")]
        {
            if let Ok(output) = std::process::Command::new("defaults")
                .args(["read", "-g", "AppleInterfaceStyle"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_lowercase();
                if output.status.success() && stdout.contains("dark") {
                    return Some(false); // definitely dark mode
                }
                // Light mode: key doesn't exist → exit code 1, empty stdout.
                // Only assert light if we see that exact signature; any other failure
                // (process error, sandbox, etc.) returns None to avoid false positives.
                if !output.status.success() && stdout.is_empty() {
                    return Some(true); // definitely light mode
                }
                // Unexpected output — don't change the theme.
                return None;
            }
        }
        // Fallback: COLORFGBG env var (set once at shell startup; won't catch mid-session changes
        // on most terminals, but better than nothing on non-macOS platforms)
        detect_via_env()
    }
}

// ── OSC 11 detection ─────────────────────────────────────────────────────────

#[cfg(unix)]
fn detect_via_osc11() -> Option<bool> {
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;

    // Only attempt when connected to a real tty
    let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    if !is_tty {
        return None;
    }

    // Open /dev/tty so we don't interfere with stdin/stdout
    let mut tty = std::fs::OpenOptions::new()
        .read(true).write(true)
        .open("/dev/tty").ok()?;

    // Enable raw mode so the response bytes come back uncooked
    crossterm::terminal::enable_raw_mode().ok()?;

    // OSC 11 query: \e]11;?\e\\
    tty.write_all(b"\x1b]11;?\x1b\\").ok()?;
    let _ = tty.flush();

    // Poll for up to 250 ms
    let fd = tty.as_raw_fd();
    let readable = {
        let mut tv = libc::timeval { tv_sec: 0, tv_usec: 250_000 };
        let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_SET(fd, &mut fds);
            libc::select(fd + 1, &mut fds, std::ptr::null_mut(), std::ptr::null_mut(), &mut tv) > 0
                && libc::FD_ISSET(fd, &fds)
        }
    };

    let result = if readable {
        let mut buf = [0u8; 64];
        tty.read(&mut buf).ok()
            .and_then(|n| parse_osc11(std::str::from_utf8(&buf[..n]).ok()?))
    } else {
        None
    };

    let _ = crossterm::terminal::disable_raw_mode();
    result
}

#[cfg(not(unix))]
fn detect_via_osc11() -> Option<bool> { None }

/// Parse OSC 11 response: `\e]11;rgb:RRRR/GGGG/BBBB\e\\`
/// Returns Some(true) for light background, Some(false) for dark.
fn parse_osc11(s: &str) -> Option<bool> {
    let start = s.find("rgb:")? + 4;
    let rest = &s[start..];
    let parts: Vec<&str> = rest.splitn(3, '/').collect();
    if parts.len() < 3 { return None; }

    // Values can be 2 or 4 hex digits; strip trailing non-hex
    let parse_channel = |s: &str| -> Option<u32> {
        let hex: String = s.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let v = u32::from_str_radix(&hex, 16).ok()?;
        // Normalise 16-bit (0–65535) to 8-bit (0–255)
        Some(if v > 255 { v >> 8 } else { v })
    };

    let r = parse_channel(parts[0])?;
    let g = parse_channel(parts[1])?;
    let b = parse_channel(parts[2])?;

    // Perceived luminance (ITU-R BT.601)
    let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    Some(luma > 128.0)
}

// ── COLORFGBG fallback ───────────────────────────────────────────────────────

fn detect_via_env() -> Option<bool> {
    // COLORFGBG = "foreground;background"  (bg 0-6 dark, 7-15 light, 11 = "default dark")
    let val = std::env::var("COLORFGBG").ok()?;
    let bg = val.split(';').last()?.trim().parse::<i32>().ok()?;
    Some(bg >= 8 && bg != 11)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_osc11_4digit() {
        // White background (ffff/ffff/ffff)
        let s = "\x1b]11;rgb:ffff/ffff/ffff\x1b\\";
        assert_eq!(parse_osc11(s), Some(true));
    }

    #[test]
    fn test_parse_osc11_dark() {
        // Dark solarized base03 (#002b36 → 0000/2b2b/3636)
        let s = "\x1b]11;rgb:0000/2b2b/3636\x1b\\";
        assert_eq!(parse_osc11(s), Some(false));
    }

    #[test]
    fn test_parse_osc11_solarized_light() {
        // Solarized light base3 (#fdf6e3 → fdfd/f6f6/e3e3)
        let s = "\x1b]11;rgb:fdfd/f6f6/e3e3\x1b\\";
        assert_eq!(parse_osc11(s), Some(true));
    }
}
