mod config;
mod message;
mod notifications;
mod ollama;
mod session;
mod steering;
mod ui;

use anyhow::Result;
use config::Config;
use session::Session;
use ui::App;

#[tokio::main]
async fn main() -> Result<()> {
    // Load config with smart model detection from Ollama
    let config = config::Config::load_with_smart_model().await;

    // Load or create session for this directory
    let session = Session::load_or_create()?;

    eprintln!("🌷 Yggdra v0.1 starting...");
    eprintln!("📁 Session: {}", session.id);
    eprintln!("📝 Messages DB: {}", session.messages_db.display());

    // Create Ollama client and validate connection
    let ollama_client = match ollama::OllamaClient::new(&config.endpoint, &config.model).await {
        Ok(client) => {
            eprintln!("🌻 Ollama client initialized");
            Some(client)
        }
        Err(e) => {
            eprintln!("🌹 Warning: Could not connect to Ollama: {}", e);
            notifications::error_occurred(&format!("Ollama connection failed: {}", e)).await;
            None
        }
    };

    // Emit session creation notification
    notifications::session_created(&session.id).await;

    // Run TUI
    let mut app = App::new(config, session, ollama_client);
    app.run().await?;

    eprintln!("👋 Goodbye!");
    Ok(())
}
