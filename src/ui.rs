/// TUI module: handles the terminal user interface using ratatui
use crate::message::MessageBuffer;
use crate::session::{SessionManager, SessionMetadata, SessionMode};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use serde_json::json;

/// Type alias for the terminal type used in this app
type TuiTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

/// TUI application state
pub struct TuiApp {
    session: SessionMetadata,
    message_buffer: MessageBuffer,
    input_buffer: String,
    output_buffer: String,
    mode: SessionMode,
    running: bool,
    needs_save: bool,
}

impl TuiApp {
    /// Create a new TUI application
    pub fn new(session: SessionMetadata, message_buffer: MessageBuffer) -> Self {
        let mode = session.mode;
        Self {
            session,
            message_buffer,
            input_buffer: String::new(),
            output_buffer: String::new(),
            mode,
            running: true,
            needs_save: false,
        }
    }

    /// Handle keyboard events
    fn handle_input(&mut self, key: KeyEvent) -> Result<()> {
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                self.running = false;
            }
            (KeyModifiers::NONE, KeyCode::Tab) => {
                self.mode = match self.mode {
                    SessionMode::Plan => SessionMode::Build,
                    SessionMode::Build => SessionMode::Plan,
                };
                self.output_buffer
                    .push_str(&format!("🌷 Switched to {} mode\n", self.mode));
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                self.send_message()?;
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.input_buffer.pop();
            }
            (KeyModifiers::NONE, KeyCode::Char(c)) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
        Ok(())
    }

    /// Send a message and get a placeholder response
    fn send_message(&mut self) -> Result<()> {
        if self.input_buffer.trim().is_empty() {
            return Ok(());
        }

        let user_message = self.input_buffer.clone();

        // Add user message to buffer
        self.message_buffer
            .add_message("user", user_message.clone());

        // Add to output
        self.output_buffer
            .push_str(&format!("🌷 You: {}\n", user_message));

        // Create message entry for JSONL
        let message_json = json!({
            "role": "user",
            "content": user_message,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "token_count": (user_message.len() as f32 / 4.0).ceil() as u32
        });

        // Save to session
        SessionManager::append_message(&self.session.id, &message_json)?;

        // Placeholder agent response
        let agent_response = "🌷 Placeholder agent output: processing your message...";
        self.message_buffer.add_message("assistant", agent_response);

        self.output_buffer
            .push_str(&format!("🌻 Agent: {}\n", agent_response));

        let response_json = json!({
            "role": "assistant",
            "content": agent_response,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "token_count": (agent_response.len() as f32 / 4.0).ceil() as u32
        });

        SessionManager::append_message(&self.session.id, &response_json)?;

        // Check compression warning
        if self.message_buffer.needs_compression() {
            self.output_buffer.push_str(&format!(
                "🌹 Warning: Context usage at {:.1}%\n",
                self.message_buffer.context_usage_percent()
            ));
        }

        self.input_buffer.clear();
        self.needs_save = true;

        Ok(())
    }

    /// Render the TUI
    fn render(&self, f: &mut Frame) {
        let size = f.area();

        // Create main layout: header | main | status
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(3),
            ])
            .split(size);

        // Header
        self.render_header(f, chunks[0]);

        // Main content area: split into output and input
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(4)])
            .split(chunks[1]);

        self.render_output(f, main_chunks[0]);
        self.render_input(f, main_chunks[1]);

        // Status bar
        self.render_status(f, chunks[2]);
    }

    /// Render header with mode indicator
    fn render_header(&self, f: &mut Frame, area: Rect) {
        let title = format!("🌷 Yggdra - {} Mode", self.mode);
        let header = Paragraph::new(title)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::BOTTOM));
        f.render_widget(header, area);
    }

    /// Render output area
    fn render_output(&self, f: &mut Frame, area: Rect) {
        let output = Paragraph::new(self.output_buffer.as_str())
            .block(Block::default().title("🌻 Output").borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        f.render_widget(output, area);
    }

    /// Render input area
    fn render_input(&self, f: &mut Frame, area: Rect) {
        let lines = vec![Line::from(vec![
            Span::raw("🌷 > "),
            Span::raw(self.input_buffer.as_str()),
        ])];
        let input = Paragraph::new(lines)
            .block(Block::default().title("Input").borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        f.render_widget(input, area);
    }

    /// Render status bar
    fn render_status(&self, f: &mut Frame, area: Rect) {
        let context_percent = self.message_buffer.context_usage_percent();
        let status = format!(
            " Mode: {} | Session: {} | Context: {:.1}% | Battery: --% ",
            self.mode,
            &self.session.id[..8],
            context_percent
        );
        let status_bar = Paragraph::new(status)
            .block(Block::default().borders(Borders::TOP))
            .alignment(Alignment::Left);
        f.render_widget(status_bar, area);
    }
}

/// Run the TUI event loop
pub async fn run_tui(session: SessionMetadata, message_buffer: MessageBuffer) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new(session, message_buffer);

    loop {
        terminal.draw(|f| app.render(f))?;

        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.handle_input(key)?;
            }
        }

        if !app.running {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
    )?;

    Ok(())
}
