//! Native OS notifications for Yggdra events.
//! Emits system notifications for key lifecycle events.
//!
//! ## Platform behavior
//!
//! - **macOS**: Uses `osascript` (`display notification`) as the primary path.
//!   `notify-rust` on macOS uses NSUserNotification, which is deprecated and
//!   silently fails for unbundled binaries (no `.app` wrapper / Info.plist /
//!   bundle identifier registered with Notification Center). `osascript` is
//!   delivered via Script Editor's bundle and Just Works™.
//!   Falls back to `notify-rust` if `osascript` fails.
//! - **Linux**: Uses `notify-rust` (D-Bus / `org.freedesktop.Notifications`).
//!   Requires a notification daemon to be running.
//!
//! Notifications are fire-and-forget; delivery failures are logged to stderr.
//!
//! ## User setup (macOS)
//!
//! If notifications don't appear, ensure "Script Editor" is allowed in
//! System Settings → Notifications.

use notify_rust::Notification;

/// Send a notification when a new session is created
pub async fn session_created(session_id: &str) {
    send_notification(
        "🌷 Yggdra: New Session",
        &format!("Session: {}", session_id),
        3000,
    );
}

/// Send a notification when model responds
pub async fn model_responded(preview: &str) {
    send_notification("🌻 Model Response", preview, 5000);
}

/// Send a notification for errors
pub async fn error_occurred(error: &str) {
    send_notification("🌹 Error", error, 5000);
}

/// Agent wants to communicate something to the human operator.
/// Shown in chat and fires a notification that persists until dismissed
/// (where supported — macOS notifications via osascript do not honor a
/// "never expire" timeout, but they remain in Notification Center).
pub async fn agent_says(message: &str) {
    send_notification_persistent("💬 Agent", message);
}

/// Internal helper: send a timed notification with error logging.
fn send_notification(summary: &str, body: &str, timeout_ms: i32) {
    if body.is_empty() {
        return;
    }
    dispatch(summary, body, Some(timeout_ms));
}

/// Internal helper: send a "persistent" notification.
fn send_notification_persistent(summary: &str, body: &str) {
    if body.is_empty() {
        return;
    }
    dispatch(summary, body, None);
}

/// Platform dispatcher.
#[cfg(target_os = "macos")]
fn dispatch(summary: &str, body: &str, _timeout_ms: Option<i32>) {
    match send_via_osascript(summary, body) {
        Ok(()) => {
            crate::dlog!("[notify] {}", summary);
        }
        Err(e) => {
            crate::dlog!("[notify] osascript failed ({}); falling back to notify-rust", e);
            send_via_notify_rust(summary, body, _timeout_ms);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn dispatch(summary: &str, body: &str, timeout_ms: Option<i32>) {
    send_via_notify_rust(summary, body, timeout_ms);
}

/// Send via `osascript -e 'display notification "..." with title "..."'`.
/// Properly escapes backslashes and double quotes for AppleScript string literals.
#[cfg(target_os = "macos")]
fn send_via_osascript(summary: &str, body: &str) -> Result<(), String> {
    use std::process::Command;

    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape_applescript(body),
        escape_applescript(summary),
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("spawn osascript: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("osascript exit {}: {}", output.status, stderr.trim()))
    }
}

/// Escape a string for use inside a double-quoted AppleScript string literal.
/// AppleScript string literals only need backslash and double-quote escaping.
#[cfg(target_os = "macos")]
fn escape_applescript(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // Newlines/tabs become spaces — AppleScript strings can contain
            // them but they confuse the notification renderer.
            '\n' | '\r' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn send_via_notify_rust(summary: &str, body: &str, timeout_ms: Option<i32>) {
    let mut n = Notification::new();
    n.summary(summary).body(body);
    match timeout_ms {
        Some(ms) => {
            n.timeout(ms);
        }
        None => {
            n.timeout(notify_rust::Timeout::Never);
        }
    }
    match n.show() {
        Ok(_) => crate::dlog!("[notify] {} (notify-rust)", summary),
        Err(e) => crate::dlog!("[notify] failed to send '{}': {}", summary, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_created() {
        session_created("test-session-id").await;
    }

    #[tokio::test]
    async fn test_model_responded() {
        model_responded("test response").await;
    }

    #[tokio::test]
    async fn test_error_occurred() {
        error_occurred("test error").await;
    }

    #[tokio::test]
    async fn test_agent_says() {
        agent_says("test message").await;
    }

    #[test]
    fn test_empty_bodies_are_skipped() {
        send_notification("Summary", "", 1000);
        send_notification_persistent("Summary", "");
    }

    #[test]
    fn test_notification_send() {
        send_notification("Test Summary", "Test Body", 1000);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_applescript_escaping() {
        assert_eq!(escape_applescript("hello"), "hello");
        assert_eq!(escape_applescript("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_applescript("path\\to\\file"), "path\\\\to\\\\file");
        assert_eq!(escape_applescript("line1\nline2"), "line1 line2");
        // Order matters: backslashes must be escaped before quotes,
        // otherwise the escaped quote's backslash gets re-escaped.
        assert_eq!(escape_applescript("a\\\"b"), "a\\\\\\\"b");
    }
}
