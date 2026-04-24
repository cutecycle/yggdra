mod battery;
mod config;
mod dlog;
mod epoch;
mod gaps;
mod highlight;

mod markdown;
mod msglog;
mod message;
mod notifications;
mod ollama;
mod sandbox;
mod session;
mod stats;
mod steering;
mod sysinfo;
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
            "--ask"        => mode_override = Some(AppMode::Ask),
            "--build"      => mode_override = Some(AppMode::Forever),
            "--plan"       => mode_override = Some(AppMode::Plan),
            "--one"        => mode_override = Some(AppMode::One),
            "--test"          => {
                mode_override = Some(AppMode::Forever);
                std::env::set_var("YGGDRA_TEST_MODE", "1");
            }
            "--shell-only" | "--standard" => {} // no-op: shell-only is now hardcoded
            "--help" | "-h" => {
                eprintln!("Usage: yggdra [OPTIONS]");
                eprintln!("Options:");
                eprintln!("  --ask         Start in ask-only mode");
                eprintln!("  --build       Start in build mode");
                eprintln!("  --plan        Start in plan mode (default)");
                eprintln!("  --one         Start in one-off task mode (auto-completes with notification)");
                eprintln!("  --test          Run in build mode using OpenRouter API for tests");
                eprintln!("  --help        Show this help message");
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

    // Terraform: ensure .yggdra is ignored by git
    let gitignore_path = cwd.join(".gitignore");
    if gitignore_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&gitignore_path) {
            if !content.contains(".yggdra/") {
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&gitignore_path)
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "\n.yggdra/")
                    });
            }
        }
    } else {
        let _ = std::fs::write(gitignore_path, ".yggdra/\n");
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
    let config = {
        let mut c = config;
        if let Some(mode) = mode_override {
            c.mode = mode;
            eprintln!("🔧 CLI override: mode={}", mode);
        }
        c
    };

    // Ensure config file is created on every startup, regardless of mode
    if let Err(e) = config.save() {
        eprintln!("⚠️  Failed to save config: {}", e);
    }

    // Terraform: ensure .yggdra subdirectories exist
    let yggdra_dir = cwd.join(".yggdra");
    let _ = std::fs::create_dir_all(yggdra_dir.join("log"));
    let _ = std::fs::create_dir_all(yggdra_dir.join("todo"));

    // Ensure .yggdra/knowledge → ~/source/repos/offlinebase on every startup.
    // Always re-points a stale/broken symlink; leaves real directories alone.
    let knowledge_link = yggdra_dir.join("knowledge");
    if let Some(home) = dirs::home_dir() {
        let offlinebase = home.join("source").join("repos").join("offlinebase");
        if offlinebase.exists() {
            #[cfg(unix)]
            {
                let needs_create = if knowledge_link.is_symlink() {
                    // Re-point if wrong target or broken
                    std::fs::read_link(&knowledge_link)
                        .map(|t| t != offlinebase)
                        .unwrap_or(true)
                } else {
                    // Create only if nothing is there (don't overwrite a real directory)
                    !knowledge_link.exists()
                };
                if needs_create {
                    let _ = std::fs::remove_file(&knowledge_link); // no-op if absent
                    let _ = std::os::unix::fs::symlink(&offlinebase, &knowledge_link);
                }
            }
            #[cfg(not(unix))]
            if !knowledge_link.exists() {
                let _ = std::fs::create_dir_all(&knowledge_link);
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
    
    
    // One mode uses ephemeral sessions — each invocation is a fresh start.
    let session = if config.mode == AppMode::One {
        Session::create_ephemeral()?
    } else {
        Session::load_or_create()?
    };

    // Load AGENTS.md: global ~/AGENTS.md first, then project-local appended
    let global_agents = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join("AGENTS.md")).ok())
        .filter(|c| !c.trim().is_empty());
    let local_agents = std::fs::read_to_string(cwd.join("AGENTS.md"))
        .ok()
        .filter(|c| !c.trim().is_empty());
    let agents_md = yggdra::merge_agents_md(global_agents, local_agents);

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
        // Build the client immediately (no blocking probe) so the TUI can start at once.
        // A background task validates connectivity and notifies if Ollama is unreachable.
        match ollama::OllamaClient::new_unchecked(&config.endpoint, &config.model, config.api_key.as_deref()) {
            Ok(client) => {
                let checker = client.clone();
                tokio::spawn(async move {
                    if let Err(_) = checker.list_models().await {
                        notifications::error_occurred("Ollama connection failed").await;
                    }
                });
                Some(client)
            }
            Err(_) => None,
        }
    };

    // Detect native context length in the background — purely informational, not needed at launch.
    if let Some(ref client) = ollama_client {
        let ctx_client = client.clone();
        tokio::spawn(async move { ctx_client.fetch_native_ctx().await; });
    }

    let mut app = App::new(config, session, ollama_client, agents_md, config_watcher_rx);
    let result = app.run().await;

    // Kill entire process group on exit (catches spawned subagents)
    #[cfg(unix)]
    unsafe {
        libc::kill(0, libc::SIGTERM);
    }

    result
}

