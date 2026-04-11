/// TUI module: minimal terminal UI with streaming responses, tool execution, and multi-window sync
use crate::agent;
use crate::config::Config;
use crate::message::{Message, MessageBuffer};
use crate::ollama::{OllamaClient, StreamEvent};
use crate::session::Session;
use crate::steering::SteeringDirective;
use crate::task::{TaskManager, Checkpoint};
use crate::theme::Theme;
use crate::tools::ToolRegistry;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, MouseEvent, MouseEventKind, EnableMouseCapture, DisableMouseCapture},
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

const MAX_TOOL_ITERATIONS: usize = 30;

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
    PaletteCommand { name: "shell",  description: "Run a shell command inline",          keywords: "run exec shell bash command terminal", fill: "/shell " },
    PaletteCommand { name: "help",   description: "Show commands & keybindings",       keywords: "commands keyboard shortcuts guide", fill: "/help" },
    PaletteCommand { name: "models", description: "List available Ollama models",       keywords: "list llm ollama choose switch",     fill: "/models" },
    PaletteCommand { name: "checkpoint", description: "Save session checkpoint",        keywords: "save progress milestone snapshot",   fill: "/checkpoint " },
    PaletteCommand { name: "clear",  description: "Archive conversation to scrollback", keywords: "clear buffer reset history archive", fill: "/clear" },
    PaletteCommand { name: "mem",    description: "Search archived scrollback",         keywords: "search memory past conversation",    fill: "/tool mem " },
    PaletteCommand { name: "tasks",  description: "Show task dependency graph",         keywords: "task deps dependencies adjacency",   fill: "/tasks" },
    PaletteCommand { name: "gaps",   description: "Show recorded knowledge gaps",        keywords: "knowledge gap unknown missing info",  fill: "/gaps" },
    PaletteCommand { name: "plan",   description: "Switch to Plan mode (reflective)",   keywords: "interactive manual control plan",   fill: "/plan" },
    PaletteCommand { name: "build",  description: "Switch to Build mode (autonomous)",  keywords: "build auto run execute work",       fill: "/build" },
    PaletteCommand { name: "copycode",  description: "Copy code block from last reply",   keywords: "copy code block clipboard snippet", fill: "/copycode" },
    PaletteCommand { name: "copytext",  description: "Copy full last reply as plain text", keywords: "copy text clipboard message",      fill: "/copytext" },
    PaletteCommand { name: "copylink",  description: "Copy URL from last reply",           keywords: "copy link url clipboard",          fill: "/copylink" },
    PaletteCommand { name: "openlink",  description: "Open URL from last reply in browser", keywords: "open link url browser",           fill: "/openlink" },
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
    /// Receives result from async subagent execution
    subagent_result_rx: Option<oneshot::Receiver<crate::spawner::AgentResult>>,
    /// Number of subagents spawned this session (display counter)
    subagent_count: u32,
    /// Number of subagents currently running
    active_subagents: u32,
    /// Actual token counts from last completed response: (prompt, generated)
    last_token_counts: (u32, u32),
    /// Running total of tokens used this session
    total_tokens_used: u32,
    /// Last context % at which we warned in chat (prevents spam)
    last_warned_ctx_pct: u32,
    /// Monotonic frame counter — increments every event loop tick, used for animations
    tick_count: u64,
    /// Scroll offset from bottom (0 = pinned to latest, >0 = scrolled up)
    scroll_offset: u16,
    /// Whether the user has manually scrolled up (suppresses auto-pin)
    user_scrolled: bool,
    /// Last time a 🕐 clock event was inserted (for 5-min interval markers)
    last_clock: std::time::Instant,
    /// Whether the command palette is open
    palette_open: bool,
    /// Which palette item is highlighted
    palette_selection: usize,
    /// Whether the model picker overlay is open
    model_picker_open: bool,
    /// Available models for the picker
    model_picker_items: Vec<String>,
    /// Currently highlighted model in picker (index into filtered list)
    model_picker_selection: usize,
    /// Fuzzy search query for the model picker
    model_picker_query: String,
    /// Detected terminal theme (light/dark + colour palette)
    theme: Theme,
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
            subagent_result_rx: None,
            subagent_count: 0,
            active_subagents: 0,
            last_token_counts: (0, 0),
            total_tokens_used: 0,
            last_warned_ctx_pct: 0,
            tick_count: 0,
            scroll_offset: 0,
            user_scrolled: false,
            last_clock: std::time::Instant::now(),
            palette_open: false,
            palette_selection: 0,
            model_picker_open: false,
            model_picker_items: Vec::new(),
            model_picker_selection: 0,
            model_picker_query: String::new(),
            theme: Theme::detect(),
        }
    }

    /// Run the TUI — main event loop with streaming support
    pub async fn run(&mut self) -> Result<()> {
        let _guard = TerminalGuard::new()?;

        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        // In Build mode, always fire a kick prompt to orient the agent
        if self.mode == AppMode::Build {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".to_string());

            let kick = if self.agents_context.is_none() {
                // No AGENTS.md — terraforming mode: explore and create it
                format!(
                    "New session started in `{cwd}`. \
                     This directory has no AGENTS.md yet — you need to terraform it. \
                     First, explore the directory: [TOOL: spawn ls -la .] and read any \
                     key files (README, Cargo.toml, package.json, etc.). \
                     Then write an AGENTS.md that describes the project: its purpose, \
                     structure, build commands, conventions, and any gotchas. \
                     After writing AGENTS.md, continue with normal autonomous work. \
                     Say [DONE] at each milestone."
                )
            } else {
                // AGENTS.md exists — normal autonomous kick
                format!(
                    "New session started in `{cwd}`. \
                     Orient yourself: list the directory, review any existing tasks, \
                     and begin working autonomously. \
                     Use tools to explore. When a task is fully complete, say [DONE] \
                     to notify the user, then immediately continue to the next task."
                )
            };
            self.handle_message(&kick).await;
        }

        loop {
            self.tick_count = self.tick_count.wrapping_add(1);
            // Drain any pending stream tokens before drawing
            self.drain_stream_tokens();
            // Check for completed tool execution
            self.check_tool_result();
            // Check for completed gap reflection
            self.check_gap_result();
            // Check for completed subagent execution
            self.check_subagent_result();

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
                    Event::Mouse(mouse) => {
                        self.handle_mouse(mouse);
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
                    if !self.user_scrolled {
                        self.scroll_offset = 0;
                    }
                }
                Ok(StreamEvent::Done(prompt_tokens, gen_tokens)) => {
                    self.last_token_counts = (prompt_tokens, gen_tokens);
                    self.total_tokens_used += prompt_tokens + gen_tokens;
                    self.complete_streaming_turn();
                    return;
                }
                Ok(StreamEvent::Error(e)) => {
                    self.notify(format!("❌ Stream error: {}", e));
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
            self.notify(format!("⚠️ Response received but not saved: {}", e));
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
        let spawn_calls = crate::spawner::parse_spawn_agent_calls(&response_text);

        // Detect [DONE] signal: notify the user but do NOT stop — re-kick to continue
        let signaled_done = response_text.contains("[DONE]");

        // Handle spawn_agent: show 🤖 N indicator in chat, execute first one
        if !spawn_calls.is_empty() && self.subagent_result_rx.is_none() {
            let (task_id, task_desc) = &spawn_calls[0];
            self.subagent_count += 1;
            self.active_subagents += 1;
            let n = self.subagent_count;
            let spawn_msg = Message::new("spawn",
                format!("#{n} spawning  {task_id}\n{task_desc}"));
            self.persist_message(spawn_msg);
            self.cached_message_count = self.message_buffer.count()
                .unwrap_or(self.cached_message_count + 1);
            self.status_message = format!("🤖 #{n} running: {task_id}");
            self.execute_subagent_async(task_id.clone(), task_desc.clone());
            self.turn_phase = TurnPhase::ExecutingTool(format!("spawn_agent:{}", task_id));
        } else if !tool_calls.is_empty() && self.tool_iteration_count < MAX_TOOL_ITERATIONS {
            // Execute FIRST tool call only (one per turn to avoid dependency issues)
            let call = &tool_calls[0];
            self.status_message = format!("🔧 Executing tool: {} ...", call.name);
            self.execute_tool_async(call.name.clone(), call.args.clone());
            self.turn_phase = TurnPhase::ExecutingTool(call.name.clone());
        } else if signaled_done {
            // Agent thinks it's done — notify user but inject a continue kick
            self.push_system_event("🌸 Agent signaled [DONE] — notifying & continuing…");
            tokio::spawn(crate::notifications::model_responded("🌸 milestone reached"));
            self.status_message = "🌸 Milestone reached — finding next task".to_string();
            // Re-kick with a continue prompt so the loop doesn't idle
            let continue_msg = Message::new("system",
                "Good work. Now find what's next — more tasks, improvements, \
                 refactoring, or documentation. Keep going.");
            if let Err(e) = self.message_buffer.add_and_persist(continue_msg) {
                eprintln!("Failed to save continue kick: {}", e);
            }
            self.cached_message_count = self.message_buffer.count()
                .unwrap_or(self.cached_message_count + 1);
            // Immediately start next streaming turn
            let steering = self.steering_text();
            let messages = self.message_buffer.messages().unwrap_or_default();
            if let Some(client) = &self.ollama_client {
                self.stream_rx = Some(client.generate_streaming(messages, Some(&steering)));
                self.streaming_text.clear();
                self.turn_phase = TurnPhase::Streaming;
                self.tool_iteration_count = 0;
            } else {
                self.turn_phase = TurnPhase::Idle;
                self.tool_iteration_count = 0;
            }
        } else {
            if self.tool_iteration_count >= MAX_TOOL_ITERATIONS {
                self.notify("⚠️ Max tool iterations reached — pausing");
            } else if self.mode == AppMode::Build {
                // Build mode: agent gave plain text without [DONE] — nudge it to keep going
                let kick = Message::new("system", "Keep going. Find the next task or improvement.");
                if let Err(e) = self.message_buffer.add_and_persist(kick) {
                    eprintln!("Build kick persist error: {}", e);
                }
                self.cached_message_count = self.message_buffer.count()
                    .unwrap_or(self.cached_message_count + 1);
                let steering = self.steering_text();
                let messages = self.message_buffer.messages().unwrap_or_default();
                if let Some(client) = &self.ollama_client {
                    self.stream_rx = Some(client.generate_streaming(messages, Some(&steering)));
                    self.streaming_text.clear();
                    self.turn_phase = TurnPhase::Streaming;
                    self.tool_iteration_count = 0;
                    self.streaming_text.clear();
                    self.stream_rx = self.stream_rx.take(); // already set above
                    return; // skip clearing stream_rx below
                }
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

    /// Spawn a subagent off the UI thread; result arrives via subagent_result_rx
    fn execute_subagent_async(&mut self, task_id: String, task_desc: String) {
        let (tx, rx) = oneshot::channel();
        let endpoint = self.config.endpoint.clone();
        let model = self.config.model.clone();

        tokio::spawn(async move {
            let config = crate::agent::AgentConfig {
                model,
                endpoint: endpoint.clone(),
                max_iterations: 10,
                max_recursion_depth: 10,
                current_depth: 1,
            };
            let result = crate::spawner::spawn_subagent(
                "ui", &task_id, &task_desc, &endpoint, config,
            ).await;
            let agent_result = result.unwrap_or_else(|e| crate::spawner::AgentResult {
                agent_id: format!("ui/{}", task_id),
                task_description: task_desc,
                output: format!("Error: {}", e),
                success: false,
            });
            let _ = tx.send(agent_result);
        });

        self.subagent_result_rx = Some(rx);
    }

    /// Check if a subagent has finished; inject result and continue conversation
    fn check_subagent_result(&mut self) {
        let done = match self.subagent_result_rx.as_mut() {
            Some(rx) => rx.try_recv().is_ok(),
            None => return,
        };
        if !done { return; }

        // Re-take and receive (already peeked Ok above, so this won't block)
        let mut rx = self.subagent_result_rx.take().unwrap();
        let result = match rx.try_recv() {
            Ok(r) => r,
            _ => return,
        };

        self.active_subagents = self.active_subagents.saturating_sub(1);
        let status_icon = if result.success { "✅ done" } else { "❌ failed" };
        // Show a truncated preview of the output (first 3 lines, max 200 chars)
        let preview: String = result.output.lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n");
        let preview = if preview.len() > 200 {
            format!("{}…", &preview[..200])
        } else {
            preview
        };
        let done_content = format!("#{} {}  {}\n{}",
            self.subagent_count, status_icon, result.agent_id, preview);
        let done_msg = Message::new("spawn", done_content);
        self.persist_message(done_msg);
        self.cached_message_count = self.message_buffer.count()
            .unwrap_or(self.cached_message_count + 1);

        // Inject result back into conversation and continue streaming
        let injection = result.to_injection();
        let injection_msg = Message::new("tool", &injection);
        self.persist_message(injection_msg);
        self.cached_message_count = self.message_buffer.count()
            .unwrap_or(self.cached_message_count + 1);

        self.turn_phase = TurnPhase::Idle;
        self.tool_iteration_count = 0;

        // Kick next stream turn with the injected result
        if let Some(client) = self.ollama_client.clone() {
            let messages: Vec<Message> = self.message_buffer.messages().unwrap_or_default();
            let steering = self.steering_text();
            let rx = client.generate_streaming(messages, Some(&steering));
            self.stream_rx = Some(rx);
            self.streaming_text.clear();
            self.turn_phase = TurnPhase::Streaming;
        }
    }


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
                    self.notify(format!("⚠️ Failed to save tool result: {}", e));
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
                    self.notify("⚠️ Ollama offline — retrying after next message");
                    self.turn_phase = TurnPhase::Idle;
                    self.tool_iteration_count = 0;
                }
            }
            Err(oneshot::error::TryRecvError::Empty) => {
                // Still waiting for tool execution
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                self.notify("❌ Tool execution failed unexpectedly");
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
             Use tools proactively. Do not say you cannot access files—use [TOOL: spawn ls] instead. Be concise.\n\
             Say [DONE] when a task is complete — this notifies the user as a milestone. \
             After [DONE], immediately find and begin the next task: improve, refactor, document, explore. \
             Work is continuous; [DONE] is a checkpoint, not a stop."
        );
        if let Some(ctx) = &self.agents_context {
            base.push_str("\n\n--- AGENTS.md ---\n");
            base.push_str(ctx);
        } else {
            base.push_str("\n\nNo AGENTS.md exists yet. If you haven't already, explore the \
                directory and create one with [TOOL: editfile AGENTS.md].");
        }
        SteeringDirective::custom(&base).format_for_system_prompt()
    }

    /// Autocompact: drop oldest messages (keep last 20 conversational turns)
    /// to bring context back under threshold. Archived to scrollback, not deleted.
    fn run_autocompact(&mut self) {
        let all = match self.message_buffer.messages() {
            Ok(msgs) => msgs,
            Err(_) => return,
        };

        let keep_tail = 20usize;
        let conversation_count = all.iter()
            .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
            .count();

        if conversation_count <= keep_tail {
            return; // Not enough to compact
        }

        let drop_conversational = conversation_count - keep_tail;
        let mut dropped = 0usize;
        let kept: Vec<crate::message::Message> = all.into_iter()
            .filter(|m| {
                if matches!(m.role.as_str(), "user" | "assistant") && dropped < drop_conversational {
                    dropped += 1;
                    false // drop oldest
                } else {
                    true  // keep
                }
            })
            .collect();

        // Archive everything, then re-insert the kept messages
        if let Err(e) = self.message_buffer.archive_to_scrollback() {
            eprintln!("Autocompact archive failed: {}", e);
            return;
        }
        if let Err(e) = self.message_buffer.add_multiple(&kept) {
            eprintln!("Autocompact re-insert failed: {}", e);
            return;
        }

        self.last_token_counts = (0, 0);
        self.last_warned_ctx_pct = 0; // reset so threshold warnings fire again after compaction
        self.cached_message_count = self.message_buffer.count().unwrap_or(0);
        self.push_system_event(format!(
            "🌿 Autocompacted: archived {} old messages to scrollback",
            dropped
        ));
    }


    fn push_system_event(&mut self, text: impl Into<String>) {
        let msg = Message::new("system", text);
        self.persist_message(msg);
        self.cached_message_count = self.message_buffer.messages()
            .map(|v| v.len()).unwrap_or(0);
    }

    /// Show a message in both the status bar and the chat timeline.
    /// Use for errors, warnings, and significant state changes.
    fn notify(&mut self, text: impl Into<String>) {
        let s: String = text.into();
        self.status_message = s.clone();
        self.push_system_event(s);
    }

    /// Persist a message to SQLite and asynchronously write it to .yggdra/log.
    /// Inserts a 🕐 clock marker if 5+ minutes have passed since the last one.
    fn persist_message(&mut self, msg: Message) -> bool {
        // Insert clock marker every 5 minutes
        if self.last_clock.elapsed() >= std::time::Duration::from_secs(300) {
            let timestamp = chrono::Local::now().format("%H:%M").to_string();
            let clock_msg = Message::new("clock", format!("🕐 {}", timestamp));
            if let Some(sender) = &self.log_sender as &Option<crate::msglog::LogSender> {
                sender.log(&clock_msg);
            }
            let _ = self.message_buffer.add_and_persist(clock_msg);
            self.last_clock = std::time::Instant::now();
        }

        if let Some(sender) = &self.log_sender as &Option<crate::msglog::LogSender> {
            sender.log(&msg);
        }
        // Auto-pin to bottom when new content arrives
        if !self.user_scrolled {
            self.scroll_offset = 0;
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
        let (mode_label, mode_color) = match self.mode {
            AppMode::Build => ("⚡ BUILD", self.theme.violet),
            AppMode::Plan  => ("🧠 PLAN",  self.theme.accent),
        };

        // Token usage indicator — real counts when available, estimate otherwise
        let (prompt_tok, gen_tok) = self.last_token_counts;
        let context_indicator = if prompt_tok > 0 {
            // Real data from Ollama
            let context_window = self.config.context_window.unwrap_or(4096) as f64;
            let usage_pct = ((prompt_tok as f64) / context_window * 100.0).min(100.0) as u32;
            let dot = if usage_pct > 70 { "🔴" } else if usage_pct > 50 { "🟡" } else { "🟢" };
            format!("{dot} {prompt_tok}+{gen_tok}tok ({usage_pct}%)")
        } else {
            // No response yet — show session total or idle
            if self.total_tokens_used > 0 {
                format!("🟢 {}tok total", self.total_tokens_used)
            } else {
                "🟢 idle".to_string()
            }
        };

        let header_line = Line::from(vec![
            Span::raw("🌷 "),
            Span::styled(mode_label, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
            Span::raw(format!(" | {} | {} | {}", connection_status, self.config.model, context_indicator)),
        ]);

        let header = Paragraph::new(header_line)
            .block(Block::default().borders(Borders::BOTTOM).title("Status"));
        f.render_widget(header, chunks[0]);

        // Messages area with full-width colored bands — bottom-anchored with scroll
        let messages_area = chunks[1];
        let viewport_height = messages_area.height as i32;
        let area_width = messages_area.width;
        let messages_list = self
            .message_buffer
            .messages()
            .unwrap_or_default();

        // Pre-compute each message's formatted text, style, and estimated height
        struct RenderedMsg {
            text: String,
            style: Style,
            height: u16,
        }

        let mut rendered: Vec<RenderedMsg> = Vec::with_capacity(messages_list.len() + 1);
        let mut exchange_idx: usize = 0;

        for msg in messages_list.iter() {
            // Solarized-light-friendly pastel bands — no explicit fg so terminal default
            // dark text shows through. Works on both light and dark terminals.
            let (emoji, bg_tint, show_band) = match msg.role.as_str() {
                "user" => {
                    exchange_idx += 1;
                    let tint = if exchange_idx % 2 == 0 { self.theme.band_a } else { self.theme.band_b };
                    ("👤", Some(tint), true)
                }
                "assistant" => {
                    exchange_idx += 1;
                    let tint = if exchange_idx % 2 == 0 { self.theme.band_a } else { self.theme.band_b };
                    ("🤖", Some(tint), true)
                }
                "tool"   => ("🔧", None, false),
                "system" => ("⚙️", None, false),
                "clock"  => ("🕐", None, false),
                "spawn"  => ("🤖", Some(self.theme.band_spawn), true),
                _        => ("💬", None, false),
            };

            let text_content = if msg.role == "tool" || msg.role == "spawn" {
                format!("{} {}", emoji, self.format_tool_content(&msg.content))
            } else {
                format!("{} {}", emoji, self.format_message_content(&msg.content))
            };

            // Estimate height accounting for line wrapping
            let line_count = text_content.lines().count().max(1);
            let wrap_extra: usize = if area_width > 0 {
                text_content.lines()
                    .map(|l| (l.len() as u16).saturating_sub(1) / area_width.max(1))
                    .sum::<u16>() as usize
            } else { 0 };
            let height = (line_count + wrap_extra).max(1) as u16;

            let style = if show_band {
                // Dark theme: set explicit light fg so text contrasts against dark band
                // Light theme: no explicit fg → terminal's dark default text shows through
                if self.theme.kind == crate::theme::ThemeKind::Dark {
                    Style::default().fg(Color::Rgb(220, 230, 240)).bg(bg_tint.unwrap())
                } else {
                    Style::default().bg(bg_tint.unwrap())
                }
            } else {
                Style::default()
            };

            rendered.push(RenderedMsg { text: text_content, style, height: height + 1 });
        }

        // Add streaming text as a virtual message at the end
        if !self.streaming_text.is_empty() {
            let tint = if exchange_idx % 2 == 0 { self.theme.band_a } else { self.theme.band_b };
            let agent_badge = if self.active_subagents > 0 {
                format!(" [🤖{}]", self.active_subagents)
            } else {
                String::new()
            };
            let stream_text = format!("🤖{} {}▌", agent_badge, self.streaming_text);
            let line_count = stream_text.lines().count().max(1);
            let wrap_extra: usize = if area_width > 0 {
                stream_text.lines()
                    .map(|l| (l.len() as u16).saturating_sub(1) / area_width.max(1))
                    .sum::<u16>() as usize
            } else { 0 };
            let height = (line_count + wrap_extra).max(1) as u16;
            let stream_style = if self.theme.kind == crate::theme::ThemeKind::Dark {
                Style::default().fg(Color::Rgb(220, 230, 240)).bg(tint)
            } else {
                Style::default().bg(tint)
            };
            rendered.push(RenderedMsg { text: stream_text, style: stream_style, height });
        }

        // Calculate total content height and clamp scroll_offset
        let total_height: i32 = rendered.iter().map(|m| m.height as i32).sum();
        let max_scroll = (total_height - viewport_height).max(0) as u16;
        let effective_scroll = self.scroll_offset.min(max_scroll);

        // Bottom-anchored rendering: skip lines from the top based on scroll position
        // lines_to_skip = total_height - viewport_height - scroll_offset
        let lines_to_skip = (total_height - viewport_height - effective_scroll as i32).max(0);

        let mut skipped: i32 = 0;
        let mut current_y = messages_area.top();

        for rm in &rendered {
            let msg_h = rm.height as i32;

            // Skip messages that are entirely above the viewport
            if skipped + msg_h <= lines_to_skip {
                skipped += msg_h;
                continue;
            }

            // Partial skip: this message starts partway through
            let partial_skip = (lines_to_skip - skipped).max(0) as u16;
            skipped = lines_to_skip; // done skipping

            let visible_height = rm.height.saturating_sub(partial_skip);
            let available = (messages_area.bottom() as i32 - current_y as i32).max(0) as u16;
            let draw_height = visible_height.min(available);

            if draw_height == 0 || current_y >= messages_area.bottom() {
                break;
            }

            let msg_para = Paragraph::new(rm.text.clone())
                .style(rm.style)
                .wrap(ratatui::widgets::Wrap { trim: true })
                .scroll((partial_skip, 0));

            let msg_area = Rect {
                x: messages_area.x,
                y: current_y,
                width: messages_area.width,
                height: draw_height,
            };

            f.render_widget(msg_para, msg_area);
            current_y += draw_height;
        }

        // Scroll indicator in top-right of messages area
        if effective_scroll > 0 {
            let indicator = format!("↑{}", effective_scroll);
            let ind_x = messages_area.right().saturating_sub(indicator.len() as u16 + 1);
            let ind_area = Rect {
                x: ind_x,
                y: messages_area.top(),
                width: indicator.len() as u16 + 1,
                height: 1,
            };
            let ind_widget = Paragraph::new(indicator)
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(ind_widget, ind_area);
        }

        // Input area — animated hint while streaming
        // Smooth robot-yapping animation: static 🤖💬 + cycling dot ligature
        const DOTS: &[&str] = &["·", "··", "···", "····", "···", "··"];
        let dot = DOTS[(self.tick_count / 12) as usize % DOTS.len()];
        let yap = &format!("🤖💬 {}", dot);
        let input_hint = match &self.turn_phase {
            TurnPhase::Idle => "(type message or /help for commands)",
            TurnPhase::Streaming => yap,
            TurnPhase::ExecutingTool(_) => "🔧 …",
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
                                    Style::default().fg(self.theme.selected_fg).bg(self.theme.accent).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(self.theme.accent)
                                },
                            ),
                            Span::styled(
                                format!(" {}", cmd.description),
                                if i == self.palette_selection {
                                    Style::default().fg(self.theme.selected_fg).bg(self.theme.accent)
                                } else {
                                    Style::default()
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

        // Model picker overlay — centered popup over entire screen
        if self.model_picker_open && !self.model_picker_items.is_empty() {
            let area = f.area();
            let picker_width = (area.width * 9 / 10).max(50).min(area.width.saturating_sub(4));
            // +1 row for the search bar inside the border
            let filtered = self.model_picker_filtered();
            let visible_rows = (area.height * 4 / 5).saturating_sub(5).max(3);
            let picker_height = (filtered.len() as u16 + 5).min(area.height - 4).min(visible_rows + 5);
            let picker_x = (area.width.saturating_sub(picker_width)) / 2;
            let picker_y = (area.height.saturating_sub(picker_height)) / 2;
            let picker_rect = Rect { x: picker_x, y: picker_y, width: picker_width, height: picker_height };

            // Split picker: top row = search bar, rest = list
            let inner = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(Rect { x: picker_rect.x + 1, y: picker_rect.y + 1,
                               width: picker_rect.width.saturating_sub(2),
                               height: picker_rect.height.saturating_sub(2) });

            let list_rows = inner[1].height as usize;
            let scroll_top = if self.model_picker_selection >= list_rows {
                self.model_picker_selection - list_rows + 1
            } else {
                0
            };

            let items: Vec<ListItem> = filtered.iter()
                .enumerate()
                .skip(scroll_top)
                .take(list_rows)
                .map(|(vis_i, &orig_i)| {
                    let name = &self.model_picker_items[orig_i];
                    let is_current = name.starts_with(&self.config.model);
                    let marker = if is_current { "✦ " } else { "  " };
                    let label = format!("{}{}", marker, name);
                    let style = if vis_i == self.model_picker_selection {
                        Style::default().fg(self.theme.selected_fg).bg(self.theme.violet).add_modifier(Modifier::BOLD)
                    } else if is_current {
                        Style::default().fg(self.theme.violet)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Span::styled(label, style))
                }).collect();

            let count_label = if filtered.len() != self.model_picker_items.len() {
                format!(" 🌸 Models  {}/{} match  Esc cancel ", filtered.len(), self.model_picker_items.len())
            } else {
                format!(" 🌸 Models  {}  ↑↓ navigate · Enter select · Esc cancel ", self.model_picker_items.len())
            };

            // Render border + title
            let border = Block::default()
                .borders(Borders::ALL)
                .title(count_label)
                .border_style(Style::default().fg(self.theme.violet));
            f.render_widget(Clear, picker_rect);
            f.render_widget(border, picker_rect);

            // Search bar row
            let search_text = format!("🔍 {}_", self.model_picker_query);
            let search = Paragraph::new(Span::styled(search_text, Style::default().fg(self.theme.violet)));
            f.render_widget(search, inner[0]);

            // Model list
            let list = List::new(items);
            f.render_widget(list, inner[1]);
        }

        // Status bar — show current prompt tokens / context window size
        let ctx_window = self.config.context_window.unwrap_or(4096);
        let (prompt_tok, _) = self.last_token_counts;
        let token_info = if prompt_tok > 0 {
            format!("🪙 {}/{}", prompt_tok, ctx_window)
        } else if self.total_tokens_used > 0 {
            format!("🪙 ~{}/{}", self.total_tokens_used, ctx_window)
        } else {
            format!("🪙 0/{}", ctx_window)
        };
        let status = format!(
            "🔢 {} | {} | 💬 {} | {}",
            &self.session.id[..8],
            token_info,
            self.cached_message_count,
            self.status_message.lines().next().unwrap_or("")
        );
        let status_bar = Paragraph::new(status);
        f.render_widget(status_bar, chunks[3]);
    }

    /// Handle keyboard input
    async fn handle_key(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;

        // Model picker takes over all input when open
        if self.model_picker_open {
            match key.code {
                KeyCode::Esc => {
                    self.model_picker_open = false;
                    self.model_picker_query.clear();
                    self.status_message = "Model picker cancelled".to_string();
                }
                KeyCode::Char(c) => {
                    self.model_picker_query.push(c);
                    self.model_picker_selection = 0; // reset to top on new query
                }
                KeyCode::Backspace => {
                    self.model_picker_query.pop();
                    self.model_picker_selection = 0;
                }
                KeyCode::Down => {
                    let count = self.model_picker_filtered().len();
                    if count > 0 {
                        self.model_picker_selection = (self.model_picker_selection + 1) % count;
                    }
                }
                KeyCode::Up => {
                    let count = self.model_picker_filtered().len();
                    if count > 0 {
                        self.model_picker_selection = self.model_picker_selection
                            .checked_sub(1).unwrap_or(count - 1);
                    }
                }
                KeyCode::Enter => {
                    let filtered = self.model_picker_filtered();
                    if let Some(&orig_i) = filtered.get(self.model_picker_selection) {
                        let raw = self.model_picker_items[orig_i].clone();
                        let model_name = raw.split_whitespace().next().unwrap_or(&raw).to_string();
                        self.config.model = model_name.clone();
                        let endpoint = self.config.endpoint.clone();
                        match OllamaClient::new(&endpoint, &model_name).await {
                            Ok(client) => {
                                self.ollama_client = Some(client);
                                self.notify(format!("🌸 Switched to {}", model_name));
                            }
                            Err(e) => {
                                self.notify(format!("❌ Failed to connect with {}: {}", model_name, e));
                            }
                        }
                        self.model_picker_open = false;
                        self.model_picker_query.clear();
                    }
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if c == 'c' {
                    self.running = false;
                } else if c == 's' {
                    self.handle_command().await;
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
            KeyCode::PageUp => {
                let half_page = crossterm::terminal::size().map(|(_, h)| h / 2).unwrap_or(10);
                self.scroll_offset = self.scroll_offset.saturating_add(half_page);
                self.user_scrolled = true;
            }
            KeyCode::PageDown => {
                let half_page = crossterm::terminal::size().map(|(_, h)| h / 2).unwrap_or(10);
                self.scroll_offset = self.scroll_offset.saturating_sub(half_page);
                if self.scroll_offset == 0 {
                    self.user_scrolled = false;
                }
            }
            _ => {}
        }
    }

    /// Handle mouse events (scroll wheel)
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(3);
                self.user_scrolled = true;
            }
            MouseEventKind::ScrollDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(3);
                if self.scroll_offset == 0 {
                    self.user_scrolled = false;
                }
            }
            _ => {}
        }
    }
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

    /// Return indices into model_picker_items that match the current fuzzy query.
    /// Empty query → all items. Results sorted by match score descending.
    fn model_picker_filtered(&self) -> Vec<usize> {
        if self.model_picker_query.is_empty() {
            return (0..self.model_picker_items.len()).collect();
        }
        let query = self.model_picker_query.to_lowercase();
        let mut scored: Vec<(i32, usize)> = self.model_picker_items.iter()
            .enumerate()
            .filter_map(|(i, name)| {
                let s = fuzzy_score(&query, &name.to_lowercase());
                if s > 0 { Some((s, i)) } else { None }
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i)| i).collect()
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
        } else if command.starts_with("/shell ") || command == "/shell" {
            let shell_cmd = command.strip_prefix("/shell").unwrap_or("").trim().to_string();
            if shell_cmd.is_empty() {
                self.status_message = "Usage: /shell <command>".to_string();
            } else {
                self.handle_shell_command(shell_cmd).await;
            }
        } else if command == "/build" {
            self.mode = AppMode::Build;
            self.notify("⚡ Switched to Build mode — autonomous execution");
        } else if command == "/plan" {
            self.mode = AppMode::Plan;
            self.notify("🧠 Switched to Plan mode — reflective & interactive");
        } else if command == "/mode" {
            self.mode = match self.mode { AppMode::Build => AppMode::Plan, AppMode::Plan => AppMode::Build };
            let label = match self.mode { AppMode::Build => "⚡ Build", AppMode::Plan => "🧠 Plan" };
            self.notify(format!("Switched to {} mode", label));
        } else if command.starts_with("/copycode") {
            let n = command.split_whitespace().nth(1).and_then(|s| s.parse::<usize>().ok());
            self.handle_copycode(n).await;
        } else if command == "/copytext" {
            self.handle_copytext().await;
        } else if command.starts_with("/copylink") {
            let n = command.split_whitespace().nth(1).and_then(|s| s.parse::<usize>().ok());
            self.handle_link_command(false, n).await;
        } else if command.starts_with("/openlink") {
            let n = command.split_whitespace().nth(1).and_then(|s| s.parse::<usize>().ok());
            self.handle_link_command(true, n).await;
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
                    self.notify(format!("❌ Failed to save tool output: {}", e));
                } else {
                    self.status_message = format!("✅ Tool {} executed successfully", tool_name);
                }
            }
            Err(e) => {
                self.notify(format!("❌ Tool {} error: {}", tool_name, e));
            }
        }
    }

    /// Handle /shell — run a shell command, show output in chat, inform the assistant
    async fn handle_shell_command(&mut self, cmd: String) {
        self.status_message = format!("⏳ Running: {}", cmd);

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .await;

        let (stdout, stderr, code) = match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let code = out.status.code().unwrap_or(-1);
                (stdout, stderr, code)
            }
            Err(e) => (String::new(), format!("Failed to run: {e}"), -1),
        };

        let combined = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
            (false, false) => format!("{}\n{}", stdout.trim(), stderr.trim()),
            (false, true)  => stdout.trim().to_string(),
            (true, false)  => stderr.trim().to_string(),
            (true, true)   => "(no output)".to_string(),
        };

        let exit_label = if code == 0 { "✅".to_string() } else { format!("❌ exit {code}") };

        // Show in chat as a tool message (indented, distinct style)
        let tool_msg = Message::new("tool", format!("{exit_label} $ {cmd}\n{combined}"));
        self.persist_message(tool_msg);
        self.cached_message_count = self.message_buffer.count().unwrap_or(0);
        self.status_message = format!("{exit_label} shell command done");

        // Inject as user message so the assistant sees it and can respond
        let context_msg = Message::new("user",
            format!("I just ran this shell command:\n```\n$ {cmd}\n```\nOutput (exit {code}):\n```\n{combined}\n```"));
        if let Err(e) = self.message_buffer.add_and_persist(context_msg) {
            eprintln!("Failed to save shell context: {}", e);
            return;
        }
        self.cached_message_count = self.message_buffer.count().unwrap_or(0);

        // Trigger assistant response
        let steering = self.steering_text();
        let messages = self.message_buffer.messages().unwrap_or_default();
        if let Some(client) = &self.ollama_client {
            self.stream_rx = Some(client.generate_streaming(messages, Some(&steering)));
            self.streaming_text.clear();
            self.turn_phase = TurnPhase::Streaming;
            self.tool_iteration_count = 0;
        }
    }

    /// Copy text to system clipboard using pbcopy / xclip / wl-copy
    async fn copy_to_clipboard(text: &str) -> Result<(), String> {
        // Try pbcopy (macOS), then wl-copy (Wayland), then xclip (X11)
        let candidates = &[
            ("pbcopy",  vec![]),
            ("wl-copy", vec![]),
            ("xclip",   vec!["-selection", "clipboard"]),
            ("xsel",    vec!["--clipboard", "--input"]),
        ];
        for (cmd, args) in candidates {
            let mut c = tokio::process::Command::new(cmd);
            c.args(args);
            c.stdin(std::process::Stdio::piped());
            c.stdout(std::process::Stdio::null());
            c.stderr(std::process::Stdio::null());
            if let Ok(mut child) = c.spawn() {
                use tokio::io::AsyncWriteExt;
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes()).await;
                }
                if let Ok(status) = child.wait().await {
                    if status.success() { return Ok(()); }
                }
            }
        }
        Err("No clipboard utility found (pbcopy / wl-copy / xclip / xsel)".to_string())
    }

    /// Extract fenced code blocks from markdown text. Returns (lang, body) pairs.
    fn extract_code_blocks(text: &str) -> Vec<(String, String)> {
        let mut blocks = Vec::new();
        let mut in_block = false;
        let mut lang = String::new();
        let mut body = String::new();
        for line in text.lines() {
            if !in_block {
                if line.trim_start().starts_with("```") {
                    lang = line.trim_start().trim_start_matches('`').trim().to_string();
                    body.clear();
                    in_block = true;
                }
            } else if line.trim_start().starts_with("```") {
                blocks.push((lang.clone(), body.trim_end().to_string()));
                in_block = false;
            } else {
                body.push_str(line);
                body.push('\n');
            }
        }
        blocks
    }

    /// Extract URLs from text.
    fn extract_urls(text: &str) -> Vec<String> {
        let mut urls = Vec::new();
        for word in text.split_whitespace() {
            let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != ':' && c != '.' && c != '-' && c != '_' && c != '?' && c != '=' && c != '&' && c != '#' && c != '%');
            if w.starts_with("http://") || w.starts_with("https://") || w.starts_with("file://") {
                if !urls.contains(&w.to_string()) { urls.push(w.to_string()); }
            }
        }
        urls
    }

    /// Get the text of the last assistant message
    fn last_assistant_message(&self) -> Option<String> {
        self.message_buffer.messages().ok()?.into_iter().rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.content)
    }

    /// /copycode [N] — copy Nth code block (1-based) from last assistant message
    async fn handle_copycode(&mut self, arg: Option<usize>) {
        let Some(text) = self.last_assistant_message() else {
            self.notify("❌ No assistant message to copy from"); return;
        };
        let blocks = Self::extract_code_blocks(&text);
        if blocks.is_empty() {
            self.notify("❌ No code blocks found in last message"); return;
        }
        if blocks.len() == 1 || arg == Some(1) || arg.is_none() {
            let idx = arg.map(|n| n.saturating_sub(1)).unwrap_or(0).min(blocks.len() - 1);
            let (lang, code) = &blocks[idx];
            let label = if lang.is_empty() { String::new() } else { format!(" ({})", lang) };
            match Self::copy_to_clipboard(code).await {
                Ok(_) => self.notify(format!("📋 Copied code block{} ({} lines)", label, code.lines().count())),
                Err(e) => self.notify(format!("❌ Clipboard error: {}", e)),
            }
        } else {
            // Show numbered list
            let list: String = blocks.iter().enumerate()
                .map(|(i, (l, b))| format!("  {}. `{}` — {} lines", i + 1, if l.is_empty() { "plain" } else { l }, b.lines().count()))
                .collect::<Vec<_>>().join("\n");
            self.notify(format!("📋 {} code blocks found — use /copycode N:\n{}", blocks.len(), list));
        }
    }

    /// /copytext — copy full text of last assistant message
    async fn handle_copytext(&mut self) {
        let Some(text) = self.last_assistant_message() else {
            self.notify("❌ No assistant message to copy"); return;
        };
        match Self::copy_to_clipboard(&text).await {
            Ok(_) => self.notify(format!("📋 Copied {} chars to clipboard", text.len())),
            Err(e) => self.notify(format!("❌ Clipboard error: {}", e)),
        }
    }

    /// /copylink [N] / /openlink [N] — act on URL(s) in last assistant message
    async fn handle_link_command(&mut self, open: bool, arg: Option<usize>) {
        let Some(text) = self.last_assistant_message() else {
            self.notify("❌ No assistant message to scan"); return;
        };
        let urls = Self::extract_urls(&text);
        if urls.is_empty() {
            self.notify("❌ No URLs found in last message"); return;
        }
        let idx = arg.map(|n| n.saturating_sub(1)).unwrap_or(0).min(urls.len() - 1);
        if urls.len() > 1 && arg.is_none() {
            let list: String = urls.iter().enumerate()
                .map(|(i, u)| format!("  {}. {}", i + 1, u))
                .collect::<Vec<_>>().join("\n");
            let verb = if open { "openlink" } else { "copylink" };
            self.notify(format!("🔗 {} URLs found — use /{} N:\n{}", urls.len(), verb, list));
            return;
        }
        let url = &urls[idx];
        if open {
            let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            let _ = tokio::process::Command::new(opener).arg(url).spawn();
            self.notify(format!("🔗 Opening {}", url));
        } else {
            match Self::copy_to_clipboard(url).await {
                Ok(_) => self.notify(format!("📋 Copied {}", url)),
                Err(e) => self.notify(format!("❌ Clipboard error: {}", e)),
            }
        }
    }

    /// Handle /models command — fetch model list and open interactive picker
    async fn handle_models_command(&mut self) {
        // Always re-read config.toml to pick up endpoint changes without restart
        let fresh_config = crate::config::Config::load();
        let target_endpoint = fresh_config.endpoint.clone();

        // Reconnect if endpoint changed or client is missing
        let needs_reconnect = self.ollama_client.is_none()
            || self.ollama_client.as_ref().map(|c| c.endpoint()) != Some(&target_endpoint);

        if needs_reconnect {
            self.status_message = format!("🔌 Connecting to {}…", target_endpoint);
            match OllamaClient::new(&target_endpoint, &self.config.model).await {
                Ok(client) => {
                    self.config.endpoint = target_endpoint.clone();
                    self.ollama_client = Some(client);
                }
                Err(e) => {
                    self.push_system_event(format!("❌ Failed to connect to {}: {}", target_endpoint, e));
                    return;
                }
            }
        }

        match &self.ollama_client {
            Some(client) => {
                self.status_message = "⏳ Fetching models...".to_string();
                match tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client.list_models()
                ).await {
                    Ok(Ok(models)) if !models.is_empty() => {
                        // Mark current model with ✦
                        self.model_picker_items = models.iter().map(|m| {
                            let size = m.size
                                .map(|b| format!(" {:.1}GB", b as f64 / 1_073_741_824.0))
                                .unwrap_or_default();
                            format!("{}{}", m.name, size)
                        }).collect();
                        // Pre-select the current model
                        self.model_picker_selection = models.iter()
                            .position(|m| m.name == self.config.model)
                            .unwrap_or(0);
                        self.model_picker_open = true;
                        self.status_message = "🌸 Select model — ↑↓ navigate, Enter select, Esc cancel".to_string();
                    }
                    Ok(Ok(_)) => {
                        self.push_system_event("ℹ️ No models found. Run: ollama pull <model>");
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
                        self.push_system_event("❌ Model fetch timed out");
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
            self.notify(format!("❌ Storage error: {}", self.friendly_error(&e.to_string())));
            return;
        }
        self.cached_message_count = self.message_buffer.count().unwrap_or(self.cached_message_count + 1);

        // Autocompact when context window is getting full (>70% threshold)
        let context_window = self.config.context_window.unwrap_or(8192) as f64;
        let (prompt_tok, _) = self.last_token_counts;
        let usage_pct = if prompt_tok > 0 {
            (prompt_tok as f64 / context_window * 100.0) as u32
        } else {
            (self.cached_message_count as f64 * 150.0 / context_window * 100.0) as u32
        };
        if usage_pct >= 70 {
            self.run_autocompact();
        }

        if self.ollama_client.is_none() {
            self.push_system_event("🦙 Ollama offline: message saved but not sent");
            self.notify("⚠️ Ollama offline — message queued locally");
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
                    // Only display the language tag if it's a real known language;
                    // Qwen often emits ```lua for generic blocks — map those to "code"
                    const KNOWN_LANGS: &[&str] = &[
                        "rust","python","py","javascript","js","typescript","ts","go","java",
                        "c","cpp","c++","cs","csharp","bash","sh","zsh","fish","toml","yaml",
                        "yml","json","html","css","sql","dockerfile","makefile","zig","kotlin",
                        "swift","ruby","php","scala","haskell","elixir","erlang","ocaml","r",
                        "markdown","md","xml","csv","diff","patch","text","txt","plaintext",
                        "proto","graphql","nix","vim","assembly","asm","wgsl","glsl","hlsl",
                    ];
                    let canonical = lang_part.to_lowercase();
                    code_language = if lang_part.is_empty() {
                        "code".to_string()
                    } else if KNOWN_LANGS.contains(&canonical.as_str()) {
                        lang_part.to_string()
                    } else {
                        "code".to_string() // unknown/hallucinated tag → generic
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

    /// Format tool output with indented bordered block
    fn format_tool_content(&self, content: &str) -> String {
        let mut result = String::new();
        for line in content.lines() {
            result.push_str("│  ");
            result.push_str(line);
            result.push('\n');
        }
        if result.ends_with('\n') { result.pop(); }
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
                self.notify(format!("❌ Checkpoint failed: {}", e));
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
                self.notify(format!("❌ Clear failed: {}", e));
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
            self.notify(format!("❌ Failed to save search results: {}", e));
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

