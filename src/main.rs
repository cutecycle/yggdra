mod battery;
mod config;
mod dlog;
mod gaps;
mod highlight;
mod knowledge_index;
mod msglog;
mod message;
mod notifications;
mod ollama;
mod sandbox;
mod session;
mod steering;
mod theme;
mod tools;
mod agent;
mod spawner;
mod task;
mod ui;
mod metrics;
mod watcher;

use anyhow::Result;
use session::Session;
use ui::App;
use config::AppMode;

#[tokio::main]
async fn main() -> Result<()> {
    // Become process group leader so all children die when we exit
    #[cfg(unix)]
    unsafe { libc::setpgid(0, 0); }

    // Parse CLI arguments for mode override
    let mut mode_override: Option<AppMode> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--ask" => mode_override = Some(AppMode::Ask),
            "--build" => mode_override = Some(AppMode::Build),
            "--plan" => mode_override = Some(AppMode::Plan),
            "--help" | "-h" => {
                eprintln!("Usage: yggdra [OPTIONS]");
                eprintln!("Options:");
                eprintln!("  --ask       Start in ask-only mode");
                eprintln!("  --build     Start in build mode");
                eprintln!("  --plan      Start in plan mode (default)");
                eprintln!("  --help      Show this help message");
                return Ok(());
            }
            _ => {}
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Initialise sandbox — all file tools will be restricted to this root
    sandbox::init(cwd.clone());

    // Terraform: ensure git repo exists
    if !cwd.join(".git").exists() {
        eprintln!("🌱 No git repo found — running git init");
        let _ = std::process::Command::new("git")
            .arg("init")
            .current_dir(&cwd)
            .output();
    }

    // Terraform: if there are no commits yet, snapshot the current state.
    // This gives the user a safe baseline to revert to before the agent begins.
    let has_commits = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_commits {
        eprintln!("🌱 No commits yet — creating initial snapshot");
        let _ = std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "chore: initial snapshot (pre-agent baseline)"])
            .current_dir(&cwd)
            .output();
    }

    let (config, probe_client) = config::Config::load_with_smart_model().await;
    let config = if let Some(mode) = mode_override {
        let mut c = config;
        c.mode = mode;
        eprintln!("🔧 CLI override: mode={}", mode);
        c
    } else {
        config
    };

    // Ensure config file is created on every startup, regardless of mode
    if let Err(e) = config.save() {
        eprintln!("⚠️  Failed to save config: {}", e);
    }

    // Terraform: ensure .yggdra subdirectories exist
    let yggdra_dir = cwd.join(".yggdra");
    let _ = std::fs::create_dir_all(yggdra_dir.join("log"));
    let _ = std::fs::create_dir_all(yggdra_dir.join("todo"));

    // Create .yggdra/knowledge symlink → ~/source/repos/offlinebase if missing
    let knowledge_link = yggdra_dir.join("knowledge");
    if !knowledge_link.exists() && !knowledge_link.is_symlink() {
        if let Some(home) = dirs::home_dir() {
            let offlinebase = home.join("source").join("repos").join("offlinebase");
            if offlinebase.exists() {
                #[cfg(unix)]
                let _ = std::os::unix::fs::symlink(&offlinebase, &knowledge_link);
                #[cfg(not(unix))]
                let _ = std::fs::create_dir_all(&knowledge_link); // fallback on non-unix
            }
        }
    }

    // Init debug log (writes to .yggdra/debug.log)
    dlog::init();
    dlog::log(&format!("startup: mode={} model={} endpoint={}", config.mode, config.model, config.endpoint));

    // Spawn filesystem watcher for config.json and AGENTS.md changes
    let config_watcher_rx = match crate::watcher::spawn_watcher(cwd.clone()) {
        Ok((rx, _handle)) => {
            eprintln!("🔍 Filesystem watcher started");
            Some(rx)
        }
        Err(e) => {
            eprintln!("⚠️  Failed to start filesystem watcher: {}", e);
            // Return a channel that never emits - watcher is optional
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            drop(tx);
            Some(rx)
        }
    };
    
    // Start background knowledge indexing task
    let index_config = crate::knowledge_index::KnowledgeIndexConfig {
        size_limit_bytes: (config.knowledge_index.size_limit_gb as u64) * 1024 * 1024 * 1024,
        battery_delay_ms: config.knowledge_index.battery_delay_ms,
        enabled: config.knowledge_index.enabled,
    };
    knowledge_index::start_indexing_task(Some(index_config));
    
    let session = Session::load_or_create()?;

    // Load AGENTS.md from CWD if present
    let agents_md = std::fs::read_to_string(cwd.join("AGENTS.md"))
        .ok()
        .filter(|c| !c.trim().is_empty());

    // Reuse the client already validated during config load if the model matches,
    // otherwise create a fresh one.  Either way we avoid a second /api/tags round-trip.
    let ollama_client = if let Some(probe) = probe_client {
        if probe.model() == config.model {
            Some(probe)
        } else {
            // model was overridden (CLI flag or env var) — swap model, no new HTTP call
            Some(ollama::OllamaClient::new_with_existing(probe, &config.model))
        }
    } else {
        match ollama::OllamaClient::new(&config.endpoint, &config.model).await {
            Ok(client) => Some(client),
            Err(_) => {
                notifications::error_occurred("Ollama connection failed").await;
                None
            }
        }
    };

    let mut app = App::new(config, session, ollama_client, agents_md, config_watcher_rx);
    let result = app.run().await;

    // Kill entire process group on exit (catches spawned subagents)
    #[cfg(unix)]
    unsafe {
        libc::kill(0, libc::SIGTERM);
    }

    result
}
