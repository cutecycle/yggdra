/// TUI module: minimal terminal UI with multi-window sync via polling
use crate::config::Config;
use crate::message::{Message, MessageBuffer};
use crate::session::Session;
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
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

type _TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Minimal TUI application
pub struct App {
    config: Config,
    session: Session,
    input_buffer: String,
    output_buffer: String,
    running: bool,
    message_buffer: MessageBuffer,
    last_file_size: u64,
}

impl App {
    /// Create new app
    pub fn new(config: Config, session: Session) -> Self {
        let message_buffer = MessageBuffer::from_file(&session.messages_file);
        Self {
            config,
            session,
            input_buffer: String::new(),
            output_buffer: String::new(),
            running: true,
            message_buffer,
            last_file_size: 0,
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

        // Main loop
        let running = Arc::new(AtomicBool::new(true));
        let _running_clone = running.clone();

        loop {
            // Draw UI
            terminal.draw(|f| self.draw(f))?;

            // Handle events with timeout
            if crossterm::event::poll(Duration::from_millis(500))? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key);
                    if !self.running {
                        break;
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
        let header = Paragraph::new("🌷 Yggdra - Airgapped Agent")
            .block(Block::default().borders(Borders::BOTTOM));
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

        // Status bar
        let status = format!(
            "Session: {} | Model: {} | {}/? | Ctrl+C to exit",
            &self.session.id[..8],
            self.config.model,
            self.message_buffer.messages().len()
        );
        let status_bar = Paragraph::new(status);
        f.render_widget(status_bar, chunks[3]);
    }

    /// Handle keyboard input
    fn handle_key(&mut self, key: KeyEvent) {
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
                self.handle_command();
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
            }
            _ => {}
        }
    }

    /// Handle command submission
    fn handle_command(&mut self) {
        let command = self.input_buffer.trim();

        if command == "/models" {
            self.output_buffer = "📋 Available models (stub):\n  - qwen:3.5".to_string();
        } else if !command.is_empty() {
            // Save user message
            let msg = Message::new("user", command.to_string());
            let _ = msg.to_jsonl();
            eprintln!("User: {}", command);
            self.message_buffer
                .add_and_persist(msg, &self.session.messages_file)
                .ok();
        }

        self.input_buffer.clear();
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
