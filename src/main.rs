mod config;
mod message;
mod session;
mod ui;

use anyhow::Result;
use config::ConfigManager;
use message::Message;
use message::MessageBuffer;
use session::{SessionManager, SessionMode};

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let config = ConfigManager::load()?;

    // Load or create session
    let session = SessionManager::load_or_create_last()?;
    eprintln!("📋 Session: {}", session.id);
    eprintln!("🎯 Mode: {}", session.mode);

    // Load existing messages
    let messages_json = SessionManager::read_messages(&session.id)?;
    let mut messages: Vec<Message> = Vec::new();

    for msg_json in messages_json {
        if let Ok(msg) = serde_json::from_value::<Message>(msg_json) {
            messages.push(msg);
        }
    }

    // Create message buffer and load existing messages
    let message_buffer = MessageBuffer::from_messages(messages, config.context_limit);

    eprintln!(
        "📊 Context: {}/{} tokens ({:.1}%)",
        message_buffer.total_tokens(),
        config.context_limit,
        message_buffer.context_usage_percent()
    );

    // Run TUI
    ui::run_tui(session, message_buffer).await?;

    eprintln!("✅ Session saved and exiting");
    Ok(())
}
