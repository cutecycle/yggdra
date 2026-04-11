/// TUI module: minimal terminal UI with streaming responses, tool execution, and multi-window sync
use crate::agent;
use crate::config::Config;
use crate::message::{Message, MessageBuffer};
use crate::ollama::{OllamaClient, StreamEvent};
use crate::session::Session;
use crate::steering::SteeringDirective;
use crate::task::{TaskManager, Checkpoint};
use crate::tools::ToolRegistry;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, EnableMouseCapture, DisableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

type _TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

const MAX_TOOL_ITERATIONS: usize = 10;

/// A command that can be invoked from the palette
struct PaletteCommand {
    /// The slash command text (without leading /)
    name: &'static str,
    /// Short description shown in palette
    description: &'static str,
    /// Extra keywords for fuzzy matching (space-separated)
    keywords: &'static str,
    /// What to fill into the input when selected
    fill: &'static str,
}

const PALETTE_COMMANDS: &[PaletteCommand] = &[
    PaletteCommand { name: "help",   description: "Show commands & keybindings",       keywords: "commands keyboard shortcuts guide", fill: "/help" },
    PaletteCommand { name: "models", description: "List available Ollama models",       keywords: "list llm ollama choose switch",     fill: "/models" },
    PaletteCommand { name: "checkpoint", description: "Save session checkpoint",        keywords: "save progress milestone snapshot",   fill: "/checkpoint " },
    PaletteCommand { name: "clear",  description: "Archive conversation to scrollback", keywords: "clear buffer reset history archive", fill: "/clear" },
    PaletteCommand { name: "mem",    description: "Search archived scrollback",         keywords: "search memory past conversation",    fill: "/tool mem " },
    PaletteCommand { name: "tasks",  description: "Show task dependency graph",         keywords: "task deps dependencies adjacency",   fill: "/tasks" },
    PaletteCommand { name: "gaps",   description: "Show recorded knowledge gaps",        keywords: "knowledge gap unknown missing info",  fill: "/gaps" },
    PaletteCommand { name: "plan",   description: "Switch to interactive Plan mode",    keywords: "interactive manual control",        fill: "/plan" },
    PaletteCommand { name: "tool rg",    description: "Search files with ripgrep",      keywords: "search grep find file text",        fill: "/tool rg " },
    PaletteCommand { name: "tool editfile", description: "Read file contents",          keywords: "read open cat file view",           fill: "/tool editfile " },
    PaletteCommand { name: "tool spawn",    description: "Execute a local binary",      keywords: "run exec spawn binary program",     fill: "/tool spawn " },
    PaletteCommand { name: "tool commit",   description: "Git commit with message",     keywords: "git save version commit history",   fill: "/tool commit " },
    PaletteCommand { name: "tool python",   description: "Run a Python script",         keywords: "python py script run execute",      fill: "/tool python " },
    PaletteCommand { name: "tool ruste",    description: "Compile and run Rust code",   keywords: "rust compile execute cargo rustc",  fill: "/tool ruste " },
];

/// Fuzzy match: returns a score > 0 if all query chars appear in target in order.
/// Higher score = better match (chars close together and near the start).
fn fuzzy_score(query: &str, target: &str) -> i32 {
    if query.is_empty() {
        return 1;
    }
    let query = query.to_lowercase();
    let target = target.to_lowercase();
    let mut qi = query.chars().peekable();
    let mut last_match = 0usize;
    let mut score = 100i32;

    for (ti, tc) in target.chars().enumerate() {
        if let Some(&qc) = qi.peek() {
            if tc == qc {
                if ti > 0 {
                    score -= (ti - last_match) as i32; // penalise gaps
                }
                last_match = ti;
                qi.next();
            }
        } else {
            break;
        }
    }

    if qi.peek().is_none() { score } else { 0 } // 0 = no match
}

