mod config;
mod message;
mod notifications;
mod session;
mod steering;
mod ui;

use anyhow::Result;
use config::Config;
use session::Session;
use ui::App;

#[tokio::main]
async fn main() -> Result<()> {
    // Load config from environment
    let config = Config::load();

    // Load or create session for this directory
    let session = Session::load_or_create()?;

    eprintln!("🌷 Yggdra v0.1 starting...");
    eprintln!("📁 Session: {}", session.id);
    eprintln!("📝 Messages file: {}", session.messages_file.display());

    // Emit session creation notification
    notifications::session_created(&session.id).await;

    // Run TUI
    let mut app = App::new(config, session);
    app.run().await?;

    eprintln!("👋 Goodbye!");
    Ok(())
}
