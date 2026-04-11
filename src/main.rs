mod config;
mod gaps;
mod msglog;
mod message;
mod notifications;
mod ollama;
mod session;
mod steering;
mod theme;
mod tools;
mod agent;
mod spawner;
mod task;
mod ui;

use anyhow::Result;
use session::Session;
use ui::App;

#[tokio::main]
async fn main() -> Result<()> {
    // Become process group leader so all children die when we exit
    #[cfg(unix)]
    unsafe { libc::setpgid(0, 0); }

    let config = config::Config::load_with_smart_model().await;
    let session = Session::load_or_create()?;

    // Load AGENTS.md from CWD if present
    let agents_md = std::env::current_dir()
        .ok()
        .map(|p| p.join("AGENTS.md"))
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(&p).ok())
        .filter(|c| !c.trim().is_empty());

    // Create Ollama client (reuses the validated endpoint from config)
    let ollama_client = match ollama::OllamaClient::new(&config.endpoint, &config.model).await {
        Ok(client) => Some(client),
        Err(_) => {
            notifications::error_occurred("Ollama connection failed").await;
            None
        }
    };

    let mut app = App::new(config, session, ollama_client, agents_md);
    let result = app.run().await;

    // Kill entire process group on exit (catches spawned subagents)
    #[cfg(unix)]
    unsafe {
        libc::kill(0, libc::SIGTERM);
    }

    result
}
