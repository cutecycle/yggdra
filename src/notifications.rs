/// Native OS notifications for Yggdra events
/// Emits system notifications for key lifecycle events

use notify_rust::Notification;

/// Send a notification when a new session is created
pub async fn session_created(session_id: &str) {
    let _ = Notification::new()
        .summary("🌷 Yggdra: New Session")
        .body(&format!("Session: {}", session_id))
        .timeout(3000)
        .show();
}

/// Send a notification when model responds
pub async fn model_responded(preview: &str) {
    let _ = Notification::new()
        .summary("🌻 Model Response")
        .body(preview)
        .timeout(5000)
        .show();
}

/// Send a notification for errors
pub async fn error_occurred(error: &str) {
    let _ = Notification::new()
        .summary("🌹 Error")
        .body(error)
        .timeout(5000)
        .show();
}

/// Send a notification for tool execution
pub async fn tool_executed(tool_name: &str) {
    let _ = Notification::new()
        .summary("🌼 Tool Executed")
        .body(tool_name)
        .timeout(2000)
        .show();
}