/// RAII guard to restore terminal state on drop (including panics/errors)
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
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
    task_manager: TaskManager,
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
    /// Receives result from async gap query
    gap_rx: Option<oneshot::Receiver<Option<crate::gaps::Gap>>>,
    /// Async writer for .yggdra/log hierarchy
    log_sender: Option<crate::msglog::LogSender>,
    /// Root of the .yggdra/log directory for searches
    log_dir: std::path::PathBuf,
    /// Build (autonomous) vs Plan (interactive) mode
    mode: AppMode,
    /// Contents of AGENTS.md — injected into steering context
    agents_context: Option<String>,
    /// Whether the command palette is open
    palette_open: bool,
    /// Which palette item is highlighted
    palette_selection: usize,
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
        let task_manager = TaskManager::from_db(&session.tasks_db)
            .unwrap_or_else(|e| {
                eprintln!("🌹 Failed to open tasks DB: {}", e);
                TaskManager::new(&session.tasks_db)
                    .expect("Cannot create task database")
            });
        let status_message = if ollama_client.is_some() {
            "✅ Ollama connected".to_string()
        } else {
            "❌ Ollama offline".to_string()
        };

        // Start async log writer: .yggdra/log/ in cwd
        let log_dir = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".yggdra")
            .join("log");
        let log_sender = Some(crate::msglog::start(log_dir.clone()));

        Self {
            config,
            session,
            input_buffer: String::new(),
            status_message,
            running: true,
            message_buffer,
            task_manager,
            ollama_client,
            tool_registry: ToolRegistry::new(),
            cached_message_count: 0,
            streaming_text: String::new(),
            stream_rx: None,
            turn_phase: TurnPhase::Idle,
            tool_iteration_count: 0,
            tool_result_rx: None,
            gap_rx: None,
            log_sender,
            log_dir,
            mode: AppMode::Build,
            agents_context: agents_md,
            palette_open: false,
            palette_selection: 0,
        }
    }

    /// Run the TUI — main event loop with streaming support
    pub async fn run(&mut self) -> Result<()> {
        let _guard = TerminalGuard::new()?;

        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        // In Build mode, fire kick prompt to start autonomous loop
        if self.mode == AppMode::Build && self.agents_context.is_some() {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".to_string());
            let kick = format!(
                "New session started in `{cwd}`. \
                 Orient yourself: list the directory, review any existing tasks, \
                 and begin working autonomously. \
                 Use tools to explore. Say [DONE] only when fully complete."
            );
            self.handle_message(&kick).await;
        }

        loop {
            // Drain any pending stream tokens before drawing
            self.drain_stream_tokens();
            // Check for completed tool execution
            self.check_tool_result();
            // Check for completed gap reflection
            self.check_gap_result();

            terminal.draw(|f| self.draw(f))?;

            // Fast poll: 10ms when streaming (responsive to scroll), 200ms when idle
            let poll_ms = if self.turn_phase == TurnPhase::Idle { 200 } else { 10 };

            if crossterm::event::poll(Duration::from_millis(poll_ms))? {
                match event::read()? {
                    Event::Key(key) => {
                        self.handle_key(key).await;
                        if !self.running {
                            break;
                        }
                    }
                    _ => {}
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

        // Fire-and-forget gap reflection: ask the model what it wished it knew
        if let Some(client) = self.ollama_client.clone() {
            let model = self.config.model.clone();
            let text = response_text.clone();
            let (tx, rx) = oneshot::channel();
            tokio::spawn(async move {
                let result = crate::gaps::query_gap(&client, &model, &text).await;
                let _ = tx.send(result.unwrap_or(None));
            });
            self.gap_rx = Some(rx);
        }

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

    /// Check if the async gap reflection query has completed; record and surface if so
    fn check_gap_result(&mut self) {
        let rx = match self.gap_rx.as_mut() {
            Some(rx) => rx,
            None => return,
        };

        match rx.try_recv() {
            Ok(Some(gap)) => {
                self.gap_rx = None;
                if let Err(e) = crate::gaps::record_gap(&gap) {
                    eprintln!("Failed to record gap: {}", e);
                } else {
                    self.push_system_event(format!("ℹ️  I wish I knew: {}", gap.content));
                }
            }
            Ok(None) => {
                // Model reported no gap — nothing to do
                self.gap_rx = None;
            }
            Err(oneshot::error::TryRecvError::Empty) => {
                // Still waiting
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                self.gap_rx = None;
            }
        }
    }

    /// Build the steering system prompt (shared between handle_message and check_tool_result)
    fn steering_text(&self) -> String {
        let os = std::env::consts::OS;
        let term_width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
        let mut base = format!(
            "ASSISTANT is yggdra, a terminal ai agent. OS: {os}. Terminal: {term_width} cols.\n\
             Tools: [TOOL: rg PATTERN PATH], [TOOL: editfile PATH], [TOOL: spawn BINARY ARGS], \
             [TOOL: commit MSG], [TOOL: python SCRIPT ARGS], [TOOL: ruste FILE].\n\
             Examples: [TOOL: spawn ls -la .] or [TOOL: rg TODO src/] or [TOOL: editfile Cargo.toml].\n\
             Use tools proactively. Do not say you cannot access files—use [TOOL: spawn ls] instead. Be concise."
        );
        if let Some(ctx) = &self.agents_context {
            base.push_str("\n\n--- AGENTS.md ---\n");
            base.push_str(ctx);
        }
        SteeringDirective::custom(&base).format_for_system_prompt()
    }

    /// Push a system-level notice into the conversation (compaction, warnings, etc.)
    fn push_system_event(&mut self, text: impl Into<String>) {
        let msg = Message::new("system", text);
        self.persist_message(msg);
        self.cached_message_count = self.message_buffer.messages()
            .map(|v| v.len()).unwrap_or(0);
    }

    /// Persist a message to SQLite and asynchronously write it to .yggdra/log
    fn persist_message(&mut self, msg: Message) -> bool {
        if let Some(sender) = &self.log_sender as &Option<crate::msglog::LogSender> {
            sender.log(&msg);
        }
        self.message_buffer.add_and_persist(msg).is_ok()
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

        // Header with context window indicator
        let connection_status = if self.ollama_client.is_some() { "🦙" } else { "❌" };
        let mode_indicator = match self.mode {
            AppMode::Build => "⚡ Build",
            AppMode::Plan => "🧠 Plan",
        };
        
        // Estimate context window usage (rough: message count affects token count)
        let message_count = self.cached_message_count as f64;
        let estimated_tokens = message_count * 150.0; // rough avg tokens per message
        let context_window = 4096.0; // for qwen:3.5-chat typical
        let usage_percent = (estimated_tokens / context_window * 100.0).min(100.0) as u32;
        let context_indicator = if usage_percent > 70 {
            format!("🔴 {}%", usage_percent)
        } else if usage_percent > 50 {
            format!("🟡 {}%", usage_percent)
        } else {
            format!("🟢 {}%", usage_percent)
        };
        
        let header_text = format!("🌷 {} | {} | {} | {}", 
            mode_indicator, connection_status, self.config.model, context_indicator);

        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::BOTTOM).title("Status"));
        f.render_widget(header, chunks[0]);

        // Messages area with full-width colored bands
        let messages_area = chunks[1];
        let messages_list = self
            .message_buffer
            .messages()
            .unwrap_or_default();

        // Render each message as its own Block with full-width background
        let mut exchange_idx: usize = 0;
        let mut current_y = messages_area.top();
        
        for msg in messages_list.iter() {
            let (emoji, _fg_color, bg_tint, show_band) = match msg.role.as_str() {
                "user" => {
                    exchange_idx += 1;
                    let tint = if exchange_idx % 2 == 0 {
                        Color::Rgb(30, 30, 45)   // dark blue
                    } else {
                        Color::Rgb(20, 35, 20)   // dark green
                    };
                    ("👤", Color::Cyan, Some(tint), true)
                }
                "assistant" => {
                    exchange_idx += 1;
                    let tint = if exchange_idx % 2 == 0 {
                        Color::Rgb(30, 30, 45)
                    } else {
                        Color::Rgb(20, 35, 20)
                    };
                    ("🤖", Color::Yellow, Some(tint), true)
                }
                "tool" => ("🔧", Color::Green, None, false),
                "system" => ("⚙️", Color::Rgb(180, 120, 0), None, false),
                _ => ("💬", Color::Gray, None, false),
            };

            let text_content = format!("{} {}", emoji, self.format_message_content(&msg.content));
            let msg_para = Paragraph::new(text_content)
                .wrap(ratatui::widgets::Wrap { trim: true });

            let msg_para = if show_band {
                msg_para.style(Style::default().fg(Color::White).bg(bg_tint.unwrap()))
            } else {
                msg_para
            };

            // Estimate height: count newlines, assume ~60% fill width due to wrapping
            let estimated_lines = (msg.content.len() / 50).max(msg.content.lines().count());
            let msg_height = (estimated_lines as u16).min(messages_area.bottom().saturating_sub(current_y));
            
            if current_y >= messages_area.bottom() {
                break; // No more space
            }

            let msg_area = Rect {
                x: messages_area.x,
                y: current_y,
                width: messages_area.width,
                height: msg_height,
            };

            f.render_widget(&msg_para, msg_area);
            current_y += msg_height;
        }

        // Show streaming response in a colored block
        if !self.streaming_text.is_empty() && current_y < messages_area.bottom() {
            let tint = if exchange_idx % 2 == 0 {
                Color::Rgb(30, 30, 45)
            } else {
                Color::Rgb(20, 35, 20)
            };
            let stream_text = format!("🤖 {}▌", self.streaming_text);
            let stream_para = Paragraph::new(stream_text)
                .style(Style::default().fg(Color::White).bg(tint))
                .wrap(ratatui::widgets::Wrap { trim: true });
            
            let stream_height = 1u16.min(messages_area.bottom() - current_y);
            let stream_area = Rect {
                x: messages_area.x,
                y: current_y,
                width: messages_area.width,
                height: stream_height,
            };
            f.render_widget(stream_para, stream_area);
        }

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

        // Command palette overlay (above input box)
        if self.palette_open {
            let matches = self.palette_matches();
            if !matches.is_empty() {
                let palette_height = (matches.len().min(8) + 2) as u16;
                let area = chunks[2];
                // Float palette just above the input box
                let palette_rect = Rect {
                    x: area.x,
                    y: area.y.saturating_sub(palette_height),
                    width: area.width.min(60),
                    height: palette_height,
                };
                let items: Vec<ListItem> = matches
                    .iter()
                    .enumerate()
                    .map(|(i, cmd)| {
                        let line = Line::from(vec![
                            Span::styled(
                                format!(" /{:<16}", cmd.name),
                                if i == self.palette_selection {
                                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(Color::Cyan)
                                },
                            ),
                            Span::styled(
                                format!(" {}", cmd.description),
                                if i == self.palette_selection {
                                    Style::default().fg(Color::Black).bg(Color::Cyan)
                                } else {
                                    Style::default().fg(Color::Gray)
                                },
                            ),
                        ]);
                        ListItem::new(line)
                    })
                    .collect();
                let palette = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(" 🌼 Commands "));
                f.render_widget(Clear, palette_rect);
                f.render_widget(palette, palette_rect);
            }
        }

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
            // Open palette on '/'
            KeyCode::Char('/') if self.input_buffer.is_empty() => {
                self.input_buffer.push('/');
                self.palette_open = true;
                self.palette_selection = 0;
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
                if self.palette_open {
                    self.palette_selection = 0; // reset selection on new char
                }
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                if self.palette_open && (self.input_buffer.is_empty() || !self.input_buffer.starts_with('/')) {
                    self.palette_open = false;
                }
            }
            KeyCode::Down if self.palette_open => {
                let count = self.palette_matches().len();
                if count > 0 {
                    self.palette_selection = (self.palette_selection + 1) % count;
                }
            }
            KeyCode::Up if self.palette_open => {
                let count = self.palette_matches().len();
                if count > 0 {
                    self.palette_selection = self.palette_selection.saturating_sub(1);
                }
            }
            KeyCode::Tab if self.palette_open => {
                // Tab completes the highlighted item
                let matches = self.palette_matches();
                if let Some(&cmd) = matches.get(self.palette_selection) {
                    self.input_buffer = cmd.fill.to_string();
                    self.palette_open = false;
                }
            }
            KeyCode::Enter if self.palette_open => {
                let matches = self.palette_matches();
                if let Some(&cmd) = matches.get(self.palette_selection) {
                    self.input_buffer = cmd.fill.to_string();
                    self.palette_open = false;
                    // Only submit immediately if fill has no trailing space (i.e. doesn't need args)
                    if !cmd.fill.ends_with(' ') {
                        self.handle_command().await;
                    }
                } else {
                    self.palette_open = false;
                    self.handle_command().await;
                }
            }
            KeyCode::Enter => {
                self.handle_command().await;
            }
            KeyCode::Esc => {
                if self.palette_open {
                    self.palette_open = false;
                    self.input_buffer.clear();
                } else {
                    self.input_buffer.clear();
                }
            }
            _ => {}
        }
    }

    /// Return palette commands matching the current query, scored by relevance
    fn palette_matches(&self) -> Vec<&'static PaletteCommand> {
        let query = self.input_buffer.trim_start_matches('/');
        let mut scored: Vec<(i32, &'static PaletteCommand)> = PALETTE_COMMANDS
            .iter()
            .filter_map(|cmd| {
                let haystack = format!("{} {} {}", cmd.name, cmd.description, cmd.keywords);
                let s = fuzzy_score(query, &haystack);
                if s > 0 { Some((s, cmd)) } else { None }
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, c)| c).collect()
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
        } else if command == "/clear" {
            self.handle_clear_command();
        } else if command == "/tasks" {
            self.handle_tasks_command();
        } else if command == "/gaps" {
            self.handle_gaps_command();
        } else if command.starts_with("/checkpoint") {
            let name = command.strip_prefix("/checkpoint ").unwrap_or("").trim();
            self.handle_checkpoint_command(if name.is_empty() { None } else { Some(name) });
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

        // Handle special "mem" tool for searching scrollback
        if tool_name == "mem" {
            self.handle_mem_command(args);
            return;
        }

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
                            self.push_system_event("ℹ️ No models found. Run: ollama pull <model>");
                        } else {
                            let list = models.iter()
                                .map(|m| {
                                    let size = m.size
                                        .map(|b| format!(" ({:.1}GB)", b as f64 / 1_073_741_824.0))
                                        .unwrap_or_default();
                                    format!("  🌸 {}{}", m.name, size)
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            self.push_system_event(format!("📦 Models:\n{}", list));
                        }
                    }
                    Ok(Err(e)) => {
                        let msg = if e.to_string().contains("connection refused") {
                            "🦙 Ollama not running — start with: ollama serve".to_string()
                        } else {
                            format!("❌ Models fetch failed: {}", self.friendly_error(&e.to_string()))
                        };
                        self.push_system_event(msg);
                    }
                    Err(_) => {
                        self.push_system_event("❌ Model fetch timed out — Ollama not responding");
                    }
                }
            }
            None => {
                self.push_system_event("⚠️ Ollama not connected");
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

        // Warn when context window is getting full (>70% threshold)
        let estimated_usage = (self.cached_message_count as f64 * 150.0 / 4096.0 * 100.0) as u32;
        if estimated_usage >= 70 {
            self.push_system_event(format!("⚠️ Context ~{}% full — autocompact may trigger soon", estimated_usage));
        }

        if self.ollama_client.is_none() {
            self.push_system_event("🦙 Ollama offline: message saved but not sent");
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

    /// Format message content with nice code block indentation and language detection
    fn format_message_content(&self, content: &str) -> String {
        let mut result = String::new();
        let mut in_code_block = false;
        let mut code_language = String::new();

        for line in content.lines() {
            // Detect code block markers (```language)
            if line.trim_start().starts_with("```") {
                if !in_code_block {
                    // Start of code block: extract language and add visual marker
                    let lang_part = line.trim_start().strip_prefix("```").unwrap_or("").trim();
                    code_language = if lang_part.is_empty() {
                        "code".to_string()
                    } else {
                        lang_part.to_string()
                    };
                    result.push_str(&format!("┌─ {}\n", code_language));
                    in_code_block = true;
                } else {
                    // End of code block
                    result.push_str("└─\n");
                    in_code_block = false;
                    code_language.clear();
                }
                continue;
            }

            if in_code_block {
                // Add code line with indentation (4 spaces + border)
                result.push_str("│   ");
                result.push_str(line);
            } else if line.starts_with("    ") || line.starts_with("\t") {
                // Detect indented lines as code-like (indent further)
                result.push_str("    ");
                result.push_str(line);
            } else {
                result.push_str(line);
            }
            result.push('\n');
        }

        // Trim trailing newline
        if result.ends_with('\n') {
            result.pop();
        }
        result
    }

    /// Handle /checkpoint command to save session progress
    fn handle_checkpoint_command(&mut self, name_opt: Option<&str>) {
        let checkpoint_name = name_opt
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Checkpoint at {}", chrono::Local::now().format("%H:%M:%S")));

        match self.task_manager.checkpoint(&checkpoint_name) {
            Ok(_) => {
                let summary = format!("📍 Checkpoint '{}' saved", checkpoint_name);
                self.push_system_event(&summary);
                self.status_message = summary;
            }
            Err(e) => {
                self.status_message = format!("❌ Checkpoint failed: {}", e);
            }
        }
    }

    /// Handle /clear command to archive current messages to scrollback
    fn handle_clear_command(&mut self) {
        match self.message_buffer.archive_to_scrollback() {
            Ok(count) => {
                let summary = format!("🗑️  Archived {} messages to scrollback", count);
                self.push_system_event(&summary);
                self.status_message = summary;
                self.cached_message_count = 0;
            }
            Err(e) => {
                self.status_message = format!("❌ Clear failed: {}", e);
            }
        }
    }

    /// Handle /tool mem command — searches .yggdra/log (the single source of truth)
    fn handle_mem_command(&mut self, query: &str) {
        if query.is_empty() {
            self.status_message = "❌ mem: search query required. Usage: /tool mem QUERY".to_string();
            return;
        }

        let log_dir = self.log_dir.clone();
        let results = crate::msglog::search_log(&log_dir, query, 10);

        if results.is_empty() {
            self.status_message = format!("🔍 No results for '{}' in .yggdra/log", query);
            return;
        }

        let mut output = format!("🔍 Search results for '{}' ({} found):\n\n", query, results.len());
        for m in results.iter().take(5) {
            // Show partition path relative to log_dir for context (e.g. 2026/04/11/0936)
            let rel = m.path.strip_prefix(&log_dir).unwrap_or(&m.path);
            output.push_str(&format!("**{}** ({})\n{}\n\n", m.role.to_uppercase(), rel.display(), m.excerpt));
        }

        let mem_msg = Message::new("tool", output);
        if let Err(e) = self.message_buffer.add_and_persist(mem_msg) {
            self.status_message = format!("❌ Failed to save search results: {}", e);
        } else {
            self.status_message = format!("🔍 {} results for '{}'", results.len(), query);
        }
    }

    /// Handle /tasks command to show task dependency graph
    fn handle_tasks_command(&mut self) {
        match self.task_manager.list_all_tasks() {
            Ok(tasks) => {
                if tasks.is_empty() {
                    self.status_message = "📋 No tasks defined yet".to_string();
                    return;
                }

                // Build adjacency list from dependencies
                let mut adjacency: std::collections::HashMap<String, Vec<String>> = 
                    std::collections::HashMap::new();
                
                // Initialize all tasks with empty dependency lists
                for task in &tasks {
                    adjacency.entry(task.id.clone()).or_insert_with(Vec::new);
                }

                // Add dependencies
                if let Ok(deps) = self.task_manager.get_all_dependencies() {
                    for (task_id, depends_on) in deps {
                        adjacency
                            .entry(task_id)
                            .or_insert_with(Vec::new)
                            .push(depends_on);
                    }
                }

                // Format as directed adjacency list: task -> dep1, dep2, ...
                let mut output = String::from("📊 Task Dependency Graph (DAG):\n\n");
                let mut sorted_tasks: Vec<&String> = adjacency.keys().collect();
                sorted_tasks.sort();

                for task_id in sorted_tasks {
                    if let Some(deps) = adjacency.get(task_id) {
                        if deps.is_empty() {
                            output.push_str(&format!("  {} →\n", task_id));
                        } else {
                            let deps_str = deps.join(", ");
                            output.push_str(&format!("  {} → {}\n", task_id, deps_str));
                        }
                    }
                }

                let tasks_msg = Message::new("tool", output);
                if let Err(e) = self.message_buffer.add_and_persist(tasks_msg) {
                    self.status_message = format!("❌ Failed to save tasks: {}", e);
                } else {
                    self.cached_message_count = self.message_buffer.messages()
                        .map(|v| v.len()).unwrap_or(0);
                    self.status_message = format!("📋 {} tasks", tasks.len());
                }
            }
            Err(e) => {
                self.status_message = format!("❌ Failed to list tasks: {}", e);
            }
        }
    }

    /// Handle /gaps command to show recorded knowledge gaps
    fn handle_gaps_command(&mut self) {
        match crate::gaps::load_gaps() {
            Ok(lines) => {
                if lines.is_empty() {
                    self.status_message = "ℹ️  No knowledge gaps recorded yet".to_string();
                    return;
                }

                let mut output = format!("ℹ️  Knowledge Gaps ({} recorded):\n\n", lines.len());
                for line in &lines {
                    output.push_str(&format!("  {}\n", line));
                }

                let gaps_msg = Message::new("tool", output);
                if let Err(e) = self.message_buffer.add_and_persist(gaps_msg) {
                    self.status_message = format!("❌ Failed to display gaps: {}", e);
                } else {
                    self.cached_message_count = self.message_buffer.messages()
                        .map(|v| v.len()).unwrap_or(0);
                    self.status_message = format!("ℹ️  {} gaps", lines.len());
                }
            }
            Err(e) => {
                self.status_message = format!("❌ Failed to load gaps: {}", e);
            }
        }
    }
}

