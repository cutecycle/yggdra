/// TUI module: minimal terminal UI with streaming responses and multi-window sync
use crate::config::Config;
use crate::message::{Message, MessageBuffer};
use crate::ollama::{OllamaClient, StreamEvent};
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
use std::time::Duration;
use tokio::sync::mpsc;

type _TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// RAII guard to restore terminal state on drop (including panics/errors)
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

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
    cached_message_count: usize,
    /// Accumulates tokens during streaming
    streaming_text: String,
    /// Receives tokens from the streaming task
    stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
}

impl App {
    /// Create new app with optional Ollama client
    pub fn new(
        config: Config,
        session: Session,
        ollama_client: Option<OllamaClient>,
    ) -> Self {
        let message_buffer = MessageBuffer::from_db(&session.messages_db)
            .unwrap_or_else(|e| {
                eprintln!("🌹 Failed to open messages DB: {}", e);
                MessageBuffer::new(&session.messages_db)
                    .expect("Cannot create message database")
            });
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
            cached_message_count: 0,
            streaming_text: String::new(),
            stream_rx: None,
        }
    }

    /// Run the TUI — main event loop with streaming support
    pub async fn run(&mut self) -> Result<()> {
        let _guard = TerminalGuard::new()?;

        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        loop {
            // Drain any pending stream tokens before drawing
            self.drain_stream_tokens();

            terminal.draw(|f| self.draw(f))?;

            // Short poll when streaming (16ms for smooth token display), longer when idle
            let poll_ms = if self.stream_rx.is_some() { 16 } else { 200 };

            if crossterm::event::poll(Duration::from_millis(poll_ms))? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key).await;
                    if !self.running {
                        break;
                    }
                }
            } else if self.stream_rx.is_none() {
                self.poll_for_updates();
            }
        }

        Ok(())
    }

    /// Drain all available tokens from the stream receiver
    fn drain_stream_tokens(&mut self) {
        let rx = match self.stream_rx.as_mut() {
            Some(rx) => rx,
            None => return,
        };

        loop {
            match rx.try_recv() {
                Ok(StreamEvent::Token(token)) => {
                    self.streaming_text.push_str(&token);
                }
                Ok(StreamEvent::Done) => {
                    self.finish_streaming();
                    return;
                }
                Ok(StreamEvent::Error(e)) => {
                    self.status_message = format!("❌ Stream error: {}", e);
                    self.streaming_text.clear();
                    self.stream_rx = None;
                    self.waiting_for_response = false;
                    return;
                }
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // Channel closed unexpectedly — save what we have
                    if !self.streaming_text.is_empty() {
                        self.finish_streaming();
                    } else {
                        self.stream_rx = None;
                        self.waiting_for_response = false;
                    }
                    return;
                }
            }
        }
    }

    /// Finalize streaming: persist the accumulated response and clean up
    fn finish_streaming(&mut self) {
        if !self.streaming_text.is_empty() {
            let model_msg = Message::new("assistant", self.streaming_text.clone());
            if let Err(e) = self.message_buffer.add_and_persist(model_msg) {
                eprintln!("Failed to save streamed response: {}", e);
                self.status_message = format!("⚠️ Response received but not saved: {}", e);
            } else {
                self.status_message = "✅ Model responded".to_string();
                self.cached_message_count = self.message_buffer.count().unwrap_or(self.cached_message_count + 1);
            }
        }
        self.streaming_text.clear();
        self.stream_rx = None;
        self.waiting_for_response = false;
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

        // Header
        let header_text = if self.waiting_for_response {
            format!("🌷 Yggdra v0.1 - {} | {} | ⏳ Streaming...",
                if self.ollama_client.is_some() { "✅ Connected" } else { "⚠️ Offline" },
                self.config.model
            )
        } else {
            format!("🌷 Yggdra v0.1 - {} | {} | Model: {}",
                if self.ollama_client.is_some() { "✅ Connected" } else { "⚠️ Offline" },
                self.config.endpoint.replace("http://", "").replace(":11434", ""),
                self.config.model
            )
        };

        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::BOTTOM).title("Status"));
        f.render_widget(header, chunks[0]);

        // Messages + live streaming text
        let mut messages_text: Vec<Line> = self
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

        // Show partial streaming response
        if !self.streaming_text.is_empty() {
            messages_text.push(Line::from(format!("🤖 [assistant] {}▌", self.streaming_text)));
        }

        let output = Paragraph::new(messages_text)
            .block(Block::default().title(" 🌸 Conversation ").borders(Borders::ALL))
            .wrap(ratatui::widgets::Wrap { trim: true });
        f.render_widget(output, chunks[1]);

        // Input area
        let input_hint = if self.waiting_for_response {
            "(streaming response...)"
        } else {
            "(type message or /help for commands)"
        };
        let input_text = if self.input_buffer.is_empty() {
            input_hint.to_string()
        } else {
            self.input_buffer.clone()
        };

        let input = Paragraph::new(format!("> {}", input_text))
            .block(Block::default().title(" 🌱 Input ").borders(Borders::ALL));
        f.render_widget(input, chunks[2]);

        // Status bar
        let status = format!(
            "Session: {} | Msgs: {} | {} | [Ctrl+C] Exit [ESC] Clear",
            &self.session.id[..8],
            self.cached_message_count,
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

    /// Handle user message — kick off streaming generation
    async fn handle_message(&mut self, message: &str) {
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
            self.status_message = format!("❌ Storage error: {}", self.friendly_error(&e.to_string()));
            return;
        }
        self.cached_message_count = self.message_buffer.count().unwrap_or(self.cached_message_count + 1);

        if self.ollama_client.is_none() {
            self.status_message = "⚠️ Ollama offline: Message saved but not sent.".to_string();
            return;
        }

        self.waiting_for_response = true;
        self.status_message = "⏳ Streaming response...".to_string();

        let steering_directive = SteeringDirective::custom(
            "You are Yggdra, a local agentic assistant. You have access to tools for \
             interacting with the local filesystem and running code. Available tools:\n\
             - rg PATTERN PATH — search files with ripgrep\n\
             - spawn BINARY ARGS — execute a local binary\n\
             - editfile PATH — edit a file\n\
             - commit \"MESSAGE\" — git commit\n\
             - python SCRIPT ARGS — run a Python script\n\
             - ruste FILE — compile and run Rust code\n\
             To use a tool, output: [TOOL: name args]\n\
             Be concise and helpful. You work entirely offline."
        );
        let steering_text = steering_directive.format_for_system_prompt();

        let messages_for_ollama: Vec<Message> = self
            .message_buffer
            .messages()
            .unwrap_or_default();

        // Start streaming — returns immediately, tokens arrive via channel
        if let Some(client) = &self.ollama_client {
            let rx = client.generate_streaming(messages_for_ollama, Some(&steering_text));
            self.stream_rx = Some(rx);
            self.streaming_text.clear();
        }
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

    /// Poll for database changes via existing connection (multi-window sync)
    fn poll_for_updates(&mut self) {
        if let Ok(count) = self.message_buffer.count() {
            if count != self.cached_message_count {
                self.cached_message_count = count;
            }
        }
    }
}

