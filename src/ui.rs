/// TUI module: minimal terminal UI with multi-window sync via polling
use crate::config::Config;
use crate::message::{Message, MessageBuffer};
use crate::ollama::OllamaClient;
use crate::session::Session;
use crate::steering::SteeringDirective;
use crate::tools::ToolRegistry;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

type _TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Minimal TUI application
pub struct App {
    config: Config,
    session: Session,
    input_buffer: String,
    status_message: String,
    running: bool,
    message_buffer: MessageBuffer,
    ollama_client: Option<OllamaClient>,
    waiting_for_response: bool,
    tool_registry: ToolRegistry,
}

impl App {
    /// Create new app with optional Ollama client
    pub fn new(
        config: Config,
        session: Session,
        ollama_client: Option<OllamaClient>,
    ) -> Self {
        let message_buffer = MessageBuffer::from_db(&session.messages_db)
            .unwrap_or_else(|_| MessageBuffer::from_jsonl_file(&session.messages_db));
        let status_message = if ollama_client.is_some() {
            "✅ Ollama connected".to_string()
        } else {
            "❌ Ollama offline".to_string()
        };

        Self {
            config,
            session,
            input_buffer: String::new(),
            status_message,
            running: true,
            message_buffer,
            ollama_client,
            waiting_for_response: false,
            tool_registry: ToolRegistry::new(),
        }
    }

    /// Run the TUI
    pub async fn run(&mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        let waiting_flag = Arc::new(Mutex::new(false));

        loop {
            // Draw UI
            terminal.draw(|f| self.draw(f))?;

            // Handle events with timeout
            if crossterm::event::poll(Duration::from_millis(500))? {
                if let Event::Key(key) = event::read()? {
                    if !*waiting_flag.lock().await {
                        self.handle_key(key).await;
                        if !self.running {
                            break;
                        }
                    }
                }
            } else {
                // Timeout: poll for changes (reload from DB)
                self.poll_for_updates()?;
            }
        }

        // Cleanup terminal
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;

        Ok(())
    }

