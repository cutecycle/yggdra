/// TUI module: minimal terminal UI with streaming responses, tool execution, and multi-window sync
use crate::agent;
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
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

type _TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

const MAX_TOOL_ITERATIONS: usize = 10;

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

/// Explicit state machine for the agentic turn lifecycle
#[derive(Debug, Clone, PartialEq)]
enum TurnPhase {
    Idle,
    Streaming,
    ExecutingTool(String),
}

/// Result from async tool execution
struct ToolResult {
    tool_name: String,
    _args: String,
    output: std::result::Result<String, String>,
}

/// Application mode: autonomous execution vs interactive planning
#[derive(Debug, Clone, PartialEq)]
enum AppMode {
    Build,
    Plan,
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
    tool_registry: ToolRegistry,
    cached_message_count: usize,
    /// Accumulates tokens during streaming
    streaming_text: String,
    /// Receives tokens from the streaming task
    stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
    /// Explicit state machine for the current turn
    turn_phase: TurnPhase,
    /// How many tool→re-stream cycles this turn
    tool_iteration_count: usize,
    /// Receives result from async tool execution
    tool_result_rx: Option<oneshot::Receiver<ToolResult>>,
    /// Build (autonomous) vs Plan (interactive) mode
    mode: AppMode,
    /// Contents of AGENTS.md if found on startup
    agents_task: Option<String>,
}

