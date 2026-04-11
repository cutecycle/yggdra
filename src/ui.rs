/// TUI module: minimal terminal UI with multi-window sync via polling
use crate::config::Config;
use crate::message::{Message, MessageBuffer};
use crate::ollama::OllamaClient;
use crate::session::Session;
use crate::steering::SteeringDirective;
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
    last_file_size: u64,
    ollama_client: Option<OllamaClient>,
    waiting_for_response: bool,
}

impl App {
    /// Create new app with optional Ollama client
    pub fn new(
        config: Config,
        session: Session,
        ollama_client: Option<OllamaClient>,
    ) -> Self {
        let message_buffer = MessageBuffer::from_file(&session.messages_file);
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
            last_file_size: 0,
            ollama_client,
            waiting_for_response: false,
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

        // Track last file size for polling
        self.last_file_size = self
            .session
            .messages_file
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);

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
                // Timeout: poll for changes
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
                    Constraint::Length(1),
                    Constraint::Min(3),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ]
                .as_ref(),
            )
            .split(f.area());

        // Header
        let header_text = if self.waiting_for_response {
            "🌷 Yggdra - Airgapped Agent [⏳ waiting...]".to_string()
        } else {
            "🌷 Yggdra - Airgapped Agent".to_string()
        };

        let header = Paragraph::new(header_text).block(Block::default().borders(Borders::BOTTOM));
        f.render_widget(header, chunks[0]);

        // Message output
        let messages_text: Vec<Line> = self
            .message_buffer
            .messages()
            .iter()
            .map(|m| {
                let emoji = if m.role == "user" { "🌷" } else { "🌻" };
                Line::from(format!("{} [{}] {}", emoji, m.role, m.content))
            })
            .collect();

        let output = Paragraph::new(messages_text)
            .block(Block::default().title("Messages").borders(Borders::ALL))
            .wrap(ratatui::widgets::Wrap { trim: true });
        f.render_widget(output, chunks[1]);

        // Input area
        let input = Paragraph::new(format!("> {}", self.input_buffer))
            .block(Block::default().title("Input").borders(Borders::ALL));
        f.render_widget(input, chunks[2]);

        // Status bar - show Ollama connection status, session, model, message count
        let ollama_status = if self.ollama_client.is_some() {
            "✅ Ollama"
        } else {
            "❌ Offline"
        };
        let status = format!(
            "{} | Session: {} | Model: {} | Msgs: {} | Ctrl+C to exit",
            ollama_status,
            &self.session.id[..8],
            self.config.model,
            self.message_buffer.messages().len()
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

        if command == "/models" {
            self.handle_models_command().await;
        } else if command == "/help" {
            self.status_message =
                "Commands: /models (list models), /help (show this), or type a message".to_string();
        } else if command.starts_with('/') {
            self.status_message = format!("Unknown command: {}. Type /help for help.", command);
        } else if !command.is_empty() {
            self.handle_message(&command).await;
        }

        self.input_buffer.clear();
    }

    /// Handle /models command
    async fn handle_models_command(&mut self) {
        match &self.ollama_client {
            Some(client) => {
                match client.list_models().await {
                    Ok(models) => {
                        let mut output = "🌻 Available Models:\n".to_string();
                        for model in &models {
                            output.push_str(&format!("• {}\n", model.name));
                        }
                        self.status_message = output;
                        eprintln!("Models: {:?}", models);
                    }
                    Err(e) => {
                        self.status_message = format!("🌹 Failed to fetch models: {}", e);
                        eprintln!("Error listing models: {}", e);
                    }
                }
            }
            None => {
                self.status_message = "🌹 Ollama not connected".to_string();
            }
        }
    }

    /// Handle user message - send to Ollama with steering
    async fn handle_message(&mut self, message: &str) {
        // Save user message
        let user_msg = Message::new("user", message.to_string());
        if let Err(e) = self.message_buffer.add_and_persist(user_msg, &self.session.messages_file) {
            eprintln!("Failed to save user message: {}", e);
            self.status_message = "🌹 Failed to save message".to_string();
            return;
        }

        // If no Ollama client, just display message
        if self.ollama_client.is_none() {
            self.status_message = "🌹 Ollama not connected".to_string();
            return;
        }

        self.waiting_for_response = true;
        self.status_message = "⏳ Waiting for response...".to_string();

        // Prepare messages for Ollama - convert message buffer to Message vec
        let messages_for_ollama: Vec<Message> = self
            .message_buffer
            .messages()
            .iter()
            .cloned()
            .collect();

        // Apply steering directive
        let steering_directive = SteeringDirective::custom("Be concise and helpful");
        let steering_text = steering_directive.format_for_system_prompt();

        // Send to Ollama
        if let Some(client) = &self.ollama_client {
            match client
                .generate(messages_for_ollama, Some(&steering_text))
                .await
            {
                Ok(response) => {
                    let model_msg = Message::new("assistant", response.clone());
                    if let Err(e) = self
                        .message_buffer
                        .add_and_persist(model_msg, &self.session.messages_file)
                    {
                        eprintln!("Failed to save model response: {}", e);
                        self.status_message = "🌹 Failed to save response".to_string();
                    } else {
                        self.status_message = "🌻 Model responded".to_string();
                        crate::notifications::model_responded(&response[..std::cmp::min(100, response.len())]).await;
                    }
                }
                Err(e) => {
                    eprintln!("Error sending message to Ollama: {}", e);
                    self.status_message = format!("🌹 Error: {}", e);
                    crate::notifications::error_occurred(&format!("Ollama error: {}", e)).await;
                }
            }
        }

        self.waiting_for_response = false;
    }

    /// Poll for file changes
    fn poll_for_updates(&mut self) -> Result<()> {
        if let Ok(metadata) = self.session.messages_file.metadata() {
            let new_size = metadata.len();
            if new_size > self.last_file_size {
                // File grew - reload messages
                self.message_buffer = MessageBuffer::from_file(&self.session.messages_file);
                self.last_file_size = new_size;
            }
        }
        Ok(())
    }
}