    /// Draw UI frame
    fn draw(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(2),
                    Constraint::Min(5),
                    Constraint::Length(3),
                    Constraint::Length(2),
                ]
                .as_ref(),
            )
            .split(f.area());

        // Header with status
        let header_text = if self.waiting_for_response {
            format!("🌷 Yggdra v0.1 - {} | Endpoint: {} | ⏳ Processing...",
                if self.ollama_client.is_some() { "✅ Connected" } else { "⚠️ Offline" },
                self.config.endpoint.replace("http://", "").replace(":11434", "")
            )
        } else {
            format!("🌷 Yggdra v0.1 - {} | Endpoint: {} | Model: {}",
                if self.ollama_client.is_some() { "✅ Connected" } else { "⚠️ Offline" },
                self.config.endpoint.replace("http://", "").replace(":11434", ""),
                self.config.model
            )
        };

        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::BOTTOM).title("Status"));
        f.render_widget(header, chunks[0]);

        // Messages output with scrolling
        let messages_text: Vec<Line> = self
            .message_buffer
            .messages()
            .unwrap_or_default()
            .iter()
            .map(|m| {
                let emoji = match m.role.as_str() {
                    "user" => "👤",
                    "assistant" => "🤖",
                    "tool" => "🔧",
                    _ => "💬",
                };
                Line::from(format!("{} [{}] {}", emoji, m.role, m.content))
            })
            .collect();

        let output = Paragraph::new(messages_text)
            .block(Block::default().title(" Conversation ").borders(Borders::ALL))
            .wrap(ratatui::widgets::Wrap { trim: true });
        f.render_widget(output, chunks[1]);

        // Input area
        let input_hint = if self.waiting_for_response {
            "(waiting for response...)"
        } else {
            "(type message or /help for commands)"
        };
        let input_text = if self.input_buffer.is_empty() {
            input_hint.to_string()
        } else {
            self.input_buffer.clone()
        };

        let input = Paragraph::new(format!("> {}", input_text))
            .block(Block::default().title(" Input ").borders(Borders::ALL));
        f.render_widget(input, chunks[2]);

        // Status bar with helpful info
        let status = format!(
            "Session: {} | Msgs: {} | {} | [Ctrl+C] Exit [ESC] Clear",
            &self.session.id[..8],
            self.message_buffer.messages().map(|m| m.len()).unwrap_or(0),
            self.status_message.lines().next().unwrap_or("")
        );
        let status_bar = Paragraph::new(status);
        f.render_widget(status_bar, chunks[3]);
    }

    /// Handle keyboard input
    async fn handle_key(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;

        match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if c == 'c' {
                    self.running = false;
                }
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Enter => {
                self.handle_command().await;
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
            }
            _ => {}
        }
    }

    /// Handle command submission
    async fn handle_command(&mut self) {
        let command = self.input_buffer.trim().to_string();

        // Validate input
        if command.is_empty() {
            self.status_message = "ℹ️ Please enter a message or command. Type /help for commands.".to_string();
            self.input_buffer.clear();
            return;
        }

        // Reject commands with invalid characters that could cause shell injection
        if command.starts_with("/tool ") {
            let tool_args = command.strip_prefix("/tool ").unwrap_or("").trim();
            if tool_args.is_empty() {
                self.status_message = "❌ Invalid tool command: /tool requires arguments".to_string();
                self.input_buffer.clear();
                return;
            }
            self.handle_tool_command(&command).await;
        } else if command == "/models" {
            self.handle_models_command().await;
        } else if command == "/help" {
            self.show_help();
        } else if command.starts_with('/') {
            self.status_message = format!("❓ Unknown command: '{}'. Type /help for available commands.", command);
        } else if !command.is_empty() {
            // Message validation: no excessive length, check for reasonable content
            self.handle_message(&command).await;
        }

        self.input_buffer.clear();
    }

    /// Display help text with all available commands
    fn show_help(&mut self) {
        self.status_message = 
            "📖 Commands:\n\
             /help         - Show this help message\n\
             /models       - List available Ollama models\n\
             /tool CMD     - Execute a local tool\n\n\
             Available tools:\n\
             • rg          - Ripgrep search (usage: /tool rg pattern path)\n\
             • spawn       - Execute binary (usage: /tool spawn /path/to/bin args)\n\
             • editfile    - Edit file (usage: /tool editfile /path/to/file)\n\
             • commit      - Git commit (usage: /tool commit \"message\")\n\
             • python      - Run Python script (usage: /tool python script.py args)\n\
             • ruste       - Compile/run Rust (usage: /tool ruste script.rs)\n\n\
             Keybindings:\n\
             Enter - Submit message\n\
             Escape - Clear input\n\
             Ctrl+C - Exit".to_string();
    }

    /// Handle /tool command for local tool execution
    async fn handle_tool_command(&mut self, command: &str) {
        let tool_args = command.strip_prefix("/tool ").unwrap_or("").trim();
        
        if tool_args.is_empty() {
            self.status_message = "❌ Tool error: No tool specified. Usage: /tool TOOL_NAME ARGS".to_string();
            return;
        }

        // Parse tool name and args: "rg pattern /path" -> tool_name="rg", args="pattern /path"
        let parts: Vec<&str> = tool_args.splitn(2, ' ').collect();
        if parts.is_empty() {
            self.status_message = "❌ Tool error: Invalid tool command".to_string();
            return;
        }

        let tool_name = parts[0];
        let args = if parts.len() > 1 { parts[1] } else { "" };

        self.waiting_for_response = true;
        self.status_message = format!("⏳ Executing tool: {}", tool_name);

        // Execute tool via registry
        let result = self.tool_registry.execute(tool_name, args);

        match result {
            Ok(tool_output) => {
                let output_msg = if tool_output.is_empty() {
                    "[Tool executed successfully with no output]".to_string()
                } else {
                    tool_output.lines().take(30).collect::<Vec<_>>().join("\n")
                };

                let response = format!("[TOOL: {} {}]\n{}", tool_name, args, output_msg);
                
                let tool_msg = Message::new("tool", response);
                if let Err(e) = self.message_buffer.add_and_persist(tool_msg) {
                    self.status_message = format!("❌ Failed to save tool output: {}", e);
                } else {
                    self.status_message = format!("✅ Tool {} executed successfully", tool_name);
                }
            }
            Err(e) => {
                self.status_message = format!("❌ Tool {} error: {}", tool_name, e);
            }
        }

        self.waiting_for_response = false;
    }

    /// Handle /models command
    async fn handle_models_command(&mut self) {
        match &self.ollama_client {
            Some(client) => {
                self.waiting_for_response = true;
                self.status_message = "⏳ Fetching models from Ollama...".to_string();

                match tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client.list_models()
                ).await {
                    Ok(Ok(models)) => {
                        if models.is_empty() {
                            self.status_message = "ℹ️ No models found. Pull a model using: ollama pull <model_name>".to_string();
                        } else {
                            let mut output = "📦 Available Models:\n".to_string();
                            for (i, model) in models.iter().enumerate() {
                                if i < 10 {
                                    output.push_str(&format!("  • {}\n", model.name));
                                }
                            }
                            if models.len() > 10 {
                                output.push_str(&format!("  ... and {} more\n", models.len() - 10));
                            }
                            self.status_message = output;
                        }
                        eprintln!("Models: {:?}", models);
                    }
                    Ok(Err(e)) => {
                        let friendly_msg = if e.to_string().contains("connection refused") {
                            "Ollama is not running. Start it with: ollama serve".to_string()
                        } else {
                            self.friendly_error(&e.to_string())
                        };
                        self.status_message = format!("❌ Failed to fetch models: {}", friendly_msg);
                        eprintln!("Error listing models: {}", e);
                    }
                    Err(_) => {
                        self.status_message = "❌ Model fetch timeout: Ollama is not responding".to_string();
                    }
                }

                self.waiting_for_response = false;
            }
            None => {
                self.status_message = "⚠️ Ollama not connected. Check that Ollama is running on http://localhost:11434".to_string();
            }
        }
    }

    /// Handle user message - send to Ollama with steering
    async fn handle_message(&mut self, message: &str) {
        // Validate message
        if message.is_empty() {
            self.status_message = "❌ Message cannot be empty".to_string();
            return;
        }

        if message.len() > 10000 {
            self.status_message = "❌ Message too long (max 10000 characters)".to_string();
            return;
        }

        // Save user message
        let user_msg = Message::new("user", message.to_string());
        if let Err(e) = self.message_buffer.add_and_persist(user_msg) {
            eprintln!("Failed to save user message: {}", e);
            self.status_message = format!("❌ Storage error: Cannot save message - {}", self.friendly_error(&e.to_string()));
            return;
        }

        // If no Ollama client, store message but explain limitation
        if self.ollama_client.is_none() {
            self.status_message = "⚠️ Ollama offline: Message saved locally but not sent to model. Try /help for more info.".to_string();
            return;
        }

        self.waiting_for_response = true;
        self.status_message = "⏳ Sending message to model...".to_string();

        // Prepare messages for Ollama
        let messages_for_ollama: Vec<Message> = self
            .message_buffer
            .messages()
            .unwrap_or_default();

        // Apply steering directive
        let steering_directive = SteeringDirective::custom("Be concise and helpful");
        let steering_text = steering_directive.format_for_system_prompt();

        // Send to Ollama with timeout handling
        if let Some(client) = &self.ollama_client {
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                client.generate(messages_for_ollama, Some(&steering_text))
            ).await {
                Ok(Ok(response)) => {
                    let model_msg = Message::new("assistant", response.clone());
                    if let Err(e) = self.message_buffer.add_and_persist(model_msg) {
                        eprintln!("Failed to save model response: {}", e);
                        self.status_message = format!("⚠️ Response received but not saved: {}", self.friendly_error(&e.to_string()));
                    } else {
                        self.status_message = "✅ Model responded".to_string();
                        crate::notifications::model_responded(&response[..std::cmp::min(100, response.len())]).await;
                    }
                }
                Ok(Err(e)) => {
                    let friendly_msg = self.friendly_error(&e.to_string());
                    eprintln!("Error sending message to Ollama: {}", e);
                    self.status_message = format!("❌ Model error: {}", friendly_msg);
                    crate::notifications::error_occurred(&format!("Failed to get response: {}", friendly_msg)).await;
                }
                Err(_) => {
                    eprintln!("Timeout waiting for Ollama response");
                    self.status_message = "❌ Model timeout: Response took too long (>2 minutes). Try a shorter message.".to_string();
                    crate::notifications::error_occurred("Request timeout").await;
                }
            }
        }

        self.waiting_for_response = false;
    }

    /// Convert technical errors to user-friendly messages
    fn friendly_error(&self, error: &str) -> String {
        if error.contains("refused") || error.contains("connection refused") {
            "Ollama is offline. Make sure Ollama is running on http://localhost:11434".to_string()
        } else if error.contains("model") && error.contains("not found") {
            format!("Model '{}' not found. Use /models to see available models.", self.config.model)
        } else if error.contains("timeout") {
            "Connection timeout. Ollama may be unresponsive.".to_string()
        } else if error.contains("permission") {
            "Permission denied. Check file/directory permissions.".to_string()
        } else if error.contains("Parse") || error.contains("parse") {
            "Invalid response format from Ollama. Check logs.".to_string()
        } else {
            // Generic fallback - show first 80 chars
            let truncated = if error.len() > 80 {
                format!("{}...", &error[..80])
            } else {
                error.to_string()
            };
            truncated
        }
    }

    /// Poll for database changes by reloading from SQLite
    fn poll_for_updates(&mut self) -> Result<()> {
        if let Ok(new_buffer) = MessageBuffer::from_db(&self.session.messages_db) {
            self.message_buffer = new_buffer;
        }
        Ok(())
    }
}