impl App {
    /// Create new app with optional Ollama client and AGENTS.md content
    pub fn new(
        config: Config,
        session: Session,
        ollama_client: Option<OllamaClient>,
        agents_md: Option<String>,
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
            tool_registry: ToolRegistry::new(),
            cached_message_count: 0,
            streaming_text: String::new(),
            stream_rx: None,
            turn_phase: TurnPhase::Idle,
            tool_iteration_count: 0,
            tool_result_rx: None,
            mode: AppMode::Build,
            agents_task: agents_md,
        }
    }

    /// Run the TUI — main event loop with streaming support
    pub async fn run(&mut self) -> Result<()> {
        let _guard = TerminalGuard::new()?;

        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        // Kick off AGENTS.md task immediately in Build mode if present
        if let Some(task) = self.agents_task.clone() {
            if !task.trim().is_empty() {
                self.handle_message(&task).await;
            }
        }

        loop {
            // Drain any pending stream tokens before drawing
            self.drain_stream_tokens();
            // Check for completed tool execution
            self.check_tool_result();

            terminal.draw(|f| self.draw(f))?;

            // Fast poll when active (16ms for smooth token display), longer when idle
            let poll_ms = if self.turn_phase == TurnPhase::Idle { 200 } else { 16 };

            if crossterm::event::poll(Duration::from_millis(poll_ms))? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key).await;
                    if !self.running {
                        break;
                    }
                }
            } else if self.turn_phase == TurnPhase::Idle {
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
                    self.complete_streaming_turn();
                    return;
                }
                Ok(StreamEvent::Error(e)) => {
                    self.status_message = format!("❌ Stream error: {}", e);
                    self.streaming_text.clear();
                    self.stream_rx = None;
                    self.turn_phase = TurnPhase::Idle;
                    self.tool_iteration_count = 0;
                    return;
                }
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // Channel closed unexpectedly — save what we have
                    if !self.streaming_text.is_empty() {
                        self.complete_streaming_turn();
                    } else {
                        self.stream_rx = None;
                        self.turn_phase = TurnPhase::Idle;
                        self.tool_iteration_count = 0;
                    }
                    return;
                }
            }
        }
    }

    /// Streaming finished: persist response, check for tool calls, maybe continue
    fn complete_streaming_turn(&mut self) {
        if self.streaming_text.is_empty() {
            self.stream_rx = None;
            self.turn_phase = TurnPhase::Idle;
            self.tool_iteration_count = 0;
            return;
        }

        let response_text = self.streaming_text.clone();

        // Persist assistant message
        let model_msg = Message::new("assistant", &response_text);
        if let Err(e) = self.message_buffer.add_and_persist(model_msg) {
            eprintln!("Failed to save streamed response: {}", e);
            self.status_message = format!("⚠️ Response received but not saved: {}", e);
            self.streaming_text.clear();
            self.stream_rx = None;
            self.turn_phase = TurnPhase::Idle;
            self.tool_iteration_count = 0;
            return;
        }
        self.cached_message_count = self.message_buffer.count()
            .unwrap_or(self.cached_message_count + 1);

        // Check for tool calls in the response
        let tool_calls = agent::parse_tool_calls(&response_text);

        if !tool_calls.is_empty() && self.tool_iteration_count < MAX_TOOL_ITERATIONS {
            // Execute FIRST tool call only (one per turn to avoid dependency issues)
            let call = &tool_calls[0];
            self.status_message = format!("🔧 Executing tool: {} ...", call.name);
            self.execute_tool_async(call.name.clone(), call.args.clone());
            self.turn_phase = TurnPhase::ExecutingTool(call.name.clone());
        } else {
            if self.tool_iteration_count >= MAX_TOOL_ITERATIONS {
                self.status_message = "⚠️ Max tool iterations reached".to_string();
            } else {
                self.status_message = "✅ Response complete".to_string();
            }
            self.turn_phase = TurnPhase::Idle;
            self.tool_iteration_count = 0;
        }

        self.streaming_text.clear();
        self.stream_rx = None;
    }

    /// Spawn tool execution off the UI thread
    fn execute_tool_async(&mut self, tool_name: String, args: String) {
        let (tx, rx) = oneshot::channel();

        tokio::task::spawn_blocking(move || {
            let registry = ToolRegistry::new();
            let result = registry.execute(&tool_name, &args);
            let _ = tx.send(ToolResult {
                tool_name,
                _args: args,
                output: result.map_err(|e| e.to_string()),
            });
        });

        self.tool_result_rx = Some(rx);
    }

    /// Check if async tool execution has completed; if so, persist result and continue
    fn check_tool_result(&mut self) {
        let rx = match self.tool_result_rx.as_mut() {
            Some(rx) => rx,
            None => return,
        };

        match rx.try_recv() {
            Ok(result) => {
                self.tool_result_rx = None;

                let output_text = match &result.output {
                    Ok(output) => {
                        let truncated = if output.len() > 4000 {
                            format!("{}...(truncated)", &output[..4000])
                        } else {
                            output.clone()
                        };
                        format!("[TOOL_OUTPUT: {} = {}]", result.tool_name, truncated)
                    }
                    Err(e) => format!("[TOOL_ERROR: {} = {}]", result.tool_name, e),
                };

                // Persist tool result
                let tool_msg = Message::new("tool", &output_text);
                if let Err(e) = self.message_buffer.add_and_persist(tool_msg) {
                    self.status_message = format!("⚠️ Failed to save tool result: {}", e);
                    self.turn_phase = TurnPhase::Idle;
                    self.tool_iteration_count = 0;
                    return;
                }
                self.cached_message_count = self.message_buffer.count()
                    .unwrap_or(self.cached_message_count + 1);

                // Start next streaming generation with full history including tool result
                self.tool_iteration_count += 1;
                self.status_message = format!(
                    "⏳ Continuing after {} (step {}/{})...",
                    result.tool_name, self.tool_iteration_count, MAX_TOOL_ITERATIONS
                );

                if let Some(client) = &self.ollama_client {
                    let steering_text = self.steering_text();
                    let messages = self.message_buffer.messages().unwrap_or_default();
                    let rx = client.generate_streaming(messages, Some(&steering_text));
                    self.stream_rx = Some(rx);
                    self.streaming_text.clear();
                    self.turn_phase = TurnPhase::Streaming;
                } else {
                    self.status_message = "⚠️ Ollama offline".to_string();
                    self.turn_phase = TurnPhase::Idle;
                    self.tool_iteration_count = 0;
                }
            }
            Err(oneshot::error::TryRecvError::Empty) => {
                // Still waiting for tool execution
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                self.status_message = "❌ Tool execution failed unexpectedly".to_string();
                self.tool_result_rx = None;
                self.turn_phase = TurnPhase::Idle;
                self.tool_iteration_count = 0;
            }
        }
    }

    /// Build the steering system prompt (shared between handle_message and check_tool_result)
    fn steering_text(&self) -> String {
        let steering_directive = SteeringDirective::custom(
            "You are Yggdra. Tools: [TOOL: rg PATTERN PATH], [TOOL: editfile PATH], \
             [TOOL: spawn BIN ARGS], [TOOL: commit MSG], [TOOL: python SCRIPT ARGS], \
             [TOOL: ruste FILE]. Output tools inline. Work offline, be concise."
        );
        steering_directive.format_for_system_prompt()
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
        let connection_status = if self.ollama_client.is_some() { "🦙" } else { "❌" };
        let mode_indicator = match self.mode {
            AppMode::Build => "⚡ Build",
            AppMode::Plan => "🧠 Plan",
        };
        let header_text = match &self.turn_phase {
            TurnPhase::Streaming if self.tool_iteration_count > 0 => {
                format!("🌷 {} | {} | {} | ⏳ Tool follow-up ({}/{})",
                    mode_indicator, connection_status, self.config.model,
                    self.tool_iteration_count, MAX_TOOL_ITERATIONS)
            }
            TurnPhase::Streaming => {
                format!("🌷 {} | {} | {} | ⏳ Streaming...",
                    mode_indicator, connection_status, self.config.model)
            }
            TurnPhase::ExecutingTool(name) => {
                format!("🌷 {} | {} | {} | 🔧 Running {}...",
                    mode_indicator, connection_status, self.config.model, name)
            }
            TurnPhase::Idle => {
                format!("🌷 {} | {} | {}",
                    mode_indicator, connection_status, self.config.model)
            }
        };

        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::BOTTOM).title("Status"));
        f.render_widget(header, chunks[0]);

        // Messages + live streaming text
        let messages_list = self
            .message_buffer
            .messages()
            .unwrap_or_default();
        let mut messages_text: Vec<Line> = messages_list
            .iter()
            .map(|m| {
                match m.role.as_str() {
                    "user" => {
                        Line::from(vec![
                            Span::styled("👤 ", Style::default().fg(Color::Cyan)),
                            Span::raw(&m.content),
                        ])
                    }
                    "assistant" => {
                        Line::from(vec![
                            Span::styled("🤖 ", Style::default().fg(Color::Yellow)),
                            Span::raw(&m.content),
                        ])
                    }
                    "tool" => {
                        Line::from(vec![
                            Span::styled("🔧 ", Style::default().fg(Color::Green)),
                            Span::raw(&m.content),
                        ])
                    }
                    _ => Line::from(vec![
                        Span::styled("💬 ", Style::default().fg(Color::Gray)),
                        Span::raw(&m.content),
                    ]),
                }
            })
            .collect();

        // Show partial streaming response
        if !self.streaming_text.is_empty() {
            messages_text.push(Line::from(vec![
                Span::styled("🤖 ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{}▌", self.streaming_text)),
            ]));
        }

        let output = Paragraph::new(messages_text)
            .block(Block::default().title(" 🌸 Conversation ").borders(Borders::ALL))
            .wrap(ratatui::widgets::Wrap { trim: true });
        f.render_widget(output, chunks[1]);

        // Input area
        let input_hint = match &self.turn_phase {
            TurnPhase::Idle => "(type message or /help for commands)",
            TurnPhase::Streaming => "(streaming response...)",
            TurnPhase::ExecutingTool(_) => "(executing tool...)",
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
            "🔢 {} | 💬 {} | {}",
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
             /help         - Show this help\n\
             /models       - List models\n\
             /plan         - Switch to Plan mode\n\
             /tool CMD     - Execute tool\n\n\
             Modes: ⚡ Build (autonomous) | 🧠 Plan (interactive)\n\n\
             Keybindings: Enter-Submit | Esc-Clear | Ctrl+C-Exit".to_string();
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
    }

    /// Handle /models command
    async fn handle_models_command(&mut self) {
        match &self.ollama_client {
            Some(client) => {
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

        self.turn_phase = TurnPhase::Streaming;
        self.tool_iteration_count = 0;
        self.status_message = "⏳ Streaming response...".to_string();

        let steering_text = self.steering_text();
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

