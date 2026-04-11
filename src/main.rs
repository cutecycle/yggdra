mod config;
mod message;
mod notifications;
mod ollama;
mod session;
mod steering;
mod tools;
mod agent;
mod ui;

use anyhow::Result;
use session::Session;
use ui::App;

#[tokio::main]
async fn main() -> Result<()> {
    let config = config::Config::load_with_smart_model().await;
    let session = Session::load_or_create()?;

    eprintln!("🌷 Yggdra v0.1 starting...");
    eprintln!("📁 Session: {}", session.id);
    eprintln!("📝 Messages DB: {}", session.messages_db.display());

    // Create Ollama client (reuses the validated endpoint from config)
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

    notifications::session_created(&session.id).await;

    let mut app = App::new(config, session, ollama_client);
    app.run().await?;

    eprintln!("👋 Goodbye!");
    Ok(())
}
