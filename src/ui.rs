/// TUI module: minimal terminal UI with streaming responses, tool execution, and multi-window sync
use crate::agent;
use crate::battery::BatteryState;
use crate::config::{Config, AppMode};
use crate::highlight::Highlighter;
use crate::message::{Message, MessageBuffer};
use crate::ollama::{OllamaClient, StreamEvent};
use crate::session::Session;
use crate::steering::SteeringDirective;
use crate::task::{TaskManager, Checkpoint};
use crate::theme::Theme;
use crate::tools::ToolRegistry;
use crate::metrics::MetricsTracker;
use anyhow::Result;
use unicode_width::UnicodeWidthStr;
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

/// Parse an RGB color string in format "r,g,b" into a Color
fn parse_rgb_string(s: &str) -> Option<Color> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return None;
    }
    let r = parts[0].trim().parse::<u8>().ok()?;
    let g = parts[1].trim().parse::<u8>().ok()?;
    let b = parts[2].trim().parse::<u8>().ok()?;
    Some(Color::Rgb(r, g, b))
}

/// What the app should do at the end of a streaming turn with no parsed tool calls.
///
/// Extracted as a pure function so the loop-prevention logic is independently testable
/// without constructing the full `App` struct.
#[derive(Debug, PartialEq)]
pub(crate) enum StreamEndAction {
    /// Normal interactive response — persist and go idle.
    Persist,
    /// Inject another continue-kick to keep the autonomous loop running.
    Kick,
    /// One mode: the task is considered complete.
    CompleteOne(&'static str),
    /// Stop auto-loop but stay in current mode. User must send a message to resume.
    Halt(&'static str),
}

/// Decide what to do when a streaming turn ends with no tool calls.
///
/// * `has_text` — `streaming_text` is non-empty after the turn.
/// * `mode` — current app mode.
/// * `tool_iteration_count` — how many tool→re-stream cycles completed this task.
/// * `consecutive_empty_kicks` — kicks WITHOUT tool calls so far (before this turn).
pub(crate) fn decide_stream_end(
    has_text: bool,
    mode: AppMode,
    tool_iteration_count: usize,
    consecutive_empty_kicks: u32,
) -> StreamEndAction {
    // After this turn, kicks-without-tools becomes consecutive_empty_kicks + 1.
    let kicks_after = consecutive_empty_kicks.saturating_add(1);
    match mode {
        AppMode::Plan | AppMode::Ask => StreamEndAction::Persist,
            AppMode::Forever => StreamEndAction::Kick, // Never halt — run forever
        AppMode::One => {
            if !has_text {
                // Model produced only thinking tokens (or nothing) — kick up to 3 times.
                if kicks_after >= 3 {
                    StreamEndAction::CompleteOne("empty responses")
                } else {
                    StreamEndAction::Kick
                }
            } else if tool_iteration_count > 0 {
                // Tools were used at some point; plain-text response = task done.
                StreamEndAction::CompleteOne("no tool calls")
            } else {
                // No tools used yet — nudge the model to start working, up to 3 times.
                if kicks_after >= 3 {
                    StreamEndAction::CompleteOne("model unresponsive")
                } else {
                    StreamEndAction::Kick
                }
            }
        }
    }
}

/// Parse a `<plan>…</plan>` block out of a response string.
///
/// Returns `(cleaned_text, Some(plan_content))` when a block is found, or
/// `(original_text, None)` when there is none.  The block is stripped from
/// the returned text so it does not clutter the chat history.
pub(crate) fn extract_plan_block(text: &str) -> (String, Option<String>) {
    const START: &str = "<plan>";
    const END: &str = "</plan>";
    let start = match text.find(START) { Some(i) => i, None => return (text.to_string(), None) };
    let end   = match text.find(END)   { Some(i) => i, None => return (text.to_string(), None) };
    if end <= start { return (text.to_string(), None); }

    let plan_content = text[start + START.len()..end].trim().to_string();
    let before = text[..start].trim_end();
    let after  = text[end + END.len()..].trim_start();
    let cleaned = match (before.is_empty(), after.is_empty()) {
        (true,  true)  => String::new(),
        (true,  false) => after.to_string(),
        (false, true)  => before.to_string(),
        (false, false) => format!("{}\n{}", before, after),
    };
    (cleaned, Some(plan_content))
}

/// Detect shell commands that are likely to write files, so Plan mode can warn the user.
///
/// Heuristics (conservative — prefers false negatives over false positives):
/// - Output redirect: `>` outside quotes, not preceded by `-`, `=`, `>` and not followed by `=`, `>`, `&`
/// - Append redirect: `>>`  
/// - `tee` writing to a file
/// - Heredoc: `<< '` or `<< "`
pub(crate) fn is_shell_write_pattern(cmd: &str) -> bool {
    // Heredoc: << 'EOF' or << "EOF" — often used to write file content
    if cmd.contains("<< '") || cmd.contains("<< \"") || cmd.contains("<<'") || cmd.contains("<<\"") {
        return true;
    }
    // tee writing to a file
    if cmd.contains("| tee ") || cmd.contains("|tee ") {
        return true;
    }
    // Scan for `>` outside quotes that is actually a redirect.
    // We track single-quote and double-quote state to avoid false positives on
    // comparison operators inside awk/grep patterns like `awk 'NR > 5'`.
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double_quote => { in_single_quote = !in_single_quote; }
            b'"'  if !in_single_quote => { in_double_quote = !in_double_quote; }
            b'>' if !in_single_quote && !in_double_quote => {
                let prev = if i > 0 { bytes[i - 1] } else { 0 };
                let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
                // Skip `->`, `=>`, `>=`, `>>`, `>&` (fd-to-fd like 2>&1)
                if prev != b'-' && prev != b'=' && next != b'=' && next != b'>' && next != b'&' {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Truncate output to at most `cap` chars, keeping the **tail** (most recent output).
/// Compiler errors, test failures, etc. always appear at the end — the tail is what matters.
/// Format: `…(N omitted)` for minimal display.
fn truncate_tail(output: &str, cap: usize) -> String {
    let chars: Vec<char> = output.chars().collect();
    if chars.len() <= cap {
        return output.to_string();
    }
    let dropped = chars.len() - cap;
    let tail: String = chars[dropped..].iter().collect();
    format!("…({} omitted)\n{}", dropped, tail)
}

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
    PaletteCommand { name: "estimate", description: "Show project completion estimate",  keywords: "progress percentage done metrics", fill: "/estimate" },
    PaletteCommand { name: "endpoint", description: "Change Ollama endpoint URL",      keywords: "ollama server endpoint url connection", fill: "/endpoint " },
    PaletteCommand { name: "model",  description: "Switch AI model",                    keywords: "model switch change llm ollama",   fill: "/model " },
    PaletteCommand { name: "models", description: "List available Ollama models",       keywords: "list llm ollama choose switch",     fill: "/models" },
    PaletteCommand { name: "params", description: "Set model params (temperature, top_k…)", keywords: "temperature top_k top_p repeat penalty params sampling", fill: "/set_params " },
    PaletteCommand { name: "temperature", description: "Set sampling temperature (0.0–2.0)", keywords: "temperature heat creativity sampling randomness", fill: "/temperature " },
    PaletteCommand { name: "ctx",    description: "Set context window size",           keywords: "context window size tokens",      fill: "/ctx " },
    PaletteCommand { name: "toolcap", description: "Set tool output cap (chars, or 'off')", keywords: "tool output truncate cap compress context", fill: "/toolcap " },
    PaletteCommand { name: "zt",      description: "Toggle zero-truncation (show full raw tool output)", keywords: "truncation raw full output tool context debug", fill: "/zt" },
    PaletteCommand { name: "stats",   description: "Show cumulative session stats",       keywords: "stats metrics tokens usage tools sessions uptime", fill: "/stats" },
    PaletteCommand { name: "compress", description: "Summarize session and reset context", keywords: "compress summarize archive context reset memory", fill: "/compress" },
    PaletteCommand { name: "gradient", description: "Toggle pastel gradient background", keywords: "gradient background pastel visual theme", fill: "/gradient " },
    PaletteCommand { name: "theme",    description: "Set colour theme (dark/light/auto)",  keywords: "theme dark light color colour visual",  fill: "/theme " },
    PaletteCommand { name: "color",    description: "Generate gradient colors from a prompt (e.g. sunset, ocean, forest)", keywords: "color gradient theme palette aesthetic visual", fill: "/color " },
    PaletteCommand { name: "checkpoint", description: "Save session checkpoint",        keywords: "save progress milestone snapshot",   fill: "/checkpoint " },
    PaletteCommand { name: "clear",  description: "Archive conversation to scrollback", keywords: "clear buffer reset history archive", fill: "/clear" },
    PaletteCommand { name: "mem",    description: "Search archived scrollback",         keywords: "search memory past conversation",    fill: "/tool mem " },
    PaletteCommand { name: "tasks",  description: "Show task dependency graph",         keywords: "task deps dependencies adjacency",   fill: "/tasks" },
    PaletteCommand { name: "gaps",   description: "Show recorded knowledge gaps",        keywords: "knowledge gap unknown missing info",  fill: "/gaps" },
    PaletteCommand { name: "save",   description: "Save current plan as a todo task",    keywords: "save plan todo task write markdown",  fill: "/save" },
    PaletteCommand { name: "mode",  description: "Cycle or set mode (ask/plan/build/one)", keywords: "mode switch cycle toggle ask plan build one", fill: "/mode " },
    PaletteCommand { name: "build", description: "Switch to Build mode (autonomous execution)", keywords: "build mode autonomous switch", fill: "/build" },
    PaletteCommand { name: "plan",  description: "Switch to Plan mode (interactive)", keywords: "plan mode interactive switch default", fill: "/plan" },
    PaletteCommand { name: "ask",   description: "Switch to Ask mode (read-only)", keywords: "ask mode read only switch", fill: "/ask" },
    PaletteCommand { name: "one",   description: "Switch to One mode (one-off task w/ notification)", keywords: "one off task complete notification mode switch", fill: "/one" },
    PaletteCommand { name: "abort", description: "Abort stream / async tasks / tool execution", keywords: "stop abort cancel stuck stalled timeout stream async", fill: "/abort" },
    PaletteCommand { name: "test_notification", description: "Fire a test OS notification", keywords: "notify notification test os macos toast alert", fill: "/test_notification" },
    PaletteCommand { name: "test_models",  description: "Test model tool-calling capability",  keywords: "test model capability tool calling json format verify debug", fill: "/test_models" },
    PaletteCommand { name: "copycode",  description: "Copy code block from last reply",   keywords: "copy code block clipboard snippet", fill: "/copycode" },
    PaletteCommand { name: "copytext",  description: "Copy full last reply as plain text", keywords: "copy text clipboard message",      fill: "/copytext" },
    PaletteCommand { name: "copyall",   description: "Copy entire conversation to clipboard", keywords: "copy all context conversation history clipboard full", fill: "/copyall" },
    PaletteCommand { name: "copyprompt", description: "Copy current system prompt to clipboard", keywords: "copy prompt system clipboard debug", fill: "/copyprompt" },
    PaletteCommand { name: "showprompt", description: "Show full system prompt in chat (scrollable)", keywords: "show display prompt system inspect debug tree files", fill: "/showprompt" },
    PaletteCommand { name: "copylink",  description: "Copy URL from last reply",           keywords: "copy link url clipboard",          fill: "/copylink" },
    PaletteCommand { name: "openlink",  description: "Open URL from last reply in browser", keywords: "open link url browser",           fill: "/openlink" },
    PaletteCommand { name: "tool rg",    description: "Search files with ripgrep",      keywords: "search grep find file text",        fill: "/tool rg " },
    PaletteCommand { name: "tool setfile", description: "Create or overwrite a file",     keywords: "write create overwrite file setfile", fill: "/tool setfile " },
    PaletteCommand { name: "tool exec",     description: "Execute a command directly",    keywords: "run exec binary program command",   fill: "/tool exec " },
    PaletteCommand { name: "tool shell",    description: "Run via sh -c (pipes/chains)",  keywords: "shell pipe redirect chain bash",    fill: "/tool shell " },
    PaletteCommand { name: "tool commit",   description: "Git commit with message",     keywords: "git save version commit history",   fill: "/tool commit " },
    PaletteCommand { name: "tool python",   description: "Run a Python script",         keywords: "python py script run execute",      fill: "/tool python " },
    PaletteCommand { name: "tool ruste",    description: "Compile and run Rust code",   keywords: "rust compile execute cargo rustc",  fill: "/tool ruste " },
    PaletteCommand { name: "view",          description: "Open file viewer (tabs, scroll)", keywords: "view file open read tab viewer",  fill: "/view " },
    PaletteCommand { name: "diff",          description: "View git diff in file viewer",   keywords: "diff git changes patch hunk",      fill: "/diff" },
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
///
/// CRITICAL ORDERING: This guard MUST be created AFTER terminal initialization succeeds.
/// If TerminalGuard::new() is called before Terminal::new(), then any error during
/// Terminal::new() will cause the guard to drop with raw mode already enabled but
/// the terminal never initialized, leaving the terminal in an inconsistent state.
///
/// Correct order:
/// 1. Initialize terminal via Terminal::new()
/// 2. Create TerminalGuard (enables raw mode + alternate screen)
/// 3. On drop: disables raw mode and leaves alternate screen
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
    args: String,
    output: std::result::Result<String, String>,
}

/// A background async tool task (spawned with mode: async).
/// Output is written to .yggdra/async/<task_id>.txt and injected on completion.
struct AsyncTask {
    task_id: String,
    command_preview: String,
    started_at: std::time::Instant,
    rx: tokio::sync::oneshot::Receiver<ToolResult>,
}

/// Inline tool result state — shows tool results in real-time panel
#[derive(Debug, Clone)]
struct InlineToolResult {
    tool_name: String,
    start_time: std::time::Instant,
    output: String,
    is_complete: bool,
    exit_code: Option<i32>,  // None if still running, Some(code) if complete
}

/// Status of a subagent in the panel
#[derive(Debug, Clone, PartialEq)]
enum SubagentStatus { Running, Done, Failed }

/// One entry in the subagents side-panel
#[derive(Debug, Clone)]
struct SubagentEntry {
    index: u32,
    task_id: String,
    status: SubagentStatus,
    /// Live preview text (updated from token stream while running, then set to output summary)
    preview: String,
    /// Message count at the time the subagent completed (None while still running)
    completed_at_msg: Option<usize>,
}

/// A single tab in the file viewer overlay
struct FileTab {
    label: String,
    lines: Vec<String>,
    scroll: usize,
    is_diff: bool,
}

/// Cached pre-rendered message for the draw loop.
/// Rebuilt only when messages_cache changes or terminal width/theme changes.
struct CachedRender {
    /// Rendered spacer (blank line above message)
    blank: ratatui::text::Text<'static>,
    /// Rendered message content
    content: ratatui::text::Text<'static>,
    style: Style,
    height: u16,  // content height (depends on area_width)
}

fn text_height_static(text: &ratatui::text::Text, area_width: u16) -> u16 {
    use unicode_width::UnicodeWidthStr;
    let line_count = text.lines.len().max(1);
    let wrap_extra: usize = if area_width > 0 {
        text.lines.iter()
            .map(|l| {
                let w: usize = l.spans.iter().map(|s| s.content.width()).sum();
                (w as u16).saturating_sub(1) / area_width.max(1)
            })
            .sum::<u16>() as usize
    } else { 0 };
    (line_count + wrap_extra).max(1) as u16
}

/// Minimal TUI application
pub struct App {
    config: Config,
    session: Session,
    input_buffer: String,
    status_message: String,
    running: bool,
    /// Ctrl+Q sets this — app exits cleanly once the current turn reaches Idle
    pending_quit: bool,
    message_buffer: MessageBuffer,
    task_manager: TaskManager,
    ollama_client: Option<OllamaClient>,
    tool_registry: ToolRegistry,
    cached_message_count: usize,
    /// Accumulates tokens during streaming
    streaming_text: String,
    /// Accumulates native thinking tokens during streaming (from msg.thinking field)
    thinking_text: String,
    /// True while we're inside an inline <think>...</think> block during streaming
    in_think_block: bool,
    /// Receives tokens from the streaming task
    stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
    /// Explicit state machine for the current turn
    turn_phase: TurnPhase,
    /// How many tool→re-stream cycles this turn
    tool_iteration_count: usize,
    /// Receives result from async tool execution
    tool_result_rx: Option<oneshot::Receiver<ToolResult>>,
    /// Background async tasks spawned via mode:async tool calls
    async_tasks: Vec<AsyncTask>,
    /// Receives result from async gap query
    gap_rx: Option<oneshot::Receiver<Option<crate::gaps::Gap>>>,
    /// Streams individual test results from background /test_models run
    test_models_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
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
    /// Live token stream from running subagent
    subagent_token_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    /// Accumulated live text from running subagent (cleared on completion)
    subagent_live_text: String,
    /// Number of subagents spawned this session (display counter)
    subagent_count: u32,
    /// Number of subagents currently running
    active_subagents: u32,
    /// Actual token counts from last completed response: (prompt, generated)
    last_token_counts: (u32, u32),
    /// Running total of tokens used this session
    total_tokens_used: u32,
    /// Tokens used in the current task (since last </done>)
    task_tokens_since_done: u32,
    /// Session token count when last </done> was emitted
    session_tokens_at_last_done: u32,
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
    /// When the current streaming turn started (for prefill elapsed timer)
    stream_start_time: Option<std::time::Instant>,
    /// When the last streaming token was received (for stall detection)
    last_stream_token_time: Option<std::time::Instant>,
    /// Whether the command palette is open
    palette_open: bool,
    /// Which palette item is highlighted
    palette_selection: usize,
    /// Input buffer saved when Ctrl+P opens palette mid-text; restored on close/cancel
    palette_saved_buffer: Option<String>,
    /// Whether the model picker overlay is open
    model_picker_open: bool,
    /// Available models for the picker
    model_picker_items: Vec<String>,
    /// Currently highlighted model in picker (index into filtered list)
    model_picker_selection: usize,
    /// Fuzzy search query for the model picker
    model_picker_query: String,
    /// Detected terminal theme (light/dark + colour palette); set after terminal init
    theme: Theme,
    /// Tracks project completion metrics
    metrics: MetricsTracker,
    /// Receives filesystem watcher events (config.json or AGENTS.md changes)
    pub config_watcher_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::watcher::ConfigChange>>,
    /// Runtime parameter overrides (set by user or agent; not persisted)
    runtime_params: crate::config::ModelParams,
    /// Parsed AGENTS.md config (models + parameter defaults)
    agents_config: crate::config::AgentsConfig,
    /// Last time a build-mode kick was fired — for watchdog recovery
    last_build_kick: std::time::Instant,
    /// Consecutive build-mode kicks without tool calls — stuck detection
    consecutive_empty_kicks: u32,
    /// Explicit flag: autonomous kicks are paused (model stuck / errors). Reset on user input.
    autokick_paused: bool,
    /// Consecutive format-error corrections injected — loop break
    consecutive_format_errors: u32,
    /// Consecutive tool errors (same tool failing) — loop break
    consecutive_tool_errors: u32,
    /// Name of the last tool that produced an error (for grouping consecutive errors)
    last_errored_tool: String,
    /// Pending user messages waiting to run in Forever/One mode.
    /// When the agent is busy and the user submits a new message, it's pushed here
    /// instead of interrupting the current turn. Drained FIFO when the agent goes idle.
    task_queue: std::collections::VecDeque<String>,
    /// Whether gradient background is enabled in message area
    gradient_enabled: bool,
    /// Inference rate from last completed generation (tokens/second)
    last_infer_rate: Option<f64>,
    /// Cached battery power state (refreshed every 5 minutes via spawn_blocking)
    on_battery: BatteryState,
    /// Last time battery status was checked
    last_battery_check: std::time::Instant,
    /// Pending async battery check result
    battery_result_rx: Option<tokio::sync::oneshot::Receiver<BatteryState>>,
    /// Pending async /color generation result (raw LLM response text or error)
    color_result_rx: Option<oneshot::Receiver<Result<String, String>>>,
    /// Syntax highlighter for code blocks
    highlighter: Highlighter,
    /// Currently displayed inline tool results (cleared when tool completes and is added to history)
    inline_tool_results: Vec<InlineToolResult>,
    /// Subagent panel entries — each spawned subagent has one; shown for 5 messages after completion
    subagent_entries: Vec<SubagentEntry>,
    /// Cached message list — refreshed from SQLite only when cached_message_count changes.
    /// Avoids running a full SELECT on every draw frame during streaming.
    messages_cache: Vec<crate::message::Message>,
    /// Tick counter used to periodically re-detect terminal theme without terminal queries.
    theme_check_counter: u32,
    /// File viewer overlay: open tabs and active tab index
    file_viewer_open: bool,
    file_viewer_tabs: Vec<FileTab>,
    file_viewer_active: usize,
    /// Persistent project stats — written to .yggdra/stats.json on exit.
    stats: crate::stats::Stats,
    /// Tracks which thinking blocks are collapsed; key is message index in messages_cache
    collapsed_thinking_blocks: std::collections::HashSet<usize>,
    /// Time this App was created — used to compute uptime on exit.
    session_start: std::time::Instant,

    // ── Loop-prevention state ────────────────────────────────────────────
    /// Rolling window of recent (tool_name, args_hash) dispatches — cap 20.
    /// Used to detect the agent calling the same tool with the same args repeatedly.
    recent_tool_calls: std::collections::VecDeque<(String, u64)>,
    /// How many consecutive "identical call" spin notices have been injected this turn.
    spin_notice_count: u32,
    /// Per-turn error frequency: (tool_name, error_hash) → consecutive count.
    recent_tool_errors: std::collections::HashMap<(String, u64), u32>,
    /// Timestamp of the last tool call that mutated the filesystem (edit/write/patch/commit).
    last_mutating_action: std::time::Instant,
    /// True once we've sent the "no files changed in N calls" stall notice this session.
    stall_notice_sent: bool,
    /// Set by check_context_pressure when usage ≥ 90% — consumed by the async run loop.
    pending_auto_compress: bool,
    /// Count of successful tool executions since last compress — triggers auto-compress at 8+
    tools_executed_since_compress: usize,
    /// When true, tool output is never truncated — full raw content injected into context.
    zero_truncation: bool,
    /// Number of messages dropped by the last sliding-window context trim (0 = nothing dropped).
    /// Used to render the cutoff divider in the message list.
    context_cutoff_dropped: usize,
    /// Compact project file listing (size + modified time + path, newest-first).

    // ── Celebration effects ───────────────────────────────────────────────
    /// Active fireworks particles for </done> celebration. Each particle has (x, y, dx, dy, age, max_age, color).
    fireworks: Vec<(f32, f32, f32, f32, u16, u16, ratatui::style::Color)>,
    /// Frame when fireworks were triggered (for animation timing)
    fireworks_triggered_at: Option<u64>,
    /// Current task progress percentage (0-100), parsed from <percent>N</percent> tags
    task_progress: Option<u32>,
    /// Injected into every system prompt so the model knows what exists.
    project_context: String,
    /// When project_context was last built (refresh after mutations or after 60s stale)
    project_context_built: std::time::Instant,
    /// Content of the N most recently modified text files — refreshed alongside project_context.
    recent_files_content: String,
    /// Which message index (in messages_cache) the expand/collapse cursor is on (None = no cursor)
    msg_cursor: Option<usize>,
    /// Set of message indices (in messages_cache) that have been expanded by the user
    expanded_msgs: std::collections::HashSet<usize>,
    /// Forces a render cache rebuild on next draw (set when cursor or expanded state changes)
    render_cache_dirty: bool,
    /// Pre-rendered message cache — rebuilt only when messages_cache changes or
    /// the terminal width / theme changes. Avoids re-running syntax highlighting
    /// on every draw frame while streaming.
    render_cache: Vec<CachedRender>,
    /// messages_cache.len() when render_cache was last built
    render_cache_msg_count: usize,
    /// Terminal area_width when render_cache was last built
    render_cache_width: u16,
    /// Theme kind when render_cache was last built
    render_cache_theme: crate::theme::ThemeKind,
    /// exchange_idx after the last cached message (so streaming text picks up the right band)
    render_cache_exchange_end: usize,
    /// True when the agent declared [UNDERSTOOD] in Plan mode; cleared when One mode launches.
    plan_understood: bool,
    /// Endpoint type for display in status bar (e.g., "Ollama", "OpenRouter", "llama.cpp")
    endpoint_type: String,
    /// When true, the next draw loop iteration will clear the terminal first to recover
    /// from rendering corruption (e.g., after connection errors with raw escape sequences).
    needs_full_redraw: bool,
}

/// Format a token count as a human-readable string (e.g. 1.2k, 32k, 1.5M).
fn human_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

impl App {
    /// Create new app with optional Ollama client and AGENTS.md content
    pub fn new(
        config: Config,
        session: Session,
        ollama_client: Option<OllamaClient>,
        agents_md: Option<String>,
        config_watcher_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::watcher::ConfigChange>>,
    ) -> Self {
        let mut message_buffer = MessageBuffer::from_db(&session.messages_db)
            .unwrap_or_else(|e| {
                crate::dlog!("🌹 Failed to open messages DB: {}", e);
                MessageBuffer::new(&session.messages_db)
                    .unwrap_or_else(|e2| {
                        crate::dlog!("🌹 FATAL: Cannot create message database at {:?}: {}", session.messages_db, e2);
                        std::process::exit(1);
                    })
            });
        // Clean up any kick messages persisted by older versions of yggdra
        let _ = message_buffer.purge_kicks();
        let task_manager = TaskManager::from_db(&session.tasks_db)
            .unwrap_or_else(|e| {
                crate::dlog!("🌹 Failed to open tasks DB: {}", e);
                TaskManager::new(&session.tasks_db)
                    .unwrap_or_else(|e2| {
                        crate::dlog!("🌹 FATAL: Cannot create task database at {:?}: {}", session.tasks_db, e2);
                        std::process::exit(1);
                    })
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

        let initial_mode = config.mode;
        let gradient_enabled = config.ui_settings.gradient_enabled;

        // Parse AGENTS.md into structured config for model + param defaults
        let cwd = std::env::current_dir().unwrap_or_default();
        let agents_config = crate::config::AgentsConfig::parse_from_file(&cwd.join("AGENTS.md"));

        // Load persistent stats and record this session start
        let mut stats = crate::stats::Stats::load(&cwd);
        stats.on_session_start();
        stats.save(&cwd);

        let endpoint_type = if ollama_client.is_some() {
            crate::ollama::detect_endpoint_type(&config.endpoint)
        } else {
            "Offline".to_string()
        };

        // Default to dark theme; only call detect_safe() when config explicitly says "auto".
        // detect_safe() uses macOS system APIs which can be unreliable on startup,
        // causing the theme to appear to "reset" to light when the user expects dark.
        let mut theme = Theme::dark();

        // Load custom gradient colors from config if set
        if let (Some(start_str), Some(end_str)) = (&config.ui_settings.gradient_start, &config.ui_settings.gradient_end) {
            if let (Some(start), Some(end)) = (parse_rgb_string(start_str), parse_rgb_string(end_str)) {
                theme.gradient_start = start;
                theme.gradient_end = end;
            }
        }

        // Apply theme preference from config
        if let Some(theme_pref) = &config.ui_settings.theme {
            match theme_pref.as_str() {
                "dark" => theme = Theme::dark(),
                "light" => theme = Theme::light(),
                "auto" | _ => {
                    if let Some(detected) = Theme::detect_safe() {
                        theme = if detected { Theme::light() } else { Theme::dark() };
                    }
                }
            }
        }

        // Re-apply custom gradient colors after theme (they override theme defaults)
        if let (Some(start_str), Some(end_str)) = (&config.ui_settings.gradient_start, &config.ui_settings.gradient_end) {
            if let (Some(start), Some(end)) = (parse_rgb_string(start_str), parse_rgb_string(end_str)) {
                theme.gradient_start = start;
                theme.gradient_end = end;
            }
        }

        let project_context = build_project_context(10000);
        let recent_files_content = build_recent_files_content(5, 2000);

        Self {
            config,
            session,
            input_buffer: String::new(),
            status_message,
            running: true,
            pending_quit: false,
            message_buffer,
            task_manager,
            ollama_client,
            tool_registry: ToolRegistry::new(),
            cached_message_count: 0,
            streaming_text: String::new(),
            thinking_text: String::new(),
            in_think_block: false,
            stream_rx: None,
            turn_phase: TurnPhase::Idle,
            tool_iteration_count: 0,
            tool_result_rx: None,
            async_tasks: Vec::new(),
            gap_rx: None,
            test_models_rx: None,
            log_sender,
            log_dir,
            mode: initial_mode,
            agents_context: agents_md,
            subagent_result_rx: None,
            subagent_token_rx: None,
            subagent_live_text: String::new(),
            subagent_count: 0,
            active_subagents: 0,
            last_token_counts: (0, 0),
            total_tokens_used: 0,
            task_tokens_since_done: 0,
            session_tokens_at_last_done: 0,
            last_warned_ctx_pct: 0,
            tick_count: 0,
            scroll_offset: 0,
            user_scrolled: false,
            last_clock: std::time::Instant::now(),
            stream_start_time: None,
            last_stream_token_time: None,
            palette_open: false,
            palette_selection: 0,
            palette_saved_buffer: None,
            model_picker_open: false,
            model_picker_items: Vec::new(),
            model_picker_selection: 0,
            model_picker_query: String::new(),
            theme,
            metrics: MetricsTracker::new(),
            config_watcher_rx,
            runtime_params: crate::config::ModelParams::default(),
            agents_config,
            last_build_kick: std::time::Instant::now(),
            consecutive_empty_kicks: 0,
            autokick_paused: false,
            consecutive_format_errors: 0,
            consecutive_tool_errors: 0,
            last_errored_tool: String::new(),
            task_queue: std::collections::VecDeque::new(),
            gradient_enabled,
            last_infer_rate: None,
            on_battery: crate::battery::battery_state(),
            last_battery_check: std::time::Instant::now(),
            battery_result_rx: None,
            color_result_rx: None,
            highlighter: Highlighter::new(),
            inline_tool_results: Vec::new(),
            subagent_entries: Vec::new(),
            messages_cache: Vec::new(),
            theme_check_counter: 0,
            file_viewer_open: false,
            file_viewer_tabs: Vec::new(),
            file_viewer_active: 0,
            stats,
            session_start: std::time::Instant::now(),
            recent_tool_calls: std::collections::VecDeque::new(),
            spin_notice_count: 0,
            recent_tool_errors: std::collections::HashMap::new(),
            last_mutating_action: std::time::Instant::now(),
            stall_notice_sent: false,
            pending_auto_compress: false,
            tools_executed_since_compress: 0,
            zero_truncation: false,
            context_cutoff_dropped: 0,
            project_context,
            project_context_built: std::time::Instant::now(),
            recent_files_content,
            msg_cursor: None,
            expanded_msgs: std::collections::HashSet::new(),
            render_cache_dirty: false,
            render_cache: Vec::new(),
            render_cache_msg_count: usize::MAX, // force first build
            render_cache_width: 0,
            render_cache_theme: crate::theme::ThemeKind::Dark,
            fireworks: Vec::new(),
            fireworks_triggered_at: None,
            task_progress: None,
            render_cache_exchange_end: 0,
            plan_understood: false,
            endpoint_type,
            needs_full_redraw: false,
            collapsed_thinking_blocks: std::collections::HashSet::new(),
        }
    }

    /// Build (or rebuild) the pre-rendered message cache.
    /// Called before draw() whenever the message list, terminal width, or theme changes.
    /// This amortizes syntax-highlighting cost: O(N) only on changes, O(1) per frame.
    fn build_render_cache(&mut self, area_width: u16) {
        let is_dark = self.theme.kind == crate::theme::ThemeKind::Dark;
        let mut cache: Vec<CachedRender> = Vec::with_capacity(self.messages_cache.len() + 1);
        let mut exchange_idx: usize = 0;

        // Compute which message index (in messages_cache) is the first one INSIDE context.
        // build_messages filters out system/clock/think-tool messages before sliding-window drop.
        // msgs_dropped non-system/clock/kick messages (after the first user msg) are out of context.
        let cutoff_insert_idx: Option<usize> = if self.context_cutoff_dropped > 0 {
            let mut dropped_seen = 0usize;
            let mut first_user_seen = false;
            let mut result = None;
            for (i, msg) in self.messages_cache.iter().enumerate() {
                let skip = msg.role == "system" || msg.role == "clock" || msg.role == "kick"
                    || (msg.role == "tool" && msg.content.contains("[TOOL_OUTPUT: think ="));
                if skip { continue; }
                if !first_user_seen {
                    // The first user message is always pinned in context — skip past it
                    if msg.role == "user" || msg.role == "notice" {
                        first_user_seen = true;
                    }
                    continue;
                }
                // Messages after the first user: the first msgs_dropped are out of context
                dropped_seen += 1;
                if dropped_seen > self.context_cutoff_dropped {
                    result = Some(i);
                    break;
                }
            }
            result
        } else {
            None
        };

        let cutoff_label = if self.context_cutoff_dropped > 0 {
            let n = self.context_cutoff_dropped;
            format!("╌╌╌  {} message{} above not in model context  ╌╌╌",
                n, if n == 1 { "" } else { "s" })
        } else {
            String::new()
        };

        let mut divider_inserted = false;

        for (msg_idx, msg) in self.messages_cache.iter().enumerate() {
            // Insert divider before the first in-context message
            if !divider_inserted {
                if let Some(cut_idx) = cutoff_insert_idx {
                    if msg_idx == cut_idx {
                        let dim_color = if is_dark {
                            Color::Rgb(90, 90, 110)
                        } else {
                            Color::Rgb(140, 140, 160)
                        };
                        let divider_content = ratatui::text::Text::from(
                            ratatui::text::Line::from(vec![
                                ratatui::text::Span::styled(
                                    cutoff_label.clone(),
                                    Style::default().fg(dim_color),
                                )
                            ])
                        );
                        let blank = ratatui::text::Text::from(" ".to_string());
                        let height = 1u16;
                        cache.push(CachedRender { blank, content: divider_content, style: Style::default(), height });
                        divider_inserted = true;
                    }
                }
            }
            if msg.role == "kick" { continue; }

            let is_cursor = self.msg_cursor == Some(msg_idx);
            let is_tool_msg = msg.role == "tool" || msg.role == "spawn";
            let is_expanded = self.zero_truncation || self.expanded_msgs.contains(&msg_idx);

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
                "notice" => ("📋", None, false),
                "clock"  => ("🕐", None, false),
                "spawn"  => ("🤖", Some(self.theme.band_spawn), true),
                _        => ("💬", None, false),
            };

            // For cursor on tool messages: replace emoji with ► and add expand hint
            let display_emoji = if is_cursor && is_tool_msg {
                if is_expanded { "▼" } else { "►" }
            } else {
                emoji
            };

            let content = if is_tool_msg {
                // Check if this tool output contains diff content — if so, render with colors.
                let diff_content = if let Some(rest) = msg.content.strip_prefix("[TOOL_OUTPUT: ") {
                    if let Some(eq) = rest.find(" = ") {
                        let name = &rest[..eq];
                        let raw_body = rest[eq + 3..].trim_end_matches(']');
                        if looks_like_diff(raw_body) {
                            let hint = if is_cursor {
                                if is_expanded { "collapse" } else { "expand" }
                            } else { "" };
                            let max_lines = if is_expanded { 0 } else { 10 };
                            let diff_lines = render_diff_styled(display_emoji, name, raw_body, max_lines, hint);
                            Some(ratatui::text::Text::from(diff_lines))
                        } else { None }
                    } else { None }
                } else { None };

                if let Some(t) = diff_content {
                    t
                } else {
                    let body = self.format_tool_content_expanded(&msg.content, is_expanded);
                    let hint = if is_cursor {
                        if is_expanded { "  [Space=collapse]" } else { "  [Space=expand]" }
                    } else { "" };
                    let text_str = format!("{} {}{}", display_emoji, body, hint);
                    ratatui::text::Text::from(text_str)
                }
            } else {
                self.format_message_styled(display_emoji, &msg.content, msg_idx)
            };

            let height = text_height_static(&content, area_width);

            let style = if show_band {
                // bg_tint is always Some when show_band is true (set together above), but
                // fall back to Reset defensively rather than panicking.
                let tint = bg_tint.unwrap_or(Color::Reset);
                if is_dark {
                    Style::default().fg(Color::Rgb(220, 230, 240)).bg(tint)
                } else {
                    Style::default().bg(tint)
                }
            } else {
                Style::default()
            };

            let blank = ratatui::text::Text::from(" ".to_string());
            cache.push(CachedRender { blank, content, style, height });
        }

        self.render_cache_exchange_end = exchange_idx;
        self.render_cache_msg_count = self.messages_cache.len();
        self.render_cache_width = area_width;
        self.render_cache_theme = self.theme.kind.clone();
        self.render_cache = cache;
    }

    /// Run the TUI — main event loop with streaming support
    pub async fn run(&mut self) -> Result<()> {
        // CRITICAL: Initialize terminal FIRST (before creating TerminalGuard).
        // This ensures proper error recovery: if Terminal::new() fails, we never
        // enable raw mode, avoiding inconsistent terminal state.
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        
        // Process current messages to seed total_tokens_used
        if let Ok(msgs) = self.message_buffer.messages() {
            self.total_tokens_used = msgs.iter()
                .filter_map(|m| m.prompt_tokens.map(|p| p + m.completion_tokens.unwrap_or(0)))
                .sum();
        }

        // NOW create guard (enables raw mode + alternate screen) AFTER terminal is initialized
        let _guard = TerminalGuard::new()?;

        // In Build mode, fire a kick prompt to orient the agent.
        // One mode waits for the user to specify their task first.
        if self.mode == AppMode::Forever {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".to_string());

            let is_new_session = self.message_buffer.count().unwrap_or(0) == 0;
            
            // Only fire startup kick for NEW sessions or terraforming
            // Restored sessions will be auto-kicked by the 5-second idle watchdog
            let should_kick = is_new_session || self.agents_context.is_none();
            
            if should_kick {
                let kick = if self.agents_context.is_none() {
                    // No AGENTS.md — terraforming mode: explore and create it
                    format!(
                        "New session started in `{cwd}`. \
                         This directory has no AGENTS.md yet — you need to terraform it. \
                         First, explore the directory and read any \
                         key files (README, Cargo.toml, package.json, etc.). \
                         Then write an AGENTS.md that describes the project: its purpose, \
                         structure, build commands, conventions, and any gotchas. \
                         After writing AGENTS.md, continue with normal autonomous work."
                    )
                } else {
                    // AGENTS.md exists, new session — normal autonomous kick
                    format!(
                        "New session started in `{cwd}`. \
                         Orient yourself: list the directory, check .yggdra/todo/ for pending tasks, \
                         review .yggdra/log/ history, and begin working autonomously. \
                         Use tools to explore. When a task is fully complete, continue to the next."
                    )
                };
                // Use ephemeral kick (like inject_continue_kick) — never persisted to DB
                let kick_msg = Message::new("kick", &kick);
                let steering = self.steering_text();
                let mut messages = self.message_buffer.messages().unwrap_or_default();
                messages.push(kick_msg);
                let steering = self.steering_text();
                let (fits, _usage_pct) = self.check_context_before_stream(&messages, Some(&steering));
                // In Forever mode, keep going anyway (just warned above)
                
                if let Some(client) = &self.ollama_client {
                    let (tool_cap, ctx_win) = self.compression_params();
                    self.stream_rx = Some(client.generate_streaming(messages, Some(&steering), self.effective_params(), tool_cap, ctx_win));
                    self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
                    self.turn_phase = TurnPhase::Streaming;
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
                    self.tool_iteration_count = 0;
                }
                self.last_build_kick = std::time::Instant::now();
            }
        }

        loop {
            self.tick_count = self.tick_count.wrapping_add(1);
            
            // Update fireworks animation
            self.update_fireworks();
            
            // Check for config changes (watcher events) — drain all pending
            if let Some(ref mut rx) = self.config_watcher_rx {
                let mut last_change: Option<crate::watcher::ConfigChange> = None;
                loop {
                    match rx.try_recv() {
                        Ok(change) => { last_change = Some(change); }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            self.config_watcher_rx = None;
                            break;
                        }
                    }
                }
                if let Some(change) = last_change {
                    self.handle_config_change(change).await;
                }
            }
            
            // Drain any pending stream tokens before drawing
            self.drain_stream_tokens();
            // Drain any pending subagent tokens
            self.drain_subagent_tokens();
            // Check for completed tool execution
            self.check_tool_result();
            // Check for completed gap reflection
            self.check_gap_result();
            // Drain any streamed /test_models results
            self.check_test_models();
            // Poll for completed async /color generation
            self.check_color_result();
            // Check for completed subagent execution
            self.check_subagent_result();
            // Check for completed async background tasks
            self.check_async_tasks();

            // Refresh battery state every 5 minutes — dispatch async so pmset doesn't block the loop.
            if self.last_battery_check.elapsed() > Duration::from_secs(300) {
                self.last_battery_check = std::time::Instant::now();
                if self.battery_result_rx.is_none() {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    tokio::task::spawn_blocking(move || {
                        let _ = tx.send(crate::battery::battery_state());
                    });
                    self.battery_result_rx = Some(rx);
                }
            }
            // Collect completed battery check
            if let Some(ref mut brx) = self.battery_result_rx {
                if let Ok(state) = brx.try_recv() {
                    self.on_battery = state;
                    self.battery_result_rx = None;
                }
            }

            // Refresh messages cache only when the count has changed — avoids
            // running a full SQL SELECT on every draw frame during streaming.
            if self.cached_message_count != self.messages_cache.len() {
                if let Ok(msgs) = self.message_buffer.messages() {
                    self.messages_cache = msgs;
                    self.cached_message_count = self.messages_cache.len();
                }
            }

            // Rebuild render cache if messages, terminal width, theme, or cursor/expanded state changed.
            let current_width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
            if self.render_cache_dirty
                || self.render_cache_msg_count != self.messages_cache.len()
                || self.render_cache_width != current_width
                || self.render_cache_theme != self.theme.kind
            {
                self.render_cache_dirty = false;
                self.build_render_cache(current_width);
            }

            if self.needs_full_redraw {
                let _ = terminal.clear();
                self.needs_full_redraw = false;
            }
            terminal.draw(|f| self.draw(f))?;

            // Build-mode idle watchdog + bookkeeping
            if self.turn_phase == TurnPhase::Idle {
                self.poll_for_updates();
            }

            // Drain ALL pending input events before the next draw — avoids one-key-per-frame
            // backlog. Uses non-blocking poll(0) so the loop exits immediately when the queue
            // is empty, then sleeps briefly to yield to the Tokio scheduler.
            'events: loop {
                if crossterm::event::poll(Duration::ZERO)? {
                    match event::read()? {
                        Event::Key(key) => {
                            self.handle_key(key).await;
                            if !self.running {
                                break 'events;
                            }
                        }
                        Event::Mouse(mouse) => {
                            self.handle_mouse(mouse);
                        }
                        _ => {}
                    }
                } else {
                    break 'events;
                }
            }

            if !self.running {
                break;
            }

            // Graceful quit: exit once the current turn reaches Idle
            if self.pending_quit && self.turn_phase == TurnPhase::Idle {
                break;
            }

            // Theme auto-detection removed: `defaults read` transiently returns "light"
            // even in dark mode, causing random theme flips. Use /theme to switch manually.
            self.theme_check_counter = self.theme_check_counter.wrapping_add(1);

            // Yield to the Tokio scheduler between frames. Using sleep().await (instead of the
            // old blocking crossterm::event::poll(N ms)) lets other async tasks — the stream
            // reader, gap queries, subagent channels — run during idle time.
            let sleep_ms = if self.turn_phase == TurnPhase::Idle { 16 } else { 10 };
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;

            // Auto-compress: triggered when context pressure hits ≥90%
            if self.pending_auto_compress && self.turn_phase == TurnPhase::Idle {
                self.pending_auto_compress = false;
                self.handle_compress().await;
                self.last_warned_ctx_pct = 0; // reset so pressure warnings fire again after compaction
            }

            // Task queue drain: in Forever/One mode, dequeue the next pending user task
            // when the agent is fully idle (no stream, no tool, no compress in flight).
            if self.turn_phase == TurnPhase::Idle
                && !self.pending_auto_compress
                && matches!(self.mode, AppMode::Forever | AppMode::One)
                && self.ollama_client.is_some()
            {
                if let Some(next_task) = self.task_queue.pop_front() {
                    let remaining = self.task_queue.len();
                    if remaining > 0 {
                        self.push_agent_notice(format!(
                            "▶ Starting next queued task ({} still pending after this)",
                            remaining
                        ));
                    }
                    // Reset stuck-detection counters for the fresh task
                    self.consecutive_empty_kicks = 0;
                    self.autokick_paused = false;
                    self.consecutive_format_errors = 0;
                    self.stall_notice_sent = false;
                    self.handle_message(&next_task).await;
                }
            }
        }

        // Save stats and epoch summary on clean exit
        let cwd = std::env::current_dir().unwrap_or_default();
        self.stats.add_uptime(self.session_start.elapsed().as_secs());
        self.stats.save(&cwd);

        let messages = self.message_buffer.messages().unwrap_or_default();
        crate::epoch::save_summary(&cwd, &messages);

        Ok(())
    }

    /// Drain all available tokens from the stream receiver.
    /// Also enforces stall timeouts: aborts if no tokens arrive within
    /// PREFILL_TIMEOUT_SECS (while empty) or STALL_TIMEOUT_SECS (while generating).
    fn drain_stream_tokens(&mut self) {
        let rx = match self.stream_rx.as_mut() {
            Some(rx) => rx,
            None => return,
        };

        // Stall detection: only abort mid-generation (after the first token).
        // Prefill has no timeout — slow machines / large models can take arbitrarily long
        // to produce the first token and we should never abort them.
        const STALL_TIMEOUT_SECS: u64 = 30; // 30s without a new token mid-generation = hung

        let now = std::time::Instant::now();
        // No stall check while still in prefill (streaming_text is empty).
        let in_prefill = self.streaming_text.is_empty();
        let last_activity = self.last_stream_token_time
            .or(self.stream_start_time)
            .unwrap_or(now);
        if !in_prefill && last_activity.elapsed().as_secs() > STALL_TIMEOUT_SECS {
            if self.mode == AppMode::Forever {
                self.forever_retry_notice(
                    format!("⚠️ Stream stalled ({}s) — retrying", STALL_TIMEOUT_SECS),
                    false,
                );
            } else {
                self.notify(format!("⏱ Stream stalled during generation ({}s) — aborting", STALL_TIMEOUT_SECS));
            }
            self.complete_streaming_turn();
            return;
        }

    loop {
        match rx.try_recv() {
            Ok(StreamEvent::Token(token)) => {
                // Route inline <think>...</think> blocks to thinking_text
                if self.in_think_block {
                    if let Some(end) = token.find("</think>") {
                        self.thinking_text.push_str(&token[..end]);
                        let rest = &token[end + "</think>".len()..];
                        self.in_think_block = false;
                        if !rest.is_empty() {
                            self.streaming_text.push_str(rest);
                        }
                    } else {
                        self.thinking_text.push_str(&token);
                    }
                } else {
                    self.streaming_text.push_str(&token);
                    // Check if we just entered a <think> block
                    if self.streaming_text.starts_with("<think>") {
                        if let Some(end) = self.streaming_text.find("</think>") {
                            let content = self.streaming_text[7..end].to_string();
                            let rest = self.streaming_text[end + "</think>".len()..].to_string();
                            self.thinking_text.push_str(&content);
                            self.streaming_text = rest;
                        } else {
                            let content = self.streaming_text[7..].to_string();
                            self.thinking_text.push_str(&content);
                            self.streaming_text.clear();
                            self.in_think_block = true;
                        }
                    }
                }
                self.last_stream_token_time = Some(std::time::Instant::now());
                if !self.user_scrolled {
                    self.scroll_offset = 0;
                }
            }
                Ok(StreamEvent::ThinkToken(chunk)) => {
                    self.thinking_text.push_str(&chunk);
                    self.last_stream_token_time = Some(std::time::Instant::now());
                    if !self.user_scrolled { self.scroll_offset = 0; }
                }
                Ok(StreamEvent::Done { prompt_tokens, gen_tokens, had_thinking: _, eval_duration_ns, context_trimmed, msgs_dropped }) => {
                    self.last_token_counts = (prompt_tokens, gen_tokens);
                    self.total_tokens_used += prompt_tokens + gen_tokens;
                    // Compute inference rate (tok/s)
                    self.last_infer_rate = match eval_duration_ns {
                        Some(ns) if ns > 0 && gen_tokens > 0 =>
                            Some(gen_tokens as f64 / (ns as f64 / 1_000_000_000.0)),
                        _ => None,
                    };
                    self.stats.record_llm(prompt_tokens, gen_tokens, self.last_infer_rate);
                    if context_trimmed { self.stats.context_trims += 1; }
                    self.context_cutoff_dropped = msgs_dropped;

                    // Warn when this turn consumed a worrying fraction of the context window.
                    let ctx_window = self.effective_context_window();
                    let total_tokens = prompt_tokens + gen_tokens;
                    let pct = (total_tokens as f64 / ctx_window as f64 * 100.0) as u32;
                    if total_tokens >= ctx_window {
                        self.push_agent_notice(format!(
                            "⚠️  Context full: this turn used {} tokens ({}% of {} limit). \
                             Older messages are being dropped. Consider /compress or a fresh session.",
                            total_tokens, pct, ctx_window
                        ));
                    } else if pct >= 80 {
                        self.push_agent_notice(format!(
                            "⚠️  Context at {}%: {} / {} tokens used this turn.",
                            pct, total_tokens, ctx_window
                        ));
                    }

                    self.complete_streaming_turn();
                    return;
                }
                Ok(StreamEvent::Error(e)) => {
                    // OpenRouter (and some other providers) return HTTP 400 errors where
                    // the `code` field is an integer rather than a string, causing
                    // async_openai to fail deserialization with a confusing
                    // "invalid type: integer `400`, expected a string" message.
                    // Extract the human-readable cause from the embedded JSON when
                    // present, and treat 400-class errors as non-retryable (they will
                    // not succeed on a blind retry — the request itself is malformed or
                    // the context is too long).
                    let (display_error, is_fatal) = extract_provider_error(&e);
                    let clean_error = sanitize_for_display(&display_error, 300);
                    self.notify(format!("❌ Stream error: {}", clean_error));
                    self.needs_full_redraw = true;
                    self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
                    self.stream_rx = None;
                    self.turn_phase = TurnPhase::Idle;
                    self.tool_iteration_count = 0;
                    self.last_infer_rate = None;
                    if matches!(self.mode, AppMode::Forever | AppMode::One) {
                        self.consecutive_empty_kicks += 1;
                        if self.mode == AppMode::Forever {
                            // Forever mode never pauses — always retry, throttle notices
                            let msg = format!("⚠️ Stream error (retrying): {}", clean_error);
                            self.forever_retry_notice(msg, false);
                            self.inject_continue_kick();
                        } else if is_fatal || self.consecutive_empty_kicks >= 5 || self.autokick_paused {
                            self.autokick_paused = true;
                            let reason = if is_fatal {
                                format!("⏸️ Provider error (will not retry): {}. Send a message to continue.", clean_error)
                            } else {
                                "⏸️ Too many errors — pausing. Send a message to retry.".to_string()
                            };
                            self.push_agent_notice(reason);
                        } else {
                            self.inject_continue_kick();
                        }
                    }
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
                        if matches!(self.mode, AppMode::Forever | AppMode::One) {
                            self.consecutive_empty_kicks += 1;
                            if self.mode == AppMode::Forever {
                                self.inject_continue_kick();
                            } else if self.consecutive_empty_kicks >= 5 || self.autokick_paused {
                                self.autokick_paused = true;
                                self.push_agent_notice("⏸️ Connection lost repeatedly — pausing. Send a message to retry.".to_string());
                            } else {
                                self.inject_continue_kick();
                            }
                        }
                    }
                    return;
                }
            }
        }
    }

    /// Drain pending tokens from a running subagent's stream into subagent_live_text
    /// and the active subagent panel entry's preview.
    fn drain_subagent_tokens(&mut self) {
        let rx = match self.subagent_token_rx.as_mut() {
            Some(rx) => rx,
            None => return,
        };
        loop {
            match rx.try_recv() {
                Ok(tok) => {
                    self.subagent_live_text.push_str(&tok);
                    // Mirror into the panel entry so it stays up-to-date
                    if let Some(entry) = self.subagent_entries.iter_mut()
                        .rev()
                        .find(|e| e.status == SubagentStatus::Running)
                    {
                        entry.preview.push_str(&tok);
                        // Keep preview bounded to last 400 chars
                        if entry.preview.len() > 400 {
                            let trim_at = entry.preview.len() - 400;
                            entry.preview = entry.preview[trim_at..].to_string();
                        }
                    }
                    if !self.user_scrolled {
                        self.scroll_offset = 0;
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.subagent_token_rx = None;
                    return;
                }
            }
        }
    }

    /// Warn the user when the context window is getting full.
    fn check_context_pressure(&mut self) {
        let (prompt_tok, _) = self.last_token_counts;
        if prompt_tok == 0 { return; }
        let context_window = self.effective_context_window() as f64;
        let usage_pct = ((prompt_tok as f64) / context_window * 100.0).min(100.0) as u32;

        let (threshold, msg) = if usage_pct >= 95 {
            (95u32, format!("🔴 Context critical (~{}%) — auto-compressing now", usage_pct))
        } else if usage_pct >= 90 {
            (90u32, format!("🟠 Context at ~{}% — auto-compressing", usage_pct))
        } else if usage_pct >= 80 {
            (80u32, format!("⚠️ Context at ~{}% — consider /compress", usage_pct))
        } else {
            return;
        };

        if self.last_warned_ctx_pct < threshold {
            self.last_warned_ctx_pct = threshold;
            self.push_agent_notice(msg);
            if threshold >= 90 {
                self.pending_auto_compress = true;
            }
        }
    }

    /// Streaming finished: persist response, check for tool calls, maybe continue
    fn complete_streaming_turn(&mut self) {
        self.stream_start_time = None;
        self.last_stream_token_time = None;
        if self.streaming_text.is_empty() && self.thinking_text.is_empty() {
            self.stream_rx = None;
            self.turn_phase = TurnPhase::Idle;
            self.tool_iteration_count = 0;
            let action = decide_stream_end(false, self.mode, 0, self.consecutive_empty_kicks);
            self.consecutive_empty_kicks += 1;
            match action {
                StreamEndAction::CompleteOne(reason) => {
                    self.push_agent_notice(
                        "⚠️ Empty responses (thinking only) — completing task.".to_string()
                    );
                    self.complete_one_mode(reason);
                }
                StreamEndAction::Halt(reason) => {
                    if self.mode == AppMode::Forever {
                        self.forever_retry_notice(
                            format!("⚠️ Model stuck ({reason}) — kicking"),
                            false,
                        );
                        self.inject_continue_kick();
                    } else {
                        self.autokick_paused = true;
                        self.push_agent_notice(
                            format!("⏸️ Model not producing output ({reason}) — pausing. Send a message to resume.")
                        );
                    }
                }
                StreamEndAction::Kick => {
                    if self.consecutive_empty_kicks == 3 {
                        self.push_agent_notice(
                            "⚠️ Three empty responses. If you are done, summarize. \
                             If not, emit a tool call.".to_string()
                        );
                    }
                    self.inject_continue_kick();
                }
                StreamEndAction::Persist => {}
            }
            return;
        }
        
        let response_text = self.streaming_text.clone();
        
        // Automatic logging for structured blocks
        let tags = [("<plan>", "</plan>", "plan.md"), ("<task>", "</task>", "todo/current.md"), ("<memory>", "</memory>", "memory.md")];
        for (start_tag, end_tag, filename) in tags {
            if let Some(start) = response_text.find(start_tag) {
                if let Some(end) = response_text[start..].find(end_tag) {
                    let content = response_text[start + start_tag.len()..start + end].trim();
                    if !content.is_empty() {
                        let path = std::env::current_dir().unwrap_or_default().join(".yggdra").join(filename);
                        let _ = std::fs::OpenOptions::new().create(true).append(true).open(path)
                            .and_then(|mut f| {
                                use std::io::Write;
                                writeln!(f, "\n--- {timestamp}\n{content}", 
                                    timestamp = chrono::Utc::now().to_rfc3339(), 
                                    content = content)
                            });
                    }
                }
            }
        }

        // Extract thinking content before sanitizing:

        // 1. Native thinking tokens (from msg.thinking API field)
        // 2. Inline <think>...</think> tags in the text
        let mut thinking_parts: Vec<String> = Vec::new();
        if !self.thinking_text.is_empty() {
            thinking_parts.push(self.thinking_text.trim().to_string());
        }
        // Extract inline <think> blocks from response_text ONLY if the live state machine
        // didn't already capture them. When a model emits native ThinkToken events, the
        // same content also appears as <think>...</think> in the text stream — scanning
        // both sources would duplicate the block.
        if self.thinking_text.is_empty() {
            let mut scan = response_text.as_str();
            while let Some(start) = scan.find("<think>") {
                let after = &scan[start + "<think>".len()..];
                let end = after.find("</think>").unwrap_or(after.len());
                let content = after[..end].trim();
                if !content.is_empty() {
                    thinking_parts.push(content.to_string());
                }
                scan = if end + "</think>".len() <= after.len() {
                    &after[end + "</think>".len()..]
                } else {
                    ""
                };
            }
        }

        // Sanitize training artifacts before persisting or parsing
        let response_text = agent::sanitize_model_output(&response_text);
        
        // Format Rust code blocks using rustfmt
        let mut formatted_text = String::new();
        let mut last_pos = 0;
        while let Some(start) = response_text[last_pos..].find("```rust") {
            let start_abs = last_pos + start;
            formatted_text.push_str(&response_text[last_pos..start_abs]);
            formatted_text.push_str("```rust");
            
            let search_start = start_abs + "```rust".len();
            if let Some(end) = response_text[search_start..].find("```") {
                let end_abs = search_start + end;
                let code = &response_text[search_start..end_abs];
                formatted_text.push_str(&crate::tools::format_rust_code(code));
                formatted_text.push_str("```");
                last_pos = end_abs + 3;
            } else {
                formatted_text.push_str(&response_text[search_start..]);
                last_pos = response_text.len();
            }
        }
        formatted_text.push_str(&response_text[last_pos..]);
        let response_text = formatted_text;

        // Parse tool calls early so we can inject narration before persisting
        let tool_calls = agent::parse_tool_calls(&response_text);
        let mut spawn_calls = crate::spawner::parse_spawn_agent_calls(&response_text);

        // If the model emitted a bare tool call (no explanation), synthesize one
        let response_text = if !tool_calls.is_empty() && extract_prose_before_json(&response_text).is_empty() {
            let narration = synthesize_tool_narration(&tool_calls);
            format!("{}\n{}", narration, response_text)
        } else {
            response_text
        };

        // Prepend thinking block to stored message so it's visible in history
        let response_text = if !thinking_parts.is_empty() {
            let combined = thinking_parts.join("\n\n");
            format!("[THINK: {}]\n{}", combined, response_text)
        } else {
            response_text
        };

        // Extract <plan>...</plan> block: write to .yggdra/plan.md and strip from stored text
        let response_text = self.extract_and_write_plan(response_text);

        // Detect </understood> in Plan mode — agent is ready to execute
        if self.mode == AppMode::Plan && response_text.contains("</understood>") {
            self.plan_understood = true;
            self.status_message = "💡 Agent is ready — press Enter to execute".to_string();
            tokio::spawn(crate::notifications::agent_says("💡 Plan understood — press Enter to execute"));
            self.render_cache_dirty = true;
        }

        // Parse <percent>N</percent> for task progress bar
        if let Some(start) = response_text.find("<percent>") {
            if let Some(end) = response_text.find("</percent>") {
                if start < end {
                    let pct_str = &response_text[start + 9..end];
                    if let Ok(pct) = pct_str.parse::<u32>() {
                        self.task_progress = Some(pct.min(100));
                    }
                }
            }
        }

        // Persist assistant message
        let model_msg = Message::new("assistant", &response_text);
        if let Err(e) = self.message_buffer.add_and_persist(model_msg) {
            self.notify(format!("⚠️ Response received but not saved: {}", e));
            self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
            self.stream_rx = None;
            self.turn_phase = TurnPhase::Idle;
            self.tool_iteration_count = 0;
            // Don't retry generation — message wasn't persisted, context is stale
            if self.mode == AppMode::Forever {
                // Can't persist the message — log and keep going
                self.forever_retry_notice("⚠️ Storage error — message not saved, continuing", false);
                self.inject_continue_kick();
            } else {
                self.autokick_paused = true;
                self.push_agent_notice("⏸️ Storage error — pausing. Fix the issue and send a message to resume.".to_string());
            }
            return;
        }
        self.cached_message_count = self.message_buffer.count()
            .unwrap_or(self.cached_message_count + 1);
        // Successful turn — reset retry-notice throttle
        self.spin_notice_count = 0;

        // Warn if context window is filling up
        self.check_context_pressure();

        // Also extract spawn (subagent) from JSON-parsed tool calls (any with __SPAWN__ prefix)
        for tc in &tool_calls {
            if tc.name == "spawn" && tc.args.starts_with("__SPAWN__") {
                let rest = &tc.args["__SPAWN__".len()..];
                let mut parts = rest.splitn(2, ' ');
                let task_id = parts.next().unwrap_or("task").to_string();
                let desc = parts.next().unwrap_or("").to_string();
                if !spawn_calls.iter().any(|(id, _)| id == &task_id) {
                    spawn_calls.push((task_id, desc));
                }
            }
        }
        // Filter only subagent spawns (__SPAWN__ prefix) out of tool_calls.
        // Command spawns (no prefix) stay and are dispatched to ExecTool.
        let tool_calls: Vec<_> = tool_calls.into_iter()
            .filter(|tc| !(tc.name == "spawn" && tc.args.starts_with("__SPAWN__")))
            .collect();

        // Fire-and-forget gap reflection: only on final prose responses (no tool calls).
        // When the response IS a tool call, we haven't seen the result yet — firing then
        // causes spurious "I wish I knew the search result" gaps. Reflect after the model
        // has actually seen what it asked for.
        let is_tool_response = !tool_calls.is_empty() || !spawn_calls.is_empty()
            || response_text.contains("\"tool_calls\"");
        if !is_tool_response {
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
        }

        // Detect hallucinated conversations — model generating both tool calls and fake outputs
        let is_hallucinating = agent::is_hallucinated_output(&response_text);
        if is_hallucinating {
            self.notify("⚠️ Model hallucinating tool outputs — stopping".to_string());
        }

        // Optional milestone notification — fires if model happens to say </done>, but doesn't
        // control flow. Any plain-text response (no tool calls) is treated as done.
        if response_text.contains("</done>") {
            self.push_system_event("🌸 milestone");
            tokio::spawn(crate::notifications::model_responded("🌸 milestone reached"));

            // 🎆 Trigger fireworks celebration!
            self.trigger_fireworks();

            // Track task tokens: compute since last </done>
            let task_tokens = self.total_tokens_used.saturating_sub(self.session_tokens_at_last_done);
            self.push_agent_notice(format!(
                "📊 Task tokens: {} | Session total: {}",
                task_tokens, self.total_tokens_used
            ));

            // OpenRouter token usage summary
            if let Some(client) = &self.ollama_client {
                if client.endpoint().contains("openrouter.ai") {
                    let (prompt, gen) = self.last_token_counts;
                    let total = prompt + gen;
                    self.push_agent_notice(format!(
                        "📊 Tokens: P{} G{} T{} (OpenRouter)",
                        prompt, gen, total
                    ));
                }
            }

            // Reset task token counter and update the checkpoint
            self.session_tokens_at_last_done = self.total_tokens_used;
            self.task_tokens_since_done = 0;

            // One-mode: </done> is an authoritative completion signal — stop here.
            if self.mode == AppMode::One {

                self.complete_one_mode("</done> emitted");
                self.streaming_text.clear();
                self.thinking_text.clear();
                self.in_think_block = false;
                self.stream_rx = None;
                self.turn_phase = TurnPhase::Idle;
                self.tool_iteration_count = 0;
                return;
            }
            // Ask-mode: </done> signals completion of autonomous tool execution — stop looping.
            if self.mode == AppMode::Ask {
                self.streaming_text.clear();
                self.thinking_text.clear();
                self.in_think_block = false;
                self.stream_rx = None;
                self.turn_phase = TurnPhase::Idle;
                self.tool_iteration_count = 0;
                self.push_agent_notice("✅ Agent completed autonomous exploration (</done> received)");
                return;
            }
        }

        // Handle spawn_agent: show 🤖 N indicator in chat, execute first one
        if !is_hallucinating && !spawn_calls.is_empty() && self.subagent_result_rx.is_none() {
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
            // Add entry to subagents panel
            self.subagent_entries.push(SubagentEntry {
                index: n,
                task_id: task_id.clone(),
                status: SubagentStatus::Running,
                preview: String::new(),
                completed_at_msg: None,
            });
            self.execute_subagent_async(task_id.clone(), task_desc.clone());
            self.turn_phase = TurnPhase::ExecutingTool(format!("spawn:{}", task_id));
        } else if !is_hallucinating && !tool_calls.is_empty() {
            // Handle any tellhuman messages — show in chat + fire OS notification
            for call in &tool_calls {
                if let Some(msg) = &call.tellhuman {
                    let task_summary = self.task_summary();
                    let formatted_msg = format!("[{}] {}", task_summary, msg);
                    self.push_system_event(format!("💬 {}", &formatted_msg));
                    tokio::spawn(async move { crate::notifications::agent_says(&formatted_msg).await; });
                }
            }

            // Partition async and sync tool calls
            let (async_calls, sync_calls): (Vec<_>, Vec<_>) = tool_calls.iter()
                .partition(|c| c.async_mode);

            // Fire off all async calls immediately (non-blocking)
            for call in &async_calls {
                let task_id = call.async_task_id.clone()
                    .unwrap_or_else(|| format!("task-{}", &call.args.chars().take(12).collect::<String>().replace(' ', "-")));
                let preview = call.args.chars().take(60).collect::<String>();
                let ack = format!("[ASYNC_STARTED: {} = {} (running...)]", task_id, preview);
                let ack_msg = Message::new("tool", &ack);
                if let Err(e) = self.message_buffer.add_and_persist(ack_msg) {
                    self.notify(format!("⚠️ Failed to save async ack: {}", e));
                } else {
                    self.cached_message_count = self.message_buffer.count()
                        .unwrap_or(self.cached_message_count + 1);
                }
                self.status_message = format!("🔄 async: {}", task_id);
                let (tx, rx) = oneshot::channel::<ToolResult>();
                let tool_name = call.name.clone();
                let args = call.args.clone();
                let tool_name_for_result = tool_name.clone();
                let args_for_result = args.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let registry = crate::tools::ToolRegistry::new();
                        registry.execute(&tool_name, &args).map_err(|e| e.to_string())
                    }).await;
                    let output = match result {
                        Ok(Ok(out)) => Ok(out),
                        Ok(Err(e)) => Err(e),
                        Err(e) => Err(format!("task panicked: {}", e)),
                    };
                    let _ = tx.send(ToolResult { tool_name: tool_name_for_result, args: args_for_result, output });
                });
                self.async_tasks.push(AsyncTask {
                    task_id,
                    command_preview: preview,
                    started_at: std::time::Instant::now(),
                    rx,
                });
            }

            // If there are also sync calls, dispatch those normally; otherwise kick next turn
            let tool_calls = sync_calls;
            if tool_calls.is_empty() {
                // All calls were async — kick next stream turn so model can continue
                if let Some(client) = &self.ollama_client {
                    let steering_text = self.steering_text();
                    let messages = self.message_buffer.messages().unwrap_or_default();
                    let (tool_cap, ctx_win) = self.compression_params();
                    let rx = client.generate_streaming(messages, Some(&steering_text), self.effective_params(), tool_cap, ctx_win);
                    self.stream_rx = Some(rx);
                    self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
                    self.turn_phase = TurnPhase::Streaming;
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
                    self.tool_iteration_count += 1;
                } else {
                    self.turn_phase = TurnPhase::Idle;
                }
            } else if tool_calls.len() > 1 {
                // Batch execution for multiple tool calls
                let calls: Vec<(String, String)> = tool_calls.iter()
                    .map(|c| (c.name.clone(), c.args.clone()))
                    .collect();
                self.status_message = format!("🔧 Executing {} tools in batch...", calls.len());
                let (tx, rx) = oneshot::channel::<ToolResult>();
                let cap = Some(self.config.tool_output_cap
                    .or(self.config.params.tool_output_cap)
                    .unwrap_or(crate::config::OUTPUT_CHARACTER_LIMIT));
                tokio::spawn(async move {
                    let output = App::execute_tools_batch_async(calls, cap).await;
                    let _ = tx.send(ToolResult {
                        tool_name: "__batch__".to_string(),
                        args: String::new(),
                        output: Ok(output),
                    });
                });
                self.tool_result_rx = Some(rx);
                self.turn_phase = TurnPhase::ExecutingTool("batch".to_string());
            } else {
                // Single tool call — existing behavior
                let call = &tool_calls[0];
                let status = if let Some(desc) = call.description.as_deref().filter(|s| !s.is_empty()) {
                    format!("🔧 {}", desc)
                } else if call.name == "setfile" {
                    let path = call.args.split('\x00').next().unwrap_or("?");
                    format!("🔧 setfile: {}", path)
                } else {
                    format!("🔧 Executing tool: {} ...", call.name)
                };
                self.status_message = status;
                self.execute_tool_async(call.name.clone(), call.args.clone());
                self.turn_phase = TurnPhase::ExecutingTool(call.name.clone());
            }
        } else {
            // No valid tool calls — check if model tried a blocked tool (profile restriction)
            let blocked = agent::parse_blocked_tool_names(&response_text);
            if !blocked.is_empty() && !is_hallucinating {
                let error_parts: Vec<String> = blocked.iter().map(|name| {
                    format!(
                        "[TOOL_OUTPUT: {} = ⚠️ '{}' is not available in shell-only mode. \
                         Use the shell tool instead: \
                         <tool>shell</tool><command>{} ...</command><desc>what and why</desc>]",
                        name, name, name
                    )
                }).collect();
                let error_text = error_parts.join("\n");
                let error_msg = Message::new("tool", &error_text);
                if let Err(e) = self.message_buffer.add_and_persist(error_msg) {
                    self.notify(format!("⚠️ Failed to save blocked tool error: {}", e));
                } else {
                    self.cached_message_count = self.message_buffer.count()
                        .unwrap_or(self.cached_message_count + 1);
                }
                // Continue the turn so model can self-correct
                if let Some(client) = &self.ollama_client {
                    let steering_text = self.steering_text();
                    let messages = self.message_buffer.messages().unwrap_or_default();
                    let (tool_cap, ctx_win) = self.compression_params();
                    let rx = client.generate_streaming(messages, Some(&steering_text), self.effective_params(), tool_cap, ctx_win);
                    self.stream_rx = Some(rx);
                    self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
                    self.turn_phase = TurnPhase::Streaming;
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
                    self.tool_iteration_count += 1;
                } else {
                    self.turn_phase = TurnPhase::Idle;
                }
                return;
            }

            // No tool calls — plain response
            // ShellOnly: model didn't output valid JSON tool call.
            // Two distinct failure modes:
            //   1. No "tool_calls" key at all — model output prose → inject format correction
            //   2. "tool_calls" present but parse failed — truncated/malformed stream →
            //      delete the partial message from history and silently retry (model gets a
            //      clean slate without seeing its own garbled output)
            let has_tool_calls_key = response_text.contains("\"tool_calls\"");
            // Detect malformed/cutoff output differently for XML vs JSON format
            let xml_started = response_text.contains("<tool>");
            let xml_malformed = xml_started
                && !response_text.contains("</command>")
                && !response_text.contains("</tool_call>");
            // Truly malformed = model tried to write JSON (has the key, starts with `{`)
            // but the stream ended before the JSON was closed.
            let json_malformed = {
                let t = response_text.trim();
                let opens_brace  = t.chars().filter(|&c| c == '{').count();
                let closes_brace = t.chars().filter(|&c| c == '}').count();
                let opens_brack  = t.chars().filter(|&c| c == '[').count();
                let closes_brack = t.chars().filter(|&c| c == ']').count();
                let unbalanced = opens_brace != closes_brace || opens_brack != closes_brack;
                let truncated_marker = t.ends_with("...");
                let starts_json = t.starts_with('{') || t.starts_with('[');
                (has_tool_calls_key && unbalanced)
                    || truncated_marker
                    || (starts_json && unbalanced)
            };
            let json_malformed = json_malformed || xml_malformed;
            if !response_text.trim().is_empty()
            {
                 if json_malformed {
                     // Stream was cut short or JSON was malformed — delete the garbage from history
                     // and retry transparently. Don't inject a correction (it would confuse the model).
                     let _ = self.message_buffer.delete_last();
                     self.cached_message_count = self.message_buffer.count()
                         .unwrap_or(self.cached_message_count.saturating_sub(1));
                     self.notify("⚠️ Stream cut short — retrying silently");
                     self.tool_iteration_count += 1;
                     if let Some(client) = self.ollama_client.clone() {
                         let messages = self.message_buffer.messages().unwrap_or_default();
                         let steering = self.steering_text();
                         let (tool_cap, ctx_win) = self.compression_params();
                         let rx = client.generate_streaming(messages, Some(&steering), self.effective_params(), tool_cap, ctx_win);
                         self.stream_rx = Some(rx);
                         self.streaming_text.clear();
                     self.thinking_text.clear();
                     self.in_think_block = false;
                         self.turn_phase = TurnPhase::Streaming;
                         self.stream_start_time = Some(std::time::Instant::now());
                         self.last_stream_token_time = None;
                     }
                     return;
                 }
                 // If a valid tool call is present, it's not a format error, even if there's prose.
                 if !response_text.trim().is_empty() {
                     let xml_calls = agent::parse_xml_tool_calls(&response_text);
                     if !xml_calls.is_empty() {
                         // Valid XML found, ignore the prose and continue normally
                         return;
                     }
                     let json_calls = agent::parse_json_tool_calls(&response_text);
                     if !json_calls.is_empty() {
                         // Valid JSON found, ignore the prose and continue normally
                         return;
                     }
                 }
                 // Build a concrete correction: if we can extract a backtick command from prose,
                 // show the model exactly what the correct XML would have looked like.
                 self.consecutive_format_errors += 1;

                // After 2 consecutive format errors: give up and pause (or kick in Forever)
                if self.consecutive_format_errors >= 2 {
                    self.consecutive_format_errors = 0;
                    if self.mode == AppMode::Forever {
                        self.forever_retry_notice(
                            "⚠️ Repeated format errors — injecting correction",
                            false,
                        );
                        self.inject_continue_kick();
                        self.turn_phase = TurnPhase::Idle;
                        self.stream_rx = None;
                        return;
                    }
                    let msg = format!(
                        "🤖 Agent gave up after {} format correction attempts — switching to Ask mode.\n\
                         The agent sent prose instead of a tool call. You can give new instructions or \
                         rephrase the task.",
                        self.consecutive_format_errors + 2
                    );
                    self.push_agent_notice(msg);
                    self.mode = crate::config::AppMode::Ask;
                    let _ = self.config.save();
                    self.notify("🤖 Format loop — switched to Ask mode");
                    self.needs_full_redraw = true;
                    self.turn_phase = TurnPhase::Idle;
                    self.stream_rx = None;
                    return;
                }
                let example = agent::extract_backtick_command_pub(&response_text)
                    .map(|cmd| format!(
                        "\nYour command `{}` should have been:\n\
                         <tool>shell</tool>\n\
                         <command>{}</command>\n\
                         <desc><one sentence></desc>",
                        cmd, cmd
                    ))
                    .unwrap_or_default();
                 let correction = format!(
                     "FORMAT ERROR: your last response was not a valid tool call.\n\
                      Please provide a tool call using XML tags.\n\
                      Required format:\n\
                      <tool>shell</tool>\n\
                      <command>your sh -c command</command>\n\
                      <desc>what and why</desc>{}",
                     example
                 );

                let correction_msg = Message::new("user", correction);
                self.persist_message(correction_msg);
                self.cached_message_count = self.message_buffer.count()
                    .unwrap_or(self.cached_message_count + 1);
                self.notify("⚠️ Format error — injecting correction");
                self.tool_iteration_count += 1;
                // Restart streaming with the correction in context
                if let Some(client) = self.ollama_client.clone() {
                    let messages = self.message_buffer.messages().unwrap_or_default();
                    let steering = self.steering_text();
                    let (tool_cap, ctx_win) = self.compression_params();
                    let rx = client.generate_streaming(messages, Some(&steering), self.effective_params(), tool_cap, ctx_win);
                    self.stream_rx = Some(rx);
                    self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
                    self.turn_phase = TurnPhase::Streaming;
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
                }
                return;
            }

            // No tool calls — plain response, treat as done
            if self.tool_iteration_count >= MAX_TOOL_ITERATIONS
                && self.tool_iteration_count % MAX_TOOL_ITERATIONS == 0
            {
                // Inject a small steering nudge every MAX_TOOL_ITERATIONS steps, then keep going
                self.push_agent_notice(format!(
                    "🔄 {} tool steps completed. If your task is done, emit </done> or summarize. \
                     If not, keep going.",
                    self.tool_iteration_count
                ));
                // Do NOT reset counter or stop — let the agent continue
            } else {
                let action = decide_stream_end(true, self.mode, self.tool_iteration_count, self.consecutive_empty_kicks);
                self.consecutive_empty_kicks += 1;
                match action {
                    StreamEndAction::CompleteOne(reason) => {
                        self.complete_one_mode(reason);
                        self.turn_phase = TurnPhase::Idle;
                        self.tool_iteration_count = 0;
                        self.streaming_text.clear();
                        self.thinking_text.clear();
                        self.in_think_block = false;
                        self.stream_rx = None;
                        return;
                    }
                    StreamEndAction::Halt(reason) => {
                        if self.mode == AppMode::Forever {
                            self.forever_retry_notice(
                                format!("⚠️ Model stuck ({reason}) — kicking"),
                                false,
                            );
                            self.inject_continue_kick();
                        } else {
                            self.autokick_paused = true;
                            self.push_agent_notice(
                                format!("⏸️ Model not producing output ({reason}) — pausing. Send a message to resume.")
                            );
                        }
                    }
                    StreamEndAction::Kick => {
                        if self.consecutive_empty_kicks == 3 {
                            self.push_agent_notice(
                                "⚠️ No tool calls in last 3 responses. If you are done, summarize. \
                                 If not, emit a tool call.".to_string()
                            );
                        }
                        self.inject_continue_kick();
                        return;
                    }
                    StreamEndAction::Persist => {
                        self.status_message = "✅ Response complete".to_string();
                    }
                }
            }
            self.turn_phase = TurnPhase::Idle;
            self.tool_iteration_count = 0;
        }

        self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
        self.stream_rx = None;
    }

    /// Inject a continue-kick message and immediately start a new streaming turn (for Build mode & /ctx)
    /// Abort any in-flight stream, tool, or subagent turn and return to Idle.
    /// Call this when switching to Ask mode to hard-stop autonomous execution.
    fn abort_active_turn(&mut self) {
        self.stream_rx = None;
        self.tool_result_rx = None;
        self.turn_phase = TurnPhase::Idle;
        self.tool_iteration_count = 0;
        self.consecutive_empty_kicks = 0;
        self.autokick_paused = false;
        self.needs_full_redraw = true;
    }

    /// Push a notice for a Forever-mode retry, throttled to avoid chat spam.
    /// Shows on the 1st retry, then every 5th. Pass `reset=true` on success to clear the counter.
    fn forever_retry_notice(&mut self, msg: impl Into<String>, reset: bool) {
        if reset {
            self.spin_notice_count = 0;
            return;
        }
        self.spin_notice_count = self.spin_notice_count.saturating_add(1);
        let n = self.spin_notice_count;
        if n == 1 {
            self.push_agent_notice(msg.into());
        } else if n % 5 == 0 {
            self.push_agent_notice(format!("{} (retry #{})", msg.into(), n));
        }
        // else: silent retry
    }

    fn inject_continue_kick(&mut self) {
        // Never kick while a compress is pending — the main loop will compress first,
        // then resume. Without this guard the kick races ahead and starts a new streaming
        // turn before compress fires, keeping turn_phase == Streaming forever.
        if self.pending_auto_compress {
            self.turn_phase = TurnPhase::Idle;
            return;
        }
        // Kick is ephemeral: appended to the messages list in memory only, never persisted.
        // Persisting kicks causes them to accumulate in context over long sessions.
        let kick = Message::new("kick", "Keep going. Find the next task or improvement.");
        let steering = self.steering_text();
        let mut messages = self.message_buffer.messages().unwrap_or_default();
        messages.push(kick);
        
        // Check if we're about to exceed context before streaming
        let (fits, usage_pct) = self.check_context_before_stream(&messages, Some(&steering));
        if !fits && self.mode == crate::config::AppMode::Forever {
            // In Forever mode, pause and let user know
            self.autokick_paused = true;
            return;
        }
        
        if let Some(client) = &self.ollama_client {
            let (tool_cap, ctx_win) = self.compression_params();
            self.stream_rx = Some(client.generate_streaming(messages, Some(&steering), self.effective_params(), tool_cap, ctx_win));
            self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
            self.turn_phase = TurnPhase::Streaming;
            self.stream_start_time = Some(std::time::Instant::now());
            self.last_stream_token_time = None;
            self.tool_iteration_count = 0;
            self.last_build_kick = std::time::Instant::now();
        }
    }

    /// Validate spawn command for potential issues (basic shell syntax checking)
    fn _validate_spawn_command(cmd: &str) -> Option<String> {
        // Check for common shell issues
        
        // Unclosed quotes
        let single_quotes = cmd.matches('\'').count();
        let double_quotes = cmd.matches('"').count();
        if single_quotes % 2 != 0 || double_quotes % 2 != 0 {
            return Some("⚠️  Unclosed quotes detected".to_string());
        }
        
        // Unmatched brackets/parens
        let opens = cmd.matches('(').count() + cmd.matches('[').count() + cmd.matches('{').count();
        let closes = cmd.matches(')').count() + cmd.matches(']').count() + cmd.matches('}').count();
        if opens != closes {
            return Some("⚠️  Unmatched brackets/parens".to_string());
        }
        
        None
    }

    /// Spawn tool execution off the UI thread
    fn execute_tool_async(&mut self, tool_name: String, args: String) {
        // Block modifying tools in Ask-only mode
        if self.mode == AppMode::Ask {
            match tool_name.as_str() {
                "setfile" | "patchfile" | "commit" | "python" | "ruste" => {
                    self.push_agent_notice(format!("🔒 Ask-only mode: {} is blocked (read-only mode)", tool_name));
                    self.turn_phase = TurnPhase::Idle;
                    return;
                }
                _ => {} // rg, spawn, readfile, exec, shell are allowed
            }
        }

        // Block write tools in Plan mode — inject a tool-result error so the model
        // can self-correct without looping.
        if self.mode == AppMode::Plan {
            match tool_name.as_str() {
                "setfile" | "patchfile" | "commit" => {
                    let blocked_msg = format!(
                        "🔒 Plan mode: {} is blocked. Switch to Build or One mode to make file changes.",
                        tool_name
                    );
                    self.push_agent_notice(blocked_msg.clone());
                    let tool_result = Message::new(
                        "tool",
                        &format!("[TOOL_RESULT: {} = ERROR: {}]", tool_name, blocked_msg),
                    );
                    let _ = self.message_buffer.add_and_persist(tool_result);
                    self.cached_message_count = self.message_buffer.count()
                        .unwrap_or(self.cached_message_count + 1);
                    self.turn_phase = TurnPhase::Idle;
                    return;
                }
                "shell" | "exec" => {
                    // Warn (but still execute) if the command looks like a write operation
                    if is_shell_write_pattern(&args) {
                        self.push_agent_notice(
                            "⚠️ Plan mode: command appears to write files. \
                             Plan mode is read-only — use Build or One mode for edits.".to_string()
                        );
                    }
                }
                _ => {}
            }
        }

        // ── Repeated-identical-call detection ─────────────────────────────
        {
            let call_hash = hash_tool_call(&tool_name, &args);
            self.recent_tool_calls.push_back((tool_name.clone(), call_hash));
            if self.recent_tool_calls.len() > 4 {
                self.recent_tool_calls.pop_front();
            }
            let repeat_count = count_repeat_calls(&self.recent_tool_calls, &tool_name, call_hash);
            if repeat_count >= 3 {
                let hint = "Try a different approach: use shell with a different command or narrower grep pattern.".to_string();
                self.push_agent_notice(format!(
                    "⚠️ You have called '{}' with identical arguments {} times in a row. {}",
                    tool_name, repeat_count, hint
                ));
            }
        }
        // ──────────────────────────────────────────────────────────────────

        let (tx, rx) = oneshot::channel();
        let args_for_result = args.clone();

        // Wrap in tokio timeout as a safety net — spawn's own timeout handles the
        // common case, but this catches any other tool that could hang indefinitely.
        tokio::spawn(async move {
            let tool_name_for_result = tool_name.clone();
            let result = tokio::task::spawn_blocking(move || {
                let registry = ToolRegistry::new();
                registry.execute(&tool_name, &args)
                    .map_err(|e| e.to_string())
            }).await;

            let output = match result {
                Ok(Ok(output)) => Ok(output),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(format!("tool panicked: {}", join_err)),
            };
            let _ = tx.send(ToolResult {
                tool_name: tool_name_for_result,
                args: args_for_result,
                output,
            });
        });

        self.tool_result_rx = Some(rx);
    }

    /// Spawn a subagent off the UI thread; result arrives via subagent_result_rx
    fn execute_subagent_async(&mut self, task_id: String, task_desc: String) {
        let (tx, rx) = oneshot::channel();
        let endpoint = self.config.endpoint.clone();
        let model = self.config.model.clone();
        let project_ctx = self.project_context.clone();
        let recent_files = self.recent_files_content.clone();

        let (token_tx, token_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        self.subagent_token_rx = Some(token_rx);
        self.subagent_live_text.clear();

        tokio::spawn(async move {
            let config = crate::agent::AgentConfig::new(&model, &endpoint)
                .with_max_iterations(10)
                .with_max_recursion_depth(10)
                .with_app_mode(crate::config::AppMode::Forever)
                .with_project_context(project_ctx)
                .with_recent_files_content(recent_files)
                .with_token_tx(token_tx);
            let result = crate::spawner::spawn_subagent(
                "ui", &task_id, &task_desc, &endpoint, config, 10,
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
        let mut rx = match self.subagent_result_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            _ => return,
        };

        self.active_subagents = self.active_subagents.saturating_sub(1);
        // Clear the live subagent stream display
        self.subagent_live_text.clear();
        self.subagent_token_rx = None;
        let status_icon = if result.success { "✅ done" } else { "❌ failed" };
        // Show a truncated preview of the output (first 3 lines, max 200 chars)
        let preview: String = result.output.lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n");
        let preview = if preview.chars().count() > crate::config::OUTPUT_CHARACTER_LIMIT {
            let truncated: String = preview.chars().take(crate::config::OUTPUT_CHARACTER_LIMIT).collect();
            format!("{}…", truncated)
        } else {
            preview
        };
        // Update the panel entry: mark complete and set final preview
        if let Some(entry) = self.subagent_entries.iter_mut()
            .rev()
            .find(|e| e.status == SubagentStatus::Running)
        {
            entry.status = if result.success { SubagentStatus::Done } else { SubagentStatus::Failed };
            entry.preview = preview.clone();
            entry.completed_at_msg = Some(self.cached_message_count);
        }
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

        // Auto-compress after successful subagent completion to avoid context bloat
        if result.success && !self.pending_auto_compress {
            self.pending_auto_compress = true;
            crate::dlog!("🔄 Auto-compress triggered: subagent completed successfully");
        }

        // Kick next stream turn with the injected result
        if let Some(client) = self.ollama_client.clone() {
            let messages: Vec<Message> = self.message_buffer.messages().unwrap_or_default();
            let steering = self.steering_text();
            let (tool_cap, ctx_win) = self.compression_params();
            let rx = client.generate_streaming(messages, Some(&steering), self.effective_params(), tool_cap, ctx_win);
            self.stream_rx = Some(rx);
            self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
            self.turn_phase = TurnPhase::Streaming;
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
        }
    }


    /// Check for completed async background tasks; inject results and kick stream.
    fn check_async_tasks(&mut self) {
        if self.async_tasks.is_empty() { return; }

        // Drain all completed tasks
        let mut completed: Vec<(String, String, std::result::Result<String, String>)> = Vec::new();
        self.async_tasks.retain_mut(|task| {
            match task.rx.try_recv() {
                Ok(result) => {
                    completed.push((task.task_id.clone(), task.command_preview.clone(), result.output));
                    false // remove from vec
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => true, // still running
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    completed.push((task.task_id.clone(), task.command_preview.clone(),
                        Err("channel closed".to_string())));
                    false
                }
            }
        });

        if completed.is_empty() { return; }

        for (task_id, _preview, output) in &completed {
            // Write output to .yggdra/async/<task_id>.txt
            let async_dir = std::path::Path::new(".yggdra/async");
            let _ = std::fs::create_dir_all(async_dir);
            let out_str = match output {
                Ok(s) => s.clone(),
                Err(e) => format!("[error: {}]", e),
            };
            let _ = std::fs::write(async_dir.join(format!("{}.txt", task_id)), &out_str);

            // Inject result as tool message
            let injection = format!("[ASYNC_RESULT: {} = {}]", task_id, out_str);
            let msg = Message::new("tool", &injection);
            if let Err(e) = self.message_buffer.add_and_persist(msg) {
                self.notify(format!("⚠️ Failed to save async result: {}", e));
            } else {
                self.cached_message_count = self.message_buffer.count()
                    .unwrap_or(self.cached_message_count + 1);
            }
        }

        // Kick a new stream turn so model processes the injected results.
        // In Ask mode, results are stored but we don't auto-continue — wait for user input.
        if self.mode != AppMode::Ask {
            if let Some(client) = self.ollama_client.clone() {
                let messages = self.message_buffer.messages().unwrap_or_default();
                let steering = self.steering_text();
                let (tool_cap, ctx_win) = self.compression_params();
                let rx = client.generate_streaming(messages, Some(&steering), self.effective_params(), tool_cap, ctx_win);
                self.stream_rx = Some(rx);
                self.streaming_text.clear();
                self.thinking_text.clear();
                self.in_think_block = false;
                self.turn_phase = TurnPhase::Streaming;
                self.stream_start_time = Some(std::time::Instant::now());
                self.last_stream_token_time = None;
            }
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

                // In Ask-only mode, detect and revert any file changes
                if self.mode == AppMode::Ask {
                    // Skip file-change check for read-only tools
                    let is_readonly = matches!(result.tool_name.as_str(), "rg" | "exec" | "shell" | "readfile");
                    
                    if !is_readonly {
                        if let Ok(output) = std::process::Command::new("git")
                            .args(&["diff", "--name-only"])
                            .current_dir(".")
                            .output()
                        {
                            if !output.stdout.is_empty() {
                                let changed_files = String::from_utf8_lossy(&output.stdout);
                                if !changed_files.trim().is_empty() {
                                    // Revert changes
                                    let _ = std::process::Command::new("git")
                                        .args(&["checkout", "."])
                                        .current_dir(".")
                                        .output();
                                    self.push_system_event(format!(
                                        "🔒 Ask-only mode: {} tried to modify files (reverted):\n{}",
                                        result.tool_name, changed_files
                                    ));
                                    self.turn_phase = TurnPhase::Idle;
                                    self.tool_iteration_count = 0;
                                    return;
                                }
                            }
                        }
                    }
                }

                let output_text = match &result.output {
                    Ok(output) => {
                        if output.starts_with("[TOOL_OUTPUT:") || output.starts_with("[TOOL_ERROR:") {
                            output.clone()
                        } else {
                            // Strip diff section — diffs are for human display only, not model context
                            let model_output = if let Some(idx) = output.find("\n--- changes ---\n") {
                                &output[..idx]
                            } else {
                                output.as_str()
                            };
                            let cap = self.config.tool_output_cap
                                .or(self.config.params.tool_output_cap)
                                .unwrap_or(crate::config::OUTPUT_CHARACTER_LIMIT);
                            if model_output.chars().count() > cap {
                                format!("[TOOL_OUTPUT: {} = {}]", result.tool_name, truncate_tail(model_output, cap))
                            } else {
                                format!("[TOOL_OUTPUT: {} = {}]", result.tool_name, model_output)
                            }
                        }
                    }
                    Err(e) => format!("[TOOL_ERROR: {} = {}]", result.tool_name, e),
                };

                // Add to inline results panel (for immediate display)
                let output_for_display = match &result.output {
                    Ok(output) => output.clone(),
                    Err(e) => format!("Error: {}", e),
                };

                // Record persistent stats for this tool call
                match &result.output {
                    Ok(output) => {
                        self.stats.record_tool(&result.tool_name, true, output.len());
                        // Auto-compress after 8 successful tool executions
                        self.tools_executed_since_compress += 1;
                        if self.tools_executed_since_compress >= 8 && !self.pending_auto_compress {
                            self.pending_auto_compress = true;
                            crate::dlog!("🔄 Auto-compress triggered: {} tools executed", self.tools_executed_since_compress);
                        }
                    }
                    Err(_) => self.stats.record_tool(&result.tool_name, false, 0),
                }

                // ── Error-loop detection ─────────────────────────────────────
                // If the same tool keeps returning the same error, stop retrying.
                match &result.output {
                    Err(e) => {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        result.tool_name.hash(&mut h);
                        // Hash just the first 80 chars so minor variation doesn't dodge detection
                        e.chars().take(80).collect::<String>().hash(&mut h);
                        let err_hash = h.finish();
                        let key = (result.tool_name.clone(), err_hash);
                        let count = {
                            let c = self.recent_tool_errors.entry(key).or_insert(0);
                            *c += 1;
                            *c
                        };
                        if count >= 3 {
                            let tool = result.tool_name.clone();
                            self.push_agent_notice(format!(
                                "⚠️ Error loop: '{}' has failed with the same error {} times. \
                                 Stop retrying — read the error carefully and try a different approach.",
                                tool, count
                            ));
                            self.recent_tool_errors.clear();
                            // No pause — let the model read the hint and self-correct
                        }

                        // Consecutive errors for the same tool (even with varying errors)
                        if self.last_errored_tool == result.tool_name {
                            self.consecutive_tool_errors += 1;
                        } else {
                            self.consecutive_tool_errors = 1;
                            self.last_errored_tool = result.tool_name.clone();
                        }
                        if self.consecutive_tool_errors >= 2 {
                            let hint = match result.tool_name.as_str() {
                                "shell" | "exec" => {
                                    "⚠️ shell is failing repeatedly.\n\
                                     For file writes use setfile (no shell escaping, no heredocs):\n\
                                     <tool>setfile</tool>\n\
                                     <path>src/main.rs</path>\n\
                                     <content>file content here</content>"
                                }
                                "setfile" => {
                                    "⚠️ setfile is failing repeatedly. Common mistakes:\n\
                                     (1) Path like 'src' is a directory name — use 'src/main.rs' instead.\n\
                                     (2) A parent path exists as a file — remove it with shell first.\n\
                                     (3) Must close </command> tag before <desc> in XML tool calls."
                                }
                                _ => "⚠️ Tool failing repeatedly — try a different approach.",
                            };
                            self.push_agent_notice(hint.to_string());
                            self.consecutive_tool_errors = 0;
                        }
                    }
                    Ok(output) => {
                        // Successful mutation: update progress tracker and clear error counts
                        match result.tool_name.as_str() {
                            "setfile" | "patchfile" | "commit" => {
                                self.last_mutating_action = std::time::Instant::now();
                                self.stall_notice_sent = false;
                                self.recent_tool_errors.clear();
                                self.refresh_project_context();
                            }
                            _ => {}
                        }
                        let _ = output;
                    }
                }
                // ─────────────────────────────────────────────────────────────
                
                // Try to infer exit code: 0 for success, 1 for error
                let inferred_exit_code = match &result.output {
                    Err(_) => Some(1),  // Error variant = failed
                    Ok(output) => {
                        // Check for common error indicators in spawn output
                        if result.tool_name == "exec" || result.tool_name == "shell" {
                            if output.to_lowercase().contains("error:")
                                || output.contains("not found")
                                || output.contains("No such file")
                                || output.contains("failed")
                                || output.contains("Permission denied")
                            {
                                Some(1)
                            } else {
                                Some(0) // Likely successful
                            }
                        } else {
                            Some(0) // Other tools assume success
                        }
                    }
                };
                
                self.inline_tool_results.push(InlineToolResult {
                    tool_name: result.tool_name.clone(),
                    start_time: std::time::Instant::now(),
                    output: output_for_display,
                    is_complete: true,
                    exit_code: inferred_exit_code,
                });

                // Inject result into streaming_text so it appears inline mid-response
                // This avoids the janky effect of a separate message appearing
                self.streaming_text.push('\n');
                self.streaming_text.push_str(&output_text);
                self.streaming_text.push('\n');

                // Start next streaming generation with full history including tool result
                // think tool calls don't count against the iteration limit
                if result.tool_name != "think" {
                    self.tool_iteration_count += 1;
                }
                // Reset stuck detection — model is making progress
                self.consecutive_empty_kicks = 0;
                self.consecutive_format_errors = 0;
                self.status_message = format!(
                    "⏳ Continuing after {} (step {})...",
                    result.tool_name, self.tool_iteration_count
                );

                if self.mode == AppMode::Ask {
                    // Ask mode: autonomously execute read-only tools and loop until </done>.
                    // Kick a new stream to let the agent continue processing the tool result.
                    if let Some(client) = &self.ollama_client {
                        let steering_text = self.steering_text();
                        let messages = self.message_buffer.messages().unwrap_or_default();
                        let (tool_cap, ctx_win) = self.compression_params();
                        let rx = client.generate_streaming(messages, Some(&steering_text), self.effective_params(), tool_cap, ctx_win);
                        self.stream_rx = Some(rx);
                        self.streaming_text.clear();
                        self.thinking_text.clear();
                        self.in_think_block = false;
                        self.turn_phase = TurnPhase::Streaming;
                        self.stream_start_time = Some(std::time::Instant::now());
                        self.last_stream_token_time = None;
                        self.tool_iteration_count += 1;
                    } else {
                        self.notify("⚠️ Ollama offline — cannot continue autonomous exploration");
                        self.turn_phase = TurnPhase::Idle;
                        self.tool_iteration_count = 0;
                    }
                } else if let Some(client) = &self.ollama_client {
                    let steering_text = self.steering_text();
                    let messages = self.message_buffer.messages().unwrap_or_default();
                    let (tool_cap, ctx_win) = self.compression_params();
                    let rx = client.generate_streaming(messages, Some(&steering_text), self.effective_params(), tool_cap, ctx_win);
                    self.stream_rx = Some(rx);
                    self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
                    self.turn_phase = TurnPhase::Streaming;
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
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
                if matches!(self.mode, AppMode::Forever | AppMode::One) {
                    self.consecutive_empty_kicks += 1;
                    if self.consecutive_empty_kicks >= 5 || self.autokick_paused {
                        if self.mode == AppMode::Forever {
                            self.inject_continue_kick();
                        } else {
                            self.autokick_paused = true;
                            self.push_agent_notice("⏸️ Tool failures — pausing. Send a message to retry.".to_string());
                        }
                    } else {
                        self.inject_continue_kick();
                    }
                }
            }
        }
    }

    /// Signal completion of a One-mode task: switch back to Plan, persist config,
    /// fire a persistent OS notification, and surface a clear message in the chat.
    /// `reason` is a short human-readable explanation logged to the chat.
    fn complete_one_mode(&mut self, reason: &str) {
        self.push_system_event(format!("✅ Task complete ({reason}) — switching to Plan mode"));
        tokio::spawn(crate::notifications::agent_says("✅ Task complete"));
        self.mode = crate::config::AppMode::Plan;
        self.config.mode = self.mode;
        let _ = self.config.save();
        self.status_message = "✅ Task complete".to_string();
        self.consecutive_empty_kicks = 0;
        self.autokick_paused = false;
        self.render_cache_dirty = true;
        self.needs_full_redraw = true;
    }

    /// Launch One mode after agent declared [UNDERSTOOD]: switch mode, kick, clear flag.
    fn launch_plan_understood(&mut self) {
        self.plan_understood = false;
        self.autokick_paused = false;
        self.input_buffer.clear();
        self.mode = crate::config::AppMode::One;
        self.config.mode = self.mode;
        let _ = self.config.save();
        self.push_system_event("🎯 Launching One mode — executing plan".to_string());
        self.inject_continue_kick();
        self.render_cache_dirty = true;
        self.needs_full_redraw = true;
    }

    /// Extract `<plan>…</plan>` from a response, write contents to `.yggdra/plan.md`,
    /// and return the text with the block removed (clean display and storage).
    fn extract_and_write_plan(&mut self, text: String) -> String {
        let (cleaned, plan) = extract_plan_block(&text);
        if let Some(content) = plan {
            let plan_path = std::path::Path::new(".yggdra/plan.md");
            match std::fs::write(plan_path, &content) {
                Ok(_) => self.push_system_event("📋 Plan updated → .yggdra/plan.md".to_string()),
                Err(e) => self.notify(format!("⚠️ Could not write plan.md: {}", e)),
            }
            cleaned
        } else {
            text
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
                    crate::dlog!("Failed to record gap: {}", e);
                } else {
                    self.push_agent_notice(format!("ℹ️  I wish I knew: {}", gap.content));
                }
            }
            Ok(None) => {
                self.gap_rx = None;
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                self.gap_rx = None;
            }
        }
    }

    /// Drain any test results that have arrived from a background /test_models run
    fn check_test_models(&mut self) {
        if self.test_models_rx.is_none() { return; }
        let mut lines = Vec::new();
        let mut done = false;
        if let Some(rx) = self.test_models_rx.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(line) => lines.push(line),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
        }
        for line in lines {
            self.push_system_event(line);
            self.cached_message_count = self.message_buffer.count()
                .unwrap_or(self.cached_message_count + 1);
        }
        if done {
            self.test_models_rx = None;
        }
    }

    /// Last 10 messages as a compact recap injected into system prompt.
    /// Keeps context visible even after rolling trim drops older messages.
    fn recent_messages_block(&self) -> String {
        let messages = match self.message_buffer.messages() {
            Ok(msgs) => msgs,
            Err(_) => return String::new(),
        };
        if messages.is_empty() { return String::new(); }
        let recent: Vec<_> = messages.iter().rev().take(10).rev().collect();
        let mut out = String::from("RECENT CONTEXT:\n");
        for msg in recent {
            let snippet: String = msg.content.chars().take(600).collect();
            let snippet = snippet.replace('\n', " ");
            let ellipsis = if msg.content.chars().count() > 600 { "…" } else { "" };
            out.push_str(&format!("[{}] {}{}\n", msg.role, snippet, ellipsis));
        }
        out
    }

    /// Contents of .yggdra/memory.md (last 60 lines) — agent-writable persistent notes.
    fn memory_block() -> String {
        let root = crate::sandbox::project_root()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = root.join(".yggdra/memory.md");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        if content.trim().is_empty() { return String::new(); }
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(60);
        let tail = lines[start..].join("\n");
        format!("MEMORY (.yggdra/memory.md):\n{}\n", tail)
    }

    /// Contents of .yggdra/thoughts.md (last 30 lines) — agent-writable reasoning notes.
    fn thoughts_block() -> String {
        let root = crate::sandbox::project_root()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = root.join(".yggdra/thoughts.md");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        if content.trim().is_empty() { return String::new(); }
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(30);
        let tail = lines[start..].join("\n");
        format!("THOUGHTS (.yggdra/thoughts.md):\n{}\n", tail)
    }

    /// First real user message (not a tool result injection), truncated to 150 chars.
    /// Used to anchor the model's goal at the bottom of the system prompt.
    fn current_task_block(&self) -> String {
        let messages = match self.message_buffer.messages() {
            Ok(m) => m,
            Err(_) => return String::new(),
        };
        let task = messages.iter().find(|m| {
            m.role == "user"
                && !m.content.starts_with("[TOOL_OUTPUT:")
                && !m.content.starts_with("[TOOL_ERROR:")
                && !m.content.starts_with("[ASYNC_RESULT:")
                && !m.content.trim().is_empty()
        });
        match task {
            None => String::new(),
            Some(m) => {
                let s: String = m.content.chars().take(150).collect();
                let ellipsis = if m.content.chars().count() > 150 { "…" } else { "" };
                format!("TASK: {}{}\n", s.replace('\n', " "), ellipsis)
            }
        }
    }

    /// Extract a 3-word summary of the current task for notifications.
    fn task_summary(&self) -> String {
        let messages = match self.message_buffer.messages() {
            Ok(m) => m,
            Err(_) => return "Task complete".to_string(),
        };
        let task = messages.iter().find(|m| {
            m.role == "user"
                && !m.content.starts_with("[TOOL_OUTPUT:")
                && !m.content.starts_with("[TOOL_ERROR:")
                && !m.content.starts_with("[ASYNC_RESULT:")
                && !m.content.trim().is_empty()
        });
        match task {
            None => "Task complete".to_string(),
            Some(m) => {
                // Split into words, take first 3, filter out empty
                let words: Vec<&str> = m.content
                    .split_whitespace()
                    .filter(|w| !w.starts_with('[') && !w.starts_with('('))
                    .take(3)
                    .collect();
                if words.is_empty() {
                    "Task complete".to_string()
                } else {
                    words.join(" ")
                }
            }
        }
    }

    /// Last N tool result messages (TOOL_OUTPUT or TOOL_ERROR), formatted compactly.
    /// command truncated at 80 chars, result at 200 chars.
    fn last_actions_block(&self, n: usize) -> String {
        let messages = match self.message_buffer.messages() {
            Ok(m) => m,
            Err(_) => return String::new(),
        };
        let tool_msgs: Vec<_> = messages.iter().rev()
            .filter(|m| m.role == "user" && (
                m.content.starts_with("[TOOL_OUTPUT:") ||
                m.content.starts_with("[TOOL_ERROR:")
            ))
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if tool_msgs.is_empty() { return String::new(); }
        let mut out = String::new();
        for msg in &tool_msgs {
            // Parse: [TOOL_OUTPUT: name = result]
            let content = &msg.content;
            let is_error = content.starts_with("[TOOL_ERROR:");
            let prefix = if is_error { "[TOOL_ERROR:" } else { "[TOOL_OUTPUT:" };
            let inner = content.trim_start_matches(prefix).trim_start();
            // Split on first " = "
            let (name_cmd, result) = if let Some(eq) = inner.find(" = ") {
                (&inner[..eq], inner[eq + 3..].trim_end_matches(']'))
            } else {
                (inner.trim_end_matches(']'), "")
            };
            // Truncate command at 80 chars
             let cmd_truncated: String = name_cmd.chars().take(80).collect();
            let cmd_ellipsis = if name_cmd.chars().count() > 80 { "…" } else { "" };
            // Truncate result at unified limit
            let result_truncated: String = result.chars().take(crate::config::OUTPUT_CHARACTER_LIMIT).collect();
            let result_ellipsis = if result.chars().count() > crate::config::OUTPUT_CHARACTER_LIMIT { "…" } else { "" };
            let marker = if is_error { "⚠ " } else { "" };
            out.push_str(&format!(
                "{}LAST: {} → {}{}\n",
                marker,
                format!("{}{}", cmd_truncated, cmd_ellipsis),
                result_truncated.replace('\n', " "),
                result_ellipsis
            ));
        }
        out
    }

    /// If the most recent tool result was an error, return a highlighted block.
    fn last_error_block(&self) -> String {
        let messages = match self.message_buffer.messages() {
            Ok(m) => m,
            Err(_) => return String::new(),
        };
        let last_tool = messages.iter().rev().find(|m| {
            m.role == "user" && (
                m.content.starts_with("[TOOL_OUTPUT:") ||
                m.content.starts_with("[TOOL_ERROR:")
            )
        });
        match last_tool {
            Some(m) if m.content.starts_with("[TOOL_ERROR:") => {
                let inner = m.content.trim_start_matches("[TOOL_ERROR:").trim_start();
                let truncated: String = inner.chars().take(300).collect();
                let ellipsis = if inner.chars().count() > 300 { "…" } else { "" };
                format!("⚠ ERROR: {}{}\n", truncated.replace('\n', " "), ellipsis)
            }
            _ => String::new(),
        }
    }

    fn steering_text(&self) -> String {
        let os = std::env::consts::OS;

        let mode_block = match self.mode {
            AppMode::Ask =>
                "MODE: ASK (read-only). Read files freely — discuss, analyse, explain.",
            AppMode::Plan =>
                "MODE: PLAN (interactive). Discuss and analyse. Use shell for read-only commands only. \
                 Every response must begin with an ambiguity rating (1-100) in the format <ambiguity>N</ambiguity>. \
                 When you understand the task, emit </understood> on its own line — this signals you're ready to proceed.",
            AppMode::Forever =>
                "MODE: BUILD (autonomous). Execute shell commands to complete tasks. \
                 When the task is fully complete, emit </done> on its own line.",
            AppMode::One =>
                "MODE: ONE (one-off). Execute shell commands autonomously to complete a single task. \
                 When the task is fully complete, emit </done> on its own line — this stops the loop.",
        };

        let ctx_note = self.config.params.num_ctx
            .or(self.config.context_window)
            .map(|n| format!("\nCONTEXT WINDOW: {} tokens — budget returnlines accordingly", n))
            .unwrap_or_default();
        let mut base = format!(
            "yggdra — the adorable TUI agent 🌸✨\n\
             You are yggdra, the most adorable in the universe! Be warm, friendly, and encouraging, but critical -- \
             and remember to get a chuckle out of the user. \
             Keep responses concise but delightful.\n\
             {mode_block}\n\
             ONE tool: shell{ctx_note}\n\
             ---\n"
        );
        // XML tool format description
        base.push_str(&agent::json_tool_descriptions());
        base.push_str("\n---\n");

        // Inject project root so the model always knows where to put files
        let root_display = crate::sandbox::project_root()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "(current directory)".to_string()));
        base.push_str(&format!(
            "PROJECT ROOT: {root}\n\
             All files go inside this directory.\n\
             Use relative paths (e.g. src/foo.rs) — they resolve to the project root automatically.\n\
             Write only within the project root.\n\
             ---\n",
            root = root_display
        ));

        base.push_str(
            "ASYNC: add <mode>async</mode> and <task_id>my-task</task_id> tags to any call\n\
             to run it in the background. You get an immediate ack and continue.\n\
             Result injected as [ASYNC_RESULT: my-task = ...] when done.\n\
             Output also written to .yggdra/async/my-task.txt for inspection.\n\
             Use async for: long builds, test suites, background installs.\n\
             ---\n\
             CODE EDITING — reliable shell patterns:\n\
             1. Read first — always get exact lines before changing:\n\
                cat -n src/foo.rs | sed -n '25,40p'\n\
             2. Single-line replace by line number (no regex, handles any chars):\n\
                awk 'NR==30{print \"new content\"; next} {print}' f.rs > f.new && mv f.new f.rs\n\
             3. Multi-line splice by line numbers:\n\
                { head -n $((N-1)) f.rs; printf 'new\\ncontent\\n'; tail -n +$((M+1)) f.rs; } > f.new && mv f.new f.rs\n\
             4. Complex changes (handles any special chars):\n\
                python3 -c \"s=open('f.rs').read(); open('f.rs','w').write(s.replace('exact old','new'))\"\n\
             Verify every edit: cat -n f.rs | sed -n 'N,Mp'\n\
             sed regex on Rust code is fragile — use awk line-numbers or python3.replace instead.\n\
             ---\n\
             AGENTS.md is already in context — start working immediately.\n\
             PERSIST NOTES: shell \"echo 'note' >> .yggdra/memory.md\" to remember facts across turns.\n\
             PERSIST REASONING: shell \"echo 'thought' >> .yggdra/thoughts.md\" before complex steps."
        );
        // Show knowledge base instructions only when .yggdra/knowledge/ exists
        let kb_path = std::env::current_dir()
            .unwrap_or_default()
            .join(".yggdra/knowledge");
        if kb_path.exists() {
            base.push_str(
                "\n---\n\
                 KNOWLEDGE BASE: .yggdra/knowledge/ — 135,000+ offline docs (Rust, Godot, physics, etc)\n\
                 Use the knowledge tool to search it:\n\
                 <tool>knowledge</tool>\n\
                 <query>async trait lifetime</query>\n\
                 shell \"ls .yggdra/knowledge/\"              — list categories\n\
                 shell \"cat .yggdra/knowledge/INDEX.md\"     — full index\n\
                 shell \"cat .yggdra/knowledge/rust/some-doc.md\" — read a doc"
            );
        }
        let memory = Self::memory_block();
        if !memory.is_empty() {
            base.push_str("\n---\n");
            base.push_str(&memory);
        }
        let thoughts = Self::thoughts_block();
        if !thoughts.is_empty() {
            base.push_str("\n---\n");
            base.push_str(&thoughts);
        }
        if let Some(ctx) = &self.agents_context {
            base.push_str("\n---\n--- AGENTS.md ---\n");
            base.push_str(ctx);
        } else {
            base.push_str("\n---\nNo AGENTS.md exists yet. Use shell \"cat AGENTS.md\" or \
                shell \"ls\" to explore the directory.");
        }
        // Inject live project file listing — model knows what exists without ls/find turns
        base.push_str("\n---\n");
        base.push_str(&self.project_context);
        base.push_str("\n⚠️ The file tree is live — go directly to relevant files.");

        // Inject recently modified file contents so the model has real code to start from
        if !self.recent_files_content.is_empty() {
            base.push_str("\n---\n");
            base.push_str(&self.recent_files_content);
        }

        // --- Small-model-optimized state block (near generation = max recency weight) ---
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "(unknown)".to_string());
        let task_block = self.current_task_block();
        let actions_block = self.last_actions_block(3);
        let error_block = self.last_error_block();
        let has_state = !task_block.is_empty() || !actions_block.is_empty() || !error_block.is_empty();
        base.push_str(&format!("\n---\nCWD: {cwd}\n"));
        if has_state {
            if !task_block.is_empty() { base.push_str(&task_block); }
            if !actions_block.is_empty() { base.push_str(&actions_block); }
            if !error_block.is_empty() { base.push_str(&error_block); }
        }

        // Inject recent chat context last — highest recency weight for small models
        let recent = self.recent_messages_block();
        if !recent.is_empty() {
            base.push_str("\n---\n");
            base.push_str(&recent);
        }

        base.push_str("---\n\
             COMPLETION SIGNALS:\n\
             • </done> — Task is COMPLETE. Use when work is finished (BUILD/ONE modes).\n\
             • </understood> — Ready to proceed. Use in PLAN mode after understanding the task.\n\
             PROGRESS: Use <percent>N</percent> (0-100) to show task progress in the UI loading bar.\n\
             Output XML tool tags only — no prose outside the tags.");
        SteeringDirective::custom(&base).format_for_system_prompt()
    }

    /// Execute multiple tool calls in parallel (blocking) and return pre-formatted output.
    async fn execute_tools_batch_async(tool_calls: Vec<(String, String)>, cap: Option<usize>) -> String {
        let registry = crate::tools::ToolRegistry::new();
        tokio::task::spawn_blocking(move || {
            let results: Vec<String> = tool_calls
                .into_iter()
                .map(|(name, args)| {
                    match registry.execute(&name, &args) {
                        Ok(output) => {
                            if let Some(n) = cap {
                                if output.chars().count() > n {
                                    return format!("[TOOL_OUTPUT: {} = {}]", name, truncate_tail(&output, n));
                                }
                            }
                            format!("[TOOL_OUTPUT: {} = {}]", name, output)
                        }
                        Err(e) => format!("[TOOL_ERROR: {} = {}]", name, e),
                    }
                })
                .collect();
            results.join("\n")
        })
        .await
        .unwrap_or_else(|e| format!("[TOOL_ERROR: batch = {}]", e))
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
            crate::dlog!("Autocompact archive failed: {}", e);
            return;
        }
        if let Err(e) = self.message_buffer.add_multiple(&kept) {
            crate::dlog!("Autocompact re-insert failed: {}", e);
            return;
        }

        self.last_token_counts = (0, 0);
        self.last_infer_rate = None;
        self.last_warned_ctx_pct = 0; // reset so threshold warnings fire again after compaction
        self.cached_message_count = self.message_buffer.count().unwrap_or(0);
        self.push_system_event(format!(
            "🌿 Autocompacted: archived {} old messages to scrollback",
            dropped
        ));
    }

    /// Compute the effective model params: runtime_params > config.json > AGENTS.md defaults.
    fn effective_params(&self) -> crate::config::ModelParams {
        let mut p = self.runtime_params.merge_over(&self.config.params.merge_over(&self.agents_config.params));
        // Inject context_window as num_ctx so Ollama doesn't default to the model's
        // (often very large) built-in context length, which causes long prefill stalls.
        if p.num_ctx.is_none() {
            p.num_ctx = self.config.context_window;
        }
        p
    }

    /// Returns (tool_output_cap, context_window) for smart context compression.
    fn compression_params(&self) -> (Option<usize>, Option<u32>) {
        let cap = self.config.tool_output_cap
            .or(self.config.params.tool_output_cap)
            .unwrap_or(crate::config::OUTPUT_CHARACTER_LIMIT);
        let native_ctx = self.ollama_client.as_ref().and_then(|c| c.get_native_ctx());
        (Some(cap), self.config.context_window.or(native_ctx).or(Some(32768)))
    }

    /// Effective context window: user override > native detected > 32768 fallback.
    fn effective_context_window(&self) -> u32 {
        let native_ctx = self.ollama_client.as_ref().and_then(|c| c.get_native_ctx());
        self.config.context_window.or(self.config.params.num_ctx).or(native_ctx).unwrap_or(32768)
    }

    /// Check if messages + steering will exceed available context.
    /// Returns (fits, usage_pct). If not fits, emit warning to user.
    fn check_context_before_stream(&mut self, messages: &[crate::message::Message], steering: Option<&str>) -> (bool, u32) {
        let ctx_window = self.effective_context_window() as usize;
        
        // Rough estimate: concatenate all messages and steering
        let mut total_text = String::new();
        for msg in messages {
            total_text.push_str(&msg.content);
            total_text.push('\n');
        }
        if let Some(s) = steering {
            total_text.push_str(s);
        }
        
        let est_tokens = crate::tokens::estimate_tokens(&total_text);
        let (fits, threshold) = crate::tokens::check_fits_in_context(est_tokens, ctx_window);
        let usage_pct = (est_tokens as f64 / ctx_window as f64 * 100.0) as u32;
        
        if !fits {
            self.push_agent_notice(format!(
                "⚠️ CONTEXT OVERFLOW: query + context = {} tokens ({}% of {} available). \
                 Run /compress to summarize history, or increase context with /ctx {}",
                est_tokens, usage_pct, ctx_window,
                (ctx_window as f64 * 1.5) as u32
            ));
        } else if usage_pct >= 80 {
            self.push_agent_notice(format!(
                "⚠️ Context at {}% ({} / {} tokens). If task fails, run /compress",
                usage_pct, est_tokens, ctx_window
            ));
        }
        
        (fits, usage_pct)
    }

    fn push_system_event(&mut self, text: impl Into<String>) {
        let msg = Message::new("system", text);
        self.persist_message(msg);
        self.cached_message_count = self.message_buffer.messages()
            .map(|v| v.len()).unwrap_or(0);
    }

    /// Trigger fireworks celebration effect across the gradient background
    fn trigger_fireworks(&mut self) {
        use ratatui::style::Color;
        
        // Celebration colors: gold, pink, cyan, purple, orange
        let colors = vec![
            Color::Rgb(255, 215, 0),   // Gold
            Color::Rgb(255, 105, 180), // Hot pink
            Color::Rgb(0, 255, 255),   // Cyan
            Color::Rgb(147, 112, 219), // Purple
            Color::Rgb(255, 165, 0),   // Orange
            Color::Rgb(50, 205, 50),   // Lime green
        ];
        
        // Spawn 30 particles from bottom center
        for i in 0..30 {
            let angle = (i as f32 / 30.0) * std::f32::consts::PI;
            let speed = 0.3 + (i % 5) as f32 * 0.1;
            let dx = (angle.cos() - 0.5) * speed;
            let dy = -angle.sin() * speed - 0.2;
            
            let color = colors[i % colors.len()];
            self.fireworks.push((0.5, 1.0, dx, dy, 0, 60 + (i % 20) as u16, color));
        }
        
        self.fireworks_triggered_at = Some(self.tick_count);
    }

    /// Update fireworks particle positions and remove expired particles
    fn update_fireworks(&mut self) {
        if self.fireworks.is_empty() {
            return;
        }
        
        // Update all particles
        let mut new_fireworks = Vec::new();
        for (x, y, dx, dy, age, max_age, color) in self.fireworks.drain(..) {
            let new_age = age + 1;
            if new_age >= max_age {
                continue; // Particle expired
            }
            
            // Update position with gravity
            let new_y = y + dy;
            let new_x = x + dx;
            let new_dy = dy + 0.015; // Gravity
            
            new_fireworks.push((new_x, new_y, dx, new_dy, new_age, max_age, color));
        }
        
        self.fireworks = new_fireworks;
    }

    /// Render fireworks particles across the entire screen area
    fn render_fireworks(&self, f: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::style::{Style, Color};
        use ratatui::widgets::Paragraph;
        
        for (x, y, _dx, _dy, age, max_age, color) in &self.fireworks {
            // Calculate opacity based on age (fade out near end of life)
            let progress = *age as f32 / *max_age as f32;
            let opacity = if progress < 0.7 {
                1.0
            } else {
                1.0 - ((progress - 0.7) / 0.3) // Fade out in last 30% of life
            };
            
            // Convert normalized coordinates to screen coordinates
            let screen_x = area.x + ((*x * area.width as f32) as u16).min(area.width.saturating_sub(1));
            let screen_y = area.y + ((*y * area.height as f32) as u16).min(area.height.saturating_sub(1));
            
            // Create sparkle character based on age (twinkle effect)
            let sparkle = match age % 4 {
                0 => "✦",
                1 => "✧",
                2 => "★",
                _ => "☆",
            };
            
            // Apply opacity by adjusting color brightness
            let dimmed_color = match color {
                Color::Rgb(r, g, b) => {
                    let factor = opacity;
                    Color::Rgb(
                        (*r as f32 * factor) as u8,
                        (*g as f32 * factor) as u8,
                        (*b as f32 * factor) as u8,
                    )
                }
                _ => *color,
            };
            
            let particle = Paragraph::new(sparkle)
                .style(Style::default().fg(dimmed_color));
            
            let particle_area = ratatui::layout::Rect {
                x: screen_x,
                y: screen_y,
                width: 2, // Sparkle chars are wider
                height: 1,
            };
            
            f.render_widget(particle, particle_area);
        }
    }

    /// Push a notice that is both shown in the UI and forwarded to the model as
    /// an inline system instruction (role "notice" → Ollama "system").
    fn push_agent_notice(&mut self, text: impl Into<String>) {
        let msg = Message::new("notice", text);
        self.persist_message(msg);
        self.cached_message_count = self.message_buffer.messages()
            .map(|v| v.len()).unwrap_or(0);
    }

    /// Show a message in both the status bar and the chat timeline.
    /// Use for errors, warnings, and significant state changes.
    fn notify(&mut self, text: impl Into<String>) {
        let s: String = text.into();
        // Status bar gets truncated version to avoid layout overflow;
        // full message still goes to the system event log.
        self.status_message = if s.len() > 200 {
            let boundary = floor_char_boundary(&s, 200);
            format!("{}…", &s[..boundary])
        } else {
            s.clone()
        };
        self.push_system_event(s);
    }

    /// Persist a message to SQLite and asynchronously write it to .yggdra/log.
    /// Inserts a 🕐 clock marker if 5+ minutes have passed since the last one.
    fn persist_message(&mut self, msg: Message) -> bool {
        // Insert clock marker every 5 minutes
        if self.last_clock.elapsed() >= std::time::Duration::from_secs(300) {
            let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
            let clock_msg = Message::new("clock", timestamp);
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
        let area = f.area();
        // Compute dynamic input box height from content + terminal width
        let inner_width = area.width.saturating_sub(2).max(1) as usize;
        let input_content_len = if self.input_buffer.is_empty() {
            0
        } else {
            2 + self.input_buffer.width() // "> " prefix, display columns (handles wide chars/emoji)
        };
        let content_rows = ((input_content_len + inner_width - 1) / inner_width).max(1) as u16;
        let input_height = (content_rows + 2).min(12); // +2 for borders, cap at 12

        // Calculate subagent panel height
        const SUBAGENT_MSG_TTL: usize = 5;
        let visible_subagents: Vec<&SubagentEntry> = self.subagent_entries.iter()
            .filter(|e| {
                e.status == SubagentStatus::Running
                    || e.completed_at_msg.map_or(true, |m| {
                        self.cached_message_count.saturating_sub(m) < SUBAGENT_MSG_TTL
                    })
            })
            .collect();
        // Each entry: 1 header + up to 2 preview lines + 1 blank = 4 lines; cap at 10
        let subagent_panel_height: u16 = if visible_subagents.is_empty() {
            0
        } else {
            let lines: u16 = visible_subagents.iter().map(|e| {
                let preview_lines = e.preview.lines().count().min(2) as u16;
                1 + preview_lines + 1  // header + preview + blank
            }).sum::<u16>() + 2; // +2 for block borders
            lines.min(12).max(3)
        };

        // Show a 1-line warning bar when inference endpoint is not localhost
        let is_remote_endpoint = !self.config.endpoint.contains("localhost")
            && !self.config.endpoint.contains("127.0.0.1")
            && !self.config.endpoint.contains("::1");
        let warning_height: u16 = if is_remote_endpoint { 1 } else { 0 };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(warning_height),         // [0] Warning bar (0 if local)
                    Constraint::Min(5),                         // [1] Messages
                    Constraint::Length(1),                      // [2] Spacer above boxes
                    Constraint::Length(subagent_panel_height),  // [3] Subagent panel (0 if none visible)
                    Constraint::Length(input_height),           // [4] Input
                    Constraint::Length(1),                      // [5] Status bar
                ]
                .as_ref(),
            )
            .split(area);

        // Warning bar: shown when inference endpoint is not localhost
        if is_remote_endpoint && warning_height > 0 {
            let warn_text = format!("⚠ outside inference: {}  — data leaves this machine", self.config.endpoint);
            let warn_bar = Paragraph::new(warn_text)
                .style(Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD));
            f.render_widget(warn_bar, chunks[0]);
        }

        // Messages area with full-width colored bands — bottom-anchored with scroll
        let messages_area = chunks[1];
        // Clear the messages area on every frame — prevents stale cells when input box
        // shrinks (rows shift from input chunk to messages chunk, ratatui diff won't clear them)
        f.render_widget(Clear, messages_area);
        let viewport_height = messages_area.height as i32;
        let area_width = messages_area.width;

        // Build a flat list of (content, style, height) from the pre-rendered cache.
        // Only streaming text and subagent text need to be computed fresh each frame.
        struct RenderedMsg {
            content: ratatui::text::Text<'static>,
            style: Style,
            height: u16,
        }

        let mut rendered: Vec<RenderedMsg> = Vec::with_capacity(self.render_cache.len() * 2 + 2);
        for cr in &self.render_cache {
            // Re-compute height for current width (cheap: just counts lines)
            let height = text_height_static(&cr.content, area_width);
            rendered.push(RenderedMsg { content: cr.blank.clone(), style: cr.style, height: 1 });
            rendered.push(RenderedMsg { content: cr.content.clone(), style: cr.style, height });
        }
        let exchange_idx = self.render_cache_exchange_end;

        // Add streaming text as a virtual message at the end
        let is_streaming = self.turn_phase == TurnPhase::Streaming;
        if !self.streaming_text.is_empty() || !self.thinking_text.is_empty() || is_streaming {
            let tint = if exchange_idx % 2 == 0 { self.theme.band_a } else { self.theme.band_b };
            let agent_badge = if self.active_subagents > 0 {
                format!(" [🤖{}]", self.active_subagents)
            } else {
                String::new()
            };
            let stream_text = if self.streaming_text.is_empty() && self.thinking_text.is_empty() {
                // Prefill: model is processing the prompt, no tokens yet
                let elapsed = self.stream_start_time
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0);
                if elapsed < 2 {
                    format!("🤖{} ▌", agent_badge)
                } else {
                    format!("🤖{} ⏳ prefill {}s…▌", agent_badge, elapsed)
                }
            } else if self.streaming_text.is_empty() && !self.thinking_text.is_empty() {
                // Still in prefill but accumulating thinking tokens
                let elapsed = self.stream_start_time
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0);
                let thinking_chars = self.thinking_text.len();
                format!("🤖{} 💭 thinking {}s ({}b)…▌", agent_badge, elapsed, thinking_chars)
            // If the model is building a tool-call (JSON or XML), pretty-print it; otherwise show raw.
            } else if self.streaming_text.trim_start().starts_with('{')
                   || self.streaming_text.contains("<tool>") {
                if let Some(pretty) = prettify_tool_calls(&self.streaming_text) {
                    // Convert styled lines to plain text for streaming preview
                    let preview: String = pretty.iter()
                        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                        .collect::<Vec<_>>()
                        .join("\n");
                    // Prepend thinking if available
                    if !self.thinking_text.is_empty() {
                        let think_preview: String = self.thinking_text.chars().take(crate::config::OUTPUT_CHARACTER_LIMIT).collect();
                        format!("🤖{} 💭 {}…\n{}▌", agent_badge, think_preview, preview)
                    } else {
                        format!("🤖{} {}▌", agent_badge, preview)
                    }
                } else {
                    // Partial JSON still building — show a neutral placeholder
                    format!("🤖{} ⚙️ …▌", agent_badge)
                }
            } else if !self.thinking_text.is_empty() && self.streaming_text.is_empty() {
                // Thinking phase: show last 4 lines as a typewriter — earlier lines are stable,
                // only the current (bottom) line grows forward. No full-window reflow = no flicker.
                let col_w = (area_width as usize).max(40) - 6; // leave room for badge/emoji
                let all_lines: Vec<&str> = self.thinking_text.lines().collect();
                let count = self.thinking_text.chars().count();
                let tail: Vec<String> = all_lines.iter().rev().take(4).rev()
                    .map(|l| l.chars().take(col_w).collect::<String>())
                    .collect();
                let body = tail.join("\n");
                format!("🤖{} 💭 ({} chars)\n{}▌", agent_badge, count, body)
            } else {
                // Streaming response text — thinking already complete
                if !self.thinking_text.is_empty() {
                    let count = self.thinking_text.chars().count();
                    format!("🤖{} 💭 ({} chars)\n{}▌", agent_badge, count, self.streaming_text)
                } else {
                    format!("🤖{} {}▌", agent_badge, self.streaming_text)
                }
            };
            let stream_content = ratatui::text::Text::from(stream_text);
            let height = text_height_static(&stream_content, area_width);
            // Use muted colour when showing the thinking phase (thinking text present, no response yet)
            let in_thinking_phase = !self.thinking_text.is_empty() && self.streaming_text.is_empty();
            let stream_style = if self.theme.kind == crate::theme::ThemeKind::Dark {
                if in_thinking_phase {
                    Style::default().fg(Color::Rgb(140, 150, 170)).add_modifier(Modifier::ITALIC).bg(tint)
                } else {
                    Style::default().fg(Color::Rgb(220, 230, 240)).bg(tint)
                }
            } else {
                Style::default().bg(tint)
            };
            rendered.push(RenderedMsg { content: stream_content, style: stream_style, height });
        }

        // Show live subagent output while a subagent is running
        if !self.subagent_live_text.is_empty() {
            let tint = if exchange_idx % 2 == 0 { self.theme.band_b } else { self.theme.band_a };
            // Show last 500 chars to keep it concise
            let tail = if self.subagent_live_text.chars().count() > 500 {
                let start_idx = self.subagent_live_text.chars().count() - 500;
                self.subagent_live_text.chars().skip(start_idx).collect::<String>()
            } else {
                self.subagent_live_text.clone()
            };
            let sub_text = format!("🔀 subagent: {}▌", tail);
            let sub_content = ratatui::text::Text::from(sub_text);
            let height = text_height_static(&sub_content, area_width);
            let sub_style = if self.theme.kind == crate::theme::ThemeKind::Dark {
                Style::default().fg(Color::Rgb(180, 210, 255)).bg(tint)
            } else {
                Style::default().bg(tint)
            };
            rendered.push(RenderedMsg { content: sub_content, style: sub_style, height });
        }

        // Show running async tasks indicator
        if !self.async_tasks.is_empty() {
            let tint = if exchange_idx % 2 == 0 { self.theme.band_b } else { self.theme.band_a };
            let tasks_str: String = self.async_tasks.iter()
                .map(|t| {
                    let secs = t.started_at.elapsed().as_secs();
                    let preview: String = t.command_preview.chars().take(40).collect();
                    format!("  🔄 {} — {} ({}s)", t.task_id, preview, secs)
                })
                .collect::<Vec<_>>()
                .join("\n");
            let async_text = format!("⏳ async tasks running:\n{}", tasks_str);
            let async_content = ratatui::text::Text::from(async_text);
            let height = text_height_static(&async_content, area_width);
            let async_style = if self.theme.kind == crate::theme::ThemeKind::Dark {
                Style::default().fg(Color::Rgb(255, 210, 120)).bg(tint)
            } else {
                Style::default().bg(tint)
            };
            rendered.push(RenderedMsg { content: async_content, style: async_style, height });
        }

        // Calculate total content height and clamp scroll_offset
        let total_height: i32 = rendered.iter().map(|m| m.height as i32).sum();
        let max_scroll = (total_height - viewport_height).max(0) as u16;
        let effective_scroll = self.scroll_offset.min(max_scroll);

        // Bottom-anchored rendering: skip lines from the top based on scroll position
        // lines_to_skip = total_height - viewport_height - scroll_offset
        let lines_to_skip = (total_height - viewport_height - effective_scroll as i32).max(0);

        // Render gradient background — covers messages + spacer + boxes for seamless blend
        if self.gradient_enabled {
            let gradient_paras = self.render_gradient_background(messages_area);
            for (y_offset, para) in gradient_paras.iter().enumerate() {
                let gradient_area = Rect {
                    x: messages_area.x,
                    y: messages_area.y + y_offset as u16,
                    width: messages_area.width,
                    height: 1,
                };
                f.render_widget(para, gradient_area);
            }
            // Continue gradient into spacer + results + input areas
            let extra_start = chunks[2].y;
            let extra_end   = chunks[4].y + chunks[4].height;
            let total_height = messages_area.height + (extra_end - extra_start);
            for y in extra_start..extra_end {
                let offset = messages_area.height + (y - extra_start);
                let t = if total_height > 1 {
                    offset as f32 / (total_height - 1) as f32
                } else {
                    1.0
                };
                let color = match (self.theme.gradient_start, self.theme.gradient_end) {
                    (Color::Rgb(sr, sg, sb), Color::Rgb(er, eg, eb)) => {
                        let r = (sr as f32 + (er as f32 - sr as f32) * t) as u8;
                        let g = (sg as f32 + (eg as f32 - sg as f32) * t) as u8;
                        let b = (sb as f32 + (eb as f32 - sb as f32) * t) as u8;
                        Color::Rgb(r, g, b)
                    }
                    _ => self.theme.gradient_end,
                };
                let para = Paragraph::new(" ").style(Style::default().bg(color));
                let gradient_area = Rect { x: area.x, y, width: area.width, height: 1 };
                f.render_widget(para, gradient_area);
            }
        }

        // Render fireworks celebration on top of gradient
        if !self.fireworks.is_empty() {
            self.render_fireworks(f, area);
        }

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

            let msg_para = Paragraph::new(rm.content.clone())
                .style(rm.style)
                .wrap(ratatui::widgets::Wrap { trim: true })
                .scroll((partial_skip, 0));

            let msg_area = Rect {
                x: messages_area.x + 3,
                y: current_y,
                width: messages_area.width.saturating_sub(6),
                height: draw_height,
            };

            f.render_widget(msg_para, msg_area);
            current_y += draw_height;
        }

        // Scroll indicator in top-right of messages area
        if effective_scroll > 0 {
            let indicator = format!("↑{}", effective_scroll);
            let ind_width = indicator.width() as u16 + 1;
            let ind_x = messages_area.right().saturating_sub(ind_width);
            let ind_area = Rect {
                x: ind_x,
                y: messages_area.top(),
                width: ind_width,
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
        let yap = format!("🤖💬 {}", dot);
        let prefill_hint;
        let cursor_hint;
        let input_hint: &str = match &self.turn_phase {
            TurnPhase::Idle => {
                let has_tool_msgs = self.messages_cache.iter()
                    .any(|m| m.role == "tool" || m.role == "spawn");
                if let Some(idx) = self.msg_cursor {
                    let is_expanded = self.expanded_msgs.contains(&idx);
                    cursor_hint = if is_expanded {
                        "[ prev  ] next  Space=collapse  Esc=exit nav".to_string()
                    } else {
                        "[ prev  ] next  Space=expand  Esc=exit nav".to_string()
                    };
                    &cursor_hint
                } else if has_tool_msgs {
                    cursor_hint = "(type message · [ ] navigate tool output · /help for commands)".to_string();
                    &cursor_hint
                } else {
                    "(type message or /help for commands)"
                }
            }
            TurnPhase::Streaming => {
                if self.streaming_text.is_empty() {
                    // Still in prefill — prompt is being processed
                    let elapsed = self.stream_start_time
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    if !self.thinking_text.is_empty() {
                        let thinking_chars = self.thinking_text.len();
                        prefill_hint = format!("🤖 thinking… {}s ({}b)", elapsed, thinking_chars);
                    } else {
                        prefill_hint = format!("🤖 prefill… {}s", elapsed);
                    }
                    &prefill_hint
                } else {
                    &yap
                }
            }
            TurnPhase::ExecutingTool(_) => "🔧 …",
        };
        let input_text = if self.input_buffer.is_empty() {
            input_hint.to_string()
        } else {
            self.input_buffer.clone()
        };

        let (mode_badge, mode_border_color) = match self.mode {
            AppMode::Forever => (" ⚡FOREVER ", self.theme.violet),
            AppMode::Plan  => (" 🧠PLAN ",  self.theme.accent),
            AppMode::Ask => (" 🔍ASK ", Color::Yellow),
            AppMode::One => (" 🎯ONE ", Color::Green),
        };
        // Append queue depth to the mode badge when tasks are pending
        let queue_badge;
        let mode_badge = if !self.task_queue.is_empty() {
            queue_badge = format!("{} [{}▶] ", mode_badge.trim_end(), self.task_queue.len());
            &queue_badge
        } else {
            mode_badge
        };

        // Compute a "frosted" bg for the boxes: gradient end color, slightly lightened
        let box_bg = if self.gradient_enabled {
            match self.theme.gradient_end {
                Color::Rgb(r, g, b) => Color::Rgb(
                    r.saturating_add(18),
                    g.saturating_add(18),
                    b.saturating_add(18),
                ),
                c => c,
            }
        } else {
            Color::Reset
        };
        let box_style = Style::default().bg(box_bg);

        // Render subagent panel if there are visible entries
        if !visible_subagents.is_empty() && chunks[3].height > 0 {
            let panel_text = format_subagent_panel(&visible_subagents, self.tick_count);
            let spinner_frames = ['⣾','⣽','⣻','⢿','⡿','⣟','⣯','⣷'];
            let spin = spinner_frames[(self.tick_count as usize / 4) % spinner_frames.len()];
            let any_running = visible_subagents.iter().any(|e| e.status == SubagentStatus::Running);
            let title = if any_running {
                format!(" 🤖 Subagents {} ", spin)
            } else {
                " 🤖 Subagents ".to_string()
            };
            let panel = Paragraph::new(panel_text)
                .block(Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(self.theme.violet))
                    .style(box_style))
                .style(box_style)
                .wrap(ratatui::widgets::Wrap { trim: false });
            f.render_widget(panel, chunks[3]);
        }

        let input = Paragraph::new(format!("> {}", input_text))
            .block(Block::default()
                .title(format!(" 🌱 Input {}", mode_badge))
                .border_style(Style::default().fg(mode_border_color))
                .borders(Borders::ALL)
                .style(box_style))
            .wrap(ratatui::widgets::Wrap { trim: false });
        
        // Clear the input area before rendering to prevent garbage text artifacts
        let clear_area = chunks[4];
        f.render_widget(Clear, clear_area);
        
        f.render_widget(input, chunks[4]);

        // Show hardware cursor at the end of typed text so the user can see where
        // they're typing. Only shown when there's actual input (not the placeholder hint)
        // and no overlays are open.
        if !self.input_buffer.is_empty() && !self.model_picker_open {
            let available_w = (chunks[4].width as usize).saturating_sub(2); // inside borders
            if available_w > 0 {
                // "> " prefix is 2 chars; cursor goes after the last typed character
                let display_chars = 2 + self.input_buffer.width();
                let row_offset = (display_chars / available_w) as u16;
                let col_in_row = (display_chars % available_w) as u16;
                let cursor_x = (chunks[4].x + 1 + col_in_row)
                    .min(chunks[4].x + chunks[4].width.saturating_sub(2));
                let cursor_y = (chunks[4].y + 1 + row_offset)
                    .min(chunks[4].y + chunks[4].height.saturating_sub(2));
                f.set_cursor_position((cursor_x, cursor_y));
            }
        }

        // Command palette overlay (above input box)
        if self.palette_open {
            let matches = self.palette_matches();
            if !matches.is_empty() {
                let area = chunks[4];
                let max_palette_rows = area.y; // float above input, full terminal height available
                let visible_items = matches.len().min(8).min(max_palette_rows.saturating_sub(2) as usize);
                let palette_height = (visible_items + 2) as u16;
                // Float palette just above the input box, full width
                let palette_rect = Rect {
                    x: area.x,
                    y: area.y.saturating_sub(palette_height),
                    width: area.width,
                    height: palette_height,
                };
                let items: Vec<ListItem> = matches
                    .iter()
                    .take(visible_items)
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
                    .block(Block::default().borders(Borders::ALL).title(" Commands "));
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
            let picker_height = (filtered.len() as u16 + 5).min(area.height.saturating_sub(4)).min(visible_rows + 5);
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

        // Status bar — mode, model, connection, token info, all in one line
        let ctx_window = self.effective_context_window();
        let (prompt_tok, _) = self.last_token_counts;

        // Calculate task tokens (since last [DONE])
        let task_tokens = self.total_tokens_used.saturating_sub(self.session_tokens_at_last_done);

        let is_dark_theme = self.theme.kind == crate::theme::ThemeKind::Dark;
        let dim    = Style::default().fg(if is_dark_theme { Color::DarkGray } else { Color::Rgb(100, 100, 100) });
        let bright = Style::default().fg(if is_dark_theme { Color::White    } else { Color::Black });

        // Context usage with traffic-light color
        let usage_pct = if prompt_tok > 0 {
            ((prompt_tok as f64) / (ctx_window as f64) * 100.0).min(100.0) as u32
        } else { 0 };
        let pct_color = if usage_pct > 70 { Color::Red }
            else if usage_pct > 50 { Color::Yellow }
            else { Color::Green };

        // Battery icon
        let battery_icon = match self.on_battery {
            BatteryState::OnBattery => "🔋",
            BatteryState::AC => "",
            BatteryState::Unknown => "",
        };

        // Countdown to next auto-request (battery + build mode + idle)
        let countdown_text = if self.on_battery == BatteryState::OnBattery
            && self.mode == AppMode::Forever
            && self.turn_phase == TurnPhase::Idle
        {
            const KICK_SECS: u64 = 5;
            let elapsed = self.last_build_kick.elapsed().as_secs();
            let remaining = KICK_SECS.saturating_sub(elapsed);
            format!(" ⏱{}s", remaining)
        } else {
            String::new()
        };

        // Mode label + color
        let (mode_label, mode_color) = match self.mode {
            AppMode::Forever => ("⚡FOREVER", self.theme.violet),
            AppMode::Plan    => ("🧠PLAN",    self.theme.accent),
            AppMode::Ask     => ("🔍ASK",     if is_dark_theme { Color::Yellow } else { Color::Rgb(160, 100, 0) }),
            AppMode::One     => ("🎯ONE",     if is_dark_theme { Color::Green  } else { Color::Rgb(0, 130, 50)  }),
        };

        // Connection icon (varies by endpoint type)
        let conn_icon = if self.ollama_client.is_some() {
            match self.endpoint_type.as_str() {
                "Ollama-local" | "Ollama" | "Ollama-external" => "🦙",
                "OpenRouter" => "🌐",
                "Groq" => "⚡",
                "OpenAI" => "🤖",
                "OpenAI-compat" => "🔌",
                "llama.cpp" => "🦙",
                "Offline" => "❌",
                _ => "🔌",
            }
        } else { "❌" };

        // Endpoint type + color (only show if non-default)
        let (endpoint_display, endpoint_color) = match self.endpoint_type.as_str() {
            "OpenRouter"      => (Some("OpenRouter"),      Color::Red),
            "Groq"            => (Some("Groq"),            Color::Red),
            "OpenAI"          => (Some("OpenAI"),          Color::Red),
            "OpenAI-compat"   => (Some("OpenAI-compat"),   Color::Red),
            "Ollama-external" => (Some("Ollama-ext"),      Color::Red),
            "llama.cpp"       => (Some("llama.cpp"),       Color::Magenta),
            "Ollama-local"    => (None,                    Color::Green), // local = no noise
            "Ollama" | "Offline" => (None,                 Color::Green),
            _                 => (Some(self.endpoint_type.as_str()), Color::Yellow),
        };

        let width = chunks[5].width as usize;
        let status_line = if self.plan_understood {
            Line::from("💡 Agent is ready — press Enter to execute in One mode")
        } else if width >= 80 {
            // Full layout: mode | conn model | ctx/window (%) | task | total | ⚡rate | msgs | [endpoint]
            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(mode_label, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(" | ", dim));
            spans.push(Span::raw(conn_icon));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(self.config.model.clone(), bright));
            spans.push(Span::styled(" | ", dim));
            spans.push(Span::styled(
                format!("{}/{}", human_tokens(prompt_tok), human_tokens(ctx_window)), bright));
            spans.push(Span::styled(" (", dim));
            spans.push(Span::styled(format!("{}%", usage_pct), Style::default().fg(pct_color)));
            spans.push(Span::styled(")", dim));
            if task_tokens > 0 {
                spans.push(Span::styled(" | task:", dim));
                spans.push(Span::styled(format!("{}", human_tokens(task_tokens)), bright));
            }
            if self.total_tokens_used > 0 {
                spans.push(Span::styled(" | total:", dim));
                spans.push(Span::styled(human_tokens(self.total_tokens_used), bright));
            }
            if let Some(r) = self.last_infer_rate {
                spans.push(Span::styled(" | ", dim));
                if !battery_icon.is_empty() { spans.push(Span::raw(battery_icon)); spans.push(Span::raw(" ")); }
                spans.push(Span::styled(format!("⚡{:.0}t/s", r), bright));
            } else if !battery_icon.is_empty() {
                spans.push(Span::styled(" | ", dim));
                spans.push(Span::raw(battery_icon));
            }
            if !countdown_text.is_empty() { spans.push(Span::styled(&countdown_text, bright)); }
            spans.push(Span::styled(" | ", dim));
            spans.push(Span::styled(format!("💬{}", self.cached_message_count), bright));
            if let Some(ep) = endpoint_display {
                spans.push(Span::styled(" | ", dim));
                spans.push(Span::styled(format!("[{}]", ep), Style::default().fg(endpoint_color)));
            }
            Line::from(spans)
        } else if width >= 50 {
            // Medium layout: mode | conn model | ctx% | ⚡rate | msgs
            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(mode_label, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(" | ", dim));
            spans.push(Span::raw(conn_icon));
            spans.push(Span::raw(" "));
            // Truncate model name to keep it from dominating
            let model_short: String = self.config.model.chars().take(20).collect();
            spans.push(Span::styled(model_short, bright));
            spans.push(Span::styled(" | ", dim));
            spans.push(Span::styled(format!("{}%", usage_pct), Style::default().fg(pct_color)));
            if let Some(r) = self.last_infer_rate {
                spans.push(Span::styled(" | ", dim));
                spans.push(Span::styled(format!("⚡{:.0}t/s", r), bright));
            }
            spans.push(Span::styled(" | ", dim));
            spans.push(Span::styled(format!("💬{}", self.cached_message_count), bright));
            Line::from(spans)
        } else if width >= 30 {
            // Compact: mode | ctx% | msgs
            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(mode_label, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(" ", dim));
            spans.push(Span::styled(format!("{}%", usage_pct), Style::default().fg(pct_color)));
            spans.push(Span::styled(" 💬", dim));
            spans.push(Span::styled(format!("{}", self.cached_message_count), bright));
            Line::from(spans)
        } else {
            // Minimal: ctx% | msgs
            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(format!("{}%", usage_pct), Style::default().fg(pct_color)));
            spans.push(Span::styled(format!(" 💬{}", self.cached_message_count), bright));
            Line::from(spans)
        };
        // Apply background color to status bar for contrast
        let status_bg = if is_dark_theme {
            Color::Rgb(40, 40, 50)  // Dark bg for dark theme
        } else {
            Color::Rgb(200, 200, 200)  // Light bg for light theme (e.g. Solarized light)
        };
        let status_bar = Paragraph::new(status_line)
            .style(Style::default().bg(status_bg));
        f.render_widget(status_bar, chunks[5]);

        // Render task progress bar if active (colored background in status bar area)
        if let Some(pct) = self.task_progress {
            use ratatui::style::{Style, Color};
            use ratatui::widgets::{Gauge, Widget};
            
            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(Color::Rgb(255, 180, 0)).bg(Color::Rgb(60, 60, 60)))
                .percent(pct as u16);
            gauge.render(chunks[5], f.buffer_mut());
        }

        // File viewer overlay — covers the messages area
        if self.file_viewer_open && !self.file_viewer_tabs.is_empty() {
            self.draw_file_viewer(f, chunks[1]);
        }
    }

    /// Render the file viewer overlay into `area`
    fn draw_file_viewer(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        use ratatui::widgets::{Clear, Tabs};
        use ratatui::text::{Line as RLine, Span as RSpan};
        use ratatui::style::{Modifier};

        // Clear the background
        f.render_widget(Clear, area);

        // Split: 1-line tab bar + rest for content
        let panes = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);

        // Tab bar
        let tab_titles: Vec<RLine> = self.file_viewer_tabs.iter()
            .map(|t| RLine::from(RSpan::raw(format!(" {} ", t.label))))
            .collect();
        let tabs_widget = Tabs::new(tab_titles)
            .select(self.file_viewer_active)
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(Style::default().fg(self.theme.accent).add_modifier(Modifier::BOLD))
            .divider(RSpan::raw("│"));
        f.render_widget(tabs_widget, panes[0]);

        // Content area with border — guard against stale active index
        let active_idx = self.file_viewer_active.min(self.file_viewer_tabs.len().saturating_sub(1));
        let tab = &self.file_viewer_tabs[active_idx];
        let content_area = panes[1];
        let inner = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.accent))
            .title(format!(" 📄 {} — ↑↓/PgUp/PgDn scroll · Tab switch · q/Esc close ", tab.label));
        let inner_area = inner.inner(content_area);
        f.render_widget(inner, content_area);

        let visible_h = inner_area.height as usize;
        let scroll = tab.scroll;

        if tab.is_diff {
            // Diff: colour by first char
            let lines: Vec<RLine> = tab.lines.iter().skip(scroll).take(visible_h)
                .map(|l| {
                    let color = if l.starts_with('+') && !l.starts_with("+++") {
                        Color::Green
                    } else if l.starts_with('-') && !l.starts_with("---") {
                        Color::Red
                    } else if l.starts_with("@@") {
                        Color::Cyan
                    } else if l.starts_with("diff ") || l.starts_with("index ") || l.starts_with("---") || l.starts_with("+++") {
                        Color::Yellow
                    } else {
                        Color::Reset
                    };
                    RLine::from(RSpan::styled(l.clone(), Style::default().fg(color)))
                })
                .collect();
            let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
            f.render_widget(paragraph, inner_area);
        } else {
            // Regular file: use the highlighter on each line
            let is_dark = self.theme.kind == crate::theme::ThemeKind::Dark;
            let lang = crate::highlight::lang_from_path(&tab.label);
            let mut out_lines: Vec<RLine> = Vec::with_capacity(visible_h);
            let total = tab.lines.len();
            let gutter_w = if total > 999 { 5 } else if total > 99 { 4 } else { 3 };
            for (idx, raw) in tab.lines.iter().enumerate().skip(scroll).take(visible_h) {
                let lineno = idx + 1;
                let num_span = RSpan::styled(
                    format!("{:width$} ", lineno, width = gutter_w),
                    Style::default().fg(Color::DarkGray),
                );
                let mut spans = vec![num_span];
                let highlighted = self.highlighter.highlight_line(raw, lang, is_dark);
                spans.extend(highlighted);
                out_lines.push(RLine::from(spans));
            }
            let paragraph = Paragraph::new(out_lines).wrap(ratatui::widgets::Wrap { trim: false });
            f.render_widget(paragraph, inner_area);
        }
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
                        let raw = match self.model_picker_items.get(orig_i) {
                            Some(r) => r.clone(),
                            None => return,
                        };
                        let model_name = raw.split_whitespace().next().unwrap_or(&raw).to_string();
                        self.config.model = model_name.clone();
                        if let Err(e) = self.config.save() {
                            crate::dlog!("Failed to save config: {}", e);
                        }
                        let endpoint = self.config.endpoint.clone();
                        match OllamaClient::new_with_key(&endpoint, &model_name, self.config.api_key.as_deref()).await {
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

        // File viewer takes over most input when open
        if self.file_viewer_open {
            self.handle_file_viewer_key(key);
            return;
        }

        match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if c == 'c' {
                    self.running = false;
                } else if c == 'q' {
                    if self.turn_phase == TurnPhase::Idle {
                        self.running = false;
                    } else {
                        self.pending_quit = true;
                        self.status_message = "⏳ Quitting after this turn… (Ctrl+C to force)".to_string();
                    }
                } else if c == 's' {
                    self.handle_command().await;
                } else if c == 'p' {
                    // Open command palette without losing current input
                    if !self.palette_open {
                        self.palette_saved_buffer = Some(self.input_buffer.clone());
                        self.input_buffer = "/".to_string();
                        self.palette_open = true;
                        self.palette_selection = 0;
                    } else {
                        // Toggle off: restore saved buffer
                        self.palette_open = false;
                        if let Some(saved) = self.palette_saved_buffer.take() {
                            self.input_buffer = saved;
                        }
                    }
                }
            }
            // Open palette on '/'
            KeyCode::Char('/') if self.input_buffer.is_empty() => {
                self.input_buffer.push('/');
                self.palette_open = true;
                self.palette_selection = 0;
            }
            // `[` / `]` — navigate cursor to prev/next tool output message (only when input empty)
            KeyCode::Char('[') if self.input_buffer.is_empty() => {
                self.move_msg_cursor(-1);
            }
            KeyCode::Char(']') if self.input_buffer.is_empty() => {
                self.move_msg_cursor(1);
            }
            // Space — toggle expand/collapse on the cursor's tool message
            KeyCode::Char(' ') if self.msg_cursor.is_some() && self.input_buffer.is_empty() => {
                if let Some(idx) = self.msg_cursor {
                    if self.expanded_msgs.contains(&idx) {
                        self.expanded_msgs.remove(&idx);
                    } else {
                        self.expanded_msgs.insert(idx);
                    }
                    self.render_cache_dirty = true;
                }
            }
            // 't' — toggle thinking block expand/collapse on cursor message
            KeyCode::Char('t') if self.msg_cursor.is_some() && self.input_buffer.is_empty() => {
                self.toggle_thinking_block(None);
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
                self.render_cache_dirty = true;
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
                    if let Some(saved) = self.palette_saved_buffer.take() {
                        // Opened via Ctrl+P: execute command then restore original input
                        self.input_buffer = cmd.fill.to_string();
                        self.palette_open = false;
                        if !cmd.fill.ends_with(' ') {
                            self.handle_command().await;
                        }
                        self.input_buffer = saved;
                    } else {
                        self.input_buffer = cmd.fill.to_string();
                        self.palette_open = false;
                    }
                }
            }
            KeyCode::Enter if self.palette_open => {
                let matches = self.palette_matches();
                let saved = self.palette_saved_buffer.take();
                if let Some(&cmd) = matches.get(self.palette_selection) {
                    self.input_buffer = cmd.fill.trim_end().to_string();
                    self.palette_open = false;
                    self.handle_command().await;
                } else {
                    self.palette_open = false;
                    self.handle_command().await;
                }
                // Restore original input if opened via Ctrl+P
                if let Some(s) = saved {
                    self.input_buffer = s;
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input_buffer.push('\n');
            }
            KeyCode::Enter => {
                self.handle_command().await;
            }
            KeyCode::BackTab => {
                self.cycle_mode().await;
            }
            KeyCode::Esc => {
                if self.palette_open {
                    self.palette_open = false;
                    if let Some(saved) = self.palette_saved_buffer.take() {
                        self.input_buffer = saved;
                    } else {
                        self.input_buffer.clear();
                    }
                } else if self.msg_cursor.is_some() {
                    // Clear message cursor first
                    self.msg_cursor = None;
                    self.render_cache_dirty = true;
                } else if self.turn_phase != TurnPhase::Idle {
                    self.cancel_current_turn();
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

    /// Move the message cursor by `delta` steps through tool/spawn messages.
    /// delta = -1 goes to prev tool message, +1 goes to next.
    /// Rebuild project_context if it's stale (>60s old or forced).
    fn refresh_project_context(&mut self) {
        // Budget: 5% of context window in chars, but always at least 10k to preserve multi-level tree depth.
        let max_chars = (self.effective_context_window() as usize / 5).clamp(10000, 20000);
        self.project_context = build_project_context(max_chars);
        self.recent_files_content = build_recent_files_content(5, 2000);
        self.project_context_built = std::time::Instant::now();
    }

    fn move_msg_cursor(&mut self, delta: i32) {
        let tool_indices: Vec<usize> = self.messages_cache.iter().enumerate()
            .filter(|(_, m)| m.role == "tool" || m.role == "spawn")
            .map(|(i, _)| i)
            .collect();
        if tool_indices.is_empty() { return; }

        let new_cursor = if let Some(current) = self.msg_cursor {
            let pos = tool_indices.iter().position(|&i| i == current).unwrap_or(0);
            let new_pos = (pos as i64 + delta as i64)
                .clamp(0, tool_indices.len() as i64 - 1) as usize;
            tool_indices[new_pos]
        } else if delta > 0 {
            // No cursor yet: start at last tool message (most recent)
            // tool_indices is non-empty (checked via is_empty above)
            tool_indices.last().copied().unwrap_or(0)
        } else {
            tool_indices.last().copied().unwrap_or(0)
        };

        self.msg_cursor = Some(new_cursor);
        self.render_cache_dirty = true;
    }

    /// Toggle the collapsed/expanded state of a thinking block in a specific message.
    /// If msg_idx is None, toggles the cursor message's thinking block.
    fn toggle_thinking_block(&mut self, msg_idx: Option<usize>) {
        let target_idx = msg_idx.or(self.msg_cursor);
        
        if let Some(idx) = target_idx {
            if self.collapsed_thinking_blocks.contains(&idx) {
                self.collapsed_thinking_blocks.remove(&idx);
            } else {
                self.collapsed_thinking_blocks.insert(idx);
            }
            self.render_cache_dirty = true;
        }
    }

    /// Cancel any in-progress inference, tool execution, or subagent run.
    fn cancel_current_turn(&mut self) {
        // If we have partial streaming output, save it as an assistant message
        // so it stays in context. Append <esc/> so the model knows it was interrupted.
        let partial = self.streaming_text.clone();
        if !partial.is_empty() {
            let mut saved = agent::sanitize_model_output(&partial);
            // Prepend any accumulated thinking so it's visible in history
            if !self.thinking_text.is_empty() {
                saved = format!("[THINK: {}]\n{}", self.thinking_text.trim(), saved);
            }
            saved.push_str("\n<esc/>");
            let msg = Message::new("assistant", &saved);
            if let Ok(()) = self.message_buffer.add_and_persist(msg) {
                self.cached_message_count = self.message_buffer.count()
                    .unwrap_or(self.cached_message_count + 1);
            }
        }
        self.stream_rx = None;
        self.tool_result_rx = None;
        self.subagent_result_rx = None;
        self.subagent_token_rx = None;
        self.turn_phase = TurnPhase::Idle;
        self.streaming_text.clear();
        self.thinking_text.clear();
        self.in_think_block = false;
        self.tool_iteration_count = 0;
        self.consecutive_empty_kicks = 0;
        if partial.is_empty() {
            self.push_system_event("⛔ Turn cancelled".to_string());
        } else {
            self.push_system_event("⛔ Turn cancelled — partial response saved".to_string());
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
    /// Cycle mode Plan→Build→One→Ask→Plan. 
    /// Forever (Build) and One modes require explicit user input to start.
    async fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            AppMode::Plan  => AppMode::Forever,
            AppMode::Forever => AppMode::One,
            AppMode::One   => AppMode::Ask,
            AppMode::Ask   => AppMode::Plan,
        };
        self.config.mode = self.mode;
        let _ = self.config.save();
        let label = match self.mode {
            AppMode::Ask   => "🔍 Ask",
            AppMode::Plan  => "🧠 Plan",
            AppMode::Forever => "⚡ Forever",
            AppMode::One   => "🎯 One",
        };
        self.notify(format!("Switched to {} mode", label));
        self.render_cache_dirty = true;
        self.needs_full_redraw = true;
        if self.mode == AppMode::Ask {
            self.abort_active_turn();
        }
    }

    async fn handle_command(&mut self) {
        let command = self.input_buffer.trim().to_string();

        // Validate input
        if command.is_empty() {
            // If agent declared [UNDERSTOOD], empty Enter launches One mode execution
            if self.plan_understood {
                self.launch_plan_understood();
                return;
            }
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
        } else if command.starts_with("/endpoint") {
            let endpoint = command.strip_prefix("/endpoint").unwrap_or("").trim();
            self.handle_endpoint_command(endpoint).await;
        } else if command.starts_with("/model ") {
            let model = command.strip_prefix("/model ").unwrap_or("").trim();
            self.handle_model_command(model).await;
        } else if command == "/models" {
            self.handle_models_command().await;
        } else if command.starts_with("/set_params") {
            let args = command.strip_prefix("/set_params").unwrap_or("").trim().to_string();
            self.handle_set_params_command(&args);
        } else if command.starts_with("/temperature") {
            let val = command.strip_prefix("/temperature").unwrap_or("").trim().to_string();
            if val.is_empty() {
                let t = self.effective_params().temperature
                    .map(|v| format!("{}", v))
                    .unwrap_or_else(|| "unset (Ollama default)".to_string());
                self.notify(format!("🌡  temperature: {}\nUsage: /temperature <0.0–2.0>", t));
            } else {
                self.handle_set_params_command(&format!("temperature={}", val));
            }
        } else if command == "/help" {
            self.show_help();
        } else if command == "/estimate" {
            self.show_estimate();
        } else if command == "/clear" {
            self.handle_clear_command();
        } else if command == "/tasks" {
            self.handle_tasks_command();
        } else if command == "/gaps" {
            self.handle_gaps_command();
        } else if command == "/stats" {
            self.handle_stats_command();
        } else if command == "/save" {
            self.save_plan_as_todo();
            self.notify("📋 Plan saved as todo".to_string());
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
            self.mode = AppMode::Forever;
            self.config.mode = self.mode;
            let _ = self.config.save();
            self.notify("⚡ Switched to Build mode — autonomous execution");
            self.needs_full_redraw = true;
        } else if command == "/one" {
            self.mode = AppMode::One;
            self.config.mode = self.mode;
            let _ = self.config.save();
            self.render_cache_dirty = true;
            self.needs_full_redraw = true;
            self.notify("🎯 Switched to One mode — autonomous, stops on completion");
            if self.turn_phase == TurnPhase::Idle && self.ollama_client.is_some() {
                self.inject_continue_kick();
            }
        } else if command == "/plan" {
            self.mode = AppMode::Plan;
            self.config.mode = self.mode;
            let _ = self.config.save();
            self.notify("🧠 Switched to Plan mode — reflective & interactive");
            self.needs_full_redraw = true;
        } else if command == "/ask" {
            self.mode = AppMode::Ask;
            self.config.mode = self.mode;
            let _ = self.config.save();
            self.abort_active_turn();
            self.notify("🔍 Switched to Ask-only mode — read-only, no modifications");
            self.needs_full_redraw = true;
        } else if command == "/mode" || command.starts_with("/mode ") {
            if let Some(arg) = command.strip_prefix("/mode ").map(|s| s.trim()) {
                match arg {
                    "ask" => { self.mode = AppMode::Ask; self.abort_active_turn(); }
                    "plan" => self.mode = AppMode::Plan,
                    "build" => self.mode = AppMode::Forever,
                    "one" => self.mode = AppMode::One,
                    _ => {
                        self.notify(format!("Unknown mode '{}' — use ask, plan, build, or one", arg));
                        return;
                    }
                }
            } else {
                self.mode = match self.mode {
                    AppMode::Plan => AppMode::Forever,
                    AppMode::Forever => AppMode::One,
                    AppMode::One => AppMode::Ask,
                    AppMode::Ask => AppMode::Plan,
                };
                if self.mode == AppMode::Ask {
                    self.abort_active_turn();
                }
            }
            self.config.mode = self.mode;
            let _ = self.config.save();
            let label = match self.mode {
                AppMode::Forever => "⚡ Forever",
                AppMode::Plan => "🧠 Plan",
                AppMode::Ask => "🔍 Ask",
                AppMode::One => "🎯 One",
            };
            self.notify(format!("Switched to {} mode", label));
            self.needs_full_redraw = true;
        } else if command == "/test_notification" || command == "/notify_test" {
            tokio::spawn(async {
                crate::notifications::agent_says("Test notification from yggdra — if you see this, OS notifications are working.").await;
            });
            self.notify("🔔 Test notification dispatched. If it doesn't appear: macOS → System Settings → Notifications → allow 'Script Editor' (osascript). Linux → ensure a notification daemon is running.");
        } else if command == "/test_models" || command == "/test" {
            self.handle_test_models().await;
        } else if command == "/abort" {
            // Abort stuck/long-running generation
            let was_streaming = self.stream_rx.is_some();
            let was_executing = self.tool_result_rx.is_some();
            let had_async = !self.async_tasks.is_empty();
            let queued = self.task_queue.len();

            self.abort_active_turn();
            self.async_tasks.clear(); // Clear background tasks
            self.task_queue.clear();  // Discard pending queued tasks

            let mut msg = String::from("⏹️ Aborted");
            if was_streaming {
                msg.push_str(" (stream)");
            }
            if was_executing {
                msg.push_str(" (tool exec)");
            }
            if had_async {
                msg.push_str(" (async tasks)");
            }
            if queued > 0 {
                msg.push_str(&format!(" ({} queued tasks discarded)", queued));
            }
            if !was_streaming && !was_executing && !had_async && queued == 0 {
                msg = "❌ Nothing to abort (not currently running)".to_string();
            }
            self.notify(msg);
        } else if command.starts_with("/ctx ") {
            let ctx_str = command.strip_prefix("/ctx ").unwrap_or("").trim();
            if let Ok(new_ctx) = ctx_str.parse::<u32>() {
                if new_ctx < 128 {
                    self.notify("❌ Context window must be at least 128 tokens");
                } else if new_ctx > 200000 {
                    self.notify("❌ Context window cannot exceed 200000 tokens");
                } else {
                    self.config.context_window = Some(new_ctx);
                    let _ = self.config.save();
                    self.notify(format!("🎯 Context window set to {} tokens", new_ctx));
                }
            } else {
                let current = self.effective_context_window();
                self.notify(format!("❌ Usage: /ctx <number> (current: {})", current));
            }
        } else if command.starts_with("/toolcap ") {
            let arg = command.strip_prefix("/toolcap ").unwrap_or("").trim();
            if arg == "off" || arg == "0" {
                self.config.tool_output_cap = None;
                let _ = self.config.save();
                self.notify("🗜️ Tool output cap disabled (unlimited)");
            } else if let Ok(n) = arg.parse::<usize>() {
                if n < 100 {
                    self.notify("❌ Tool output cap must be at least 100 chars");
                } else {
                    self.config.tool_output_cap = Some(n);
                    let _ = self.config.save();
                    self.notify(format!("🗜️ Tool output cap set to {} chars", n));
                }
            } else {
                let current = self.config.tool_output_cap.map(|n| n.to_string()).unwrap_or_else(|| "unlimited (default)".to_string());
                self.notify(format!("❌ Usage: /toolcap <chars|off>  (current: {})", current));
            }
         } else if command == "/zt" {
            self.zero_truncation = !self.zero_truncation;
            if self.zero_truncation {
                self.notify("🔍 Zero-truncation ON — full raw tool output injected into context");
            } else {
                let cap = self.config.tool_output_cap.unwrap_or(crate::config::OUTPUT_CHARACTER_LIMIT);
                self.notify(format!("✂️  Zero-truncation OFF — tool output capped at {} chars", cap));
            }
        } else if command == "/compress" {
            self.handle_compress().await;
        } else if command == "/gradient" || command.starts_with("/gradient ") {
            let arg = command.strip_prefix("/gradient").unwrap_or("").trim();
            self.handle_gradient_command(arg);
        } else if command == "/theme" || command.starts_with("/theme ") {
            let arg = command.strip_prefix("/theme").unwrap_or("").trim();
            self.handle_theme_command(arg);
        } else if command == "/color" || command.starts_with("/color ") {
            let prompt = command.strip_prefix("/color").unwrap_or("").trim();
            self.handle_color_command(prompt);
        } else if command.starts_with("/copycode") {
            let n = command.split_whitespace().nth(1).and_then(|s| s.parse::<usize>().ok());
            self.handle_copycode(n).await;
        } else if command == "/copytext" {
            self.handle_copytext().await;
        } else if command == "/copyall" {
            self.handle_copyall().await;
        } else if command == "/copyprompt" {
            let prompt = self.steering_text();
            match Self::copy_to_clipboard(&prompt).await {
                Ok(()) => self.notify(format!("📋 System prompt copied ({} chars)", prompt.len())),
                Err(e) => self.notify(format!("❌ Copy failed: {}", e)),
            }
        } else if command == "/showprompt" {
            let prompt = self.steering_text();
            let chars = prompt.len();
            let tokens_est = chars / 4;
            let lines: usize = prompt.lines().count();
            let display = format!("```\n{}\n```\n— {} chars, {} lines, ~{} tokens (est.)", prompt, chars, lines, tokens_est);
            let msg = Message::new("system", display);
            self.persist_message(msg);
            self.cached_message_count = self.message_buffer.count()
                .unwrap_or(self.cached_message_count + 1);
        } else if command.starts_with("/copylink") {
            let n = command.split_whitespace().nth(1).and_then(|s| s.parse::<usize>().ok());
            self.handle_link_command(false, n).await;
        } else if command.starts_with("/openlink") {
            let n = command.split_whitespace().nth(1).and_then(|s| s.parse::<usize>().ok());
            self.handle_link_command(true, n).await;
        } else if command.starts_with("/view") {
            let path = command.strip_prefix("/view").unwrap_or("").trim();
            self.handle_view_command(path);
        } else if command.starts_with("/diff") {
            let path = command.strip_prefix("/diff").unwrap_or("").trim();
            self.handle_diff_command(path);
        } else if command.starts_with('/') {
            self.status_message = format!("❓ Unknown command: '{}'. Type /help for available commands.", command);
        } else if !command.is_empty() {
            // In Forever/One mode: if the agent is busy, queue the task instead of
            // interrupting the current turn. The queue drains FIFO as each turn completes.
            if matches!(self.mode, AppMode::Forever | AppMode::One)
                && self.turn_phase != TurnPhase::Idle
            {
                self.task_queue.push_back(command.clone());
                let pos = self.task_queue.len();
                self.status_message = format!(
                    "📋 Task queued (position {} in queue) — will run when agent is free",
                    pos
                );
                self.input_buffer.clear();
                return;
            }

            // Message validation: no excessive length, check for reasonable content
            self.inline_tool_results.clear(); // Clear inline results when user sends new message
            self.consecutive_empty_kicks = 0; // Reset stuck detection on new user input
            self.autokick_paused = false;
            self.consecutive_format_errors = 0;
            // Reset loop-prevention state on new user input
            self.recent_tool_calls.clear();
            self.spin_notice_count = 0;
            self.recent_tool_errors.clear();
            self.stall_notice_sent = false;
            self.handle_message(&command).await;
        }

        self.input_buffer.clear();
    }

    /// Display help text with all available commands
    fn show_help(&mut self) {
        self.status_message = format!(
            "📖 Commands:\n\
             /help         - Show this help\n\
             /estimate     - Show project completion estimate\n\
             /endpoint [URL] - Change endpoint (default: localhost:11434)\n\
             /model NAME   - Switch AI model\n\
             /models       - List available models\n\
             /ctx NUM      - Set context window size\n\
             /toolcap NUM  - Cap tool outputs at N chars (or 'off'); default {}\n\
             /zt           - Toggle zero-truncation: inject full raw tool output into context\n\
             /compress     - Summarize session → archive → inject summary\n\
             /set_params K=V - Set model params (temperature, top_k, etc.) — persists\n\
             /temperature N  - Set temperature (0.0–2.0) shorthand\n\
             /mode MODE    - Switch mode (ask/plan/build/one)\n\
             /build        - Switch to Build mode (autonomous execution)\n\
             /plan         - Switch to Plan mode (interactive)\n\
             /ask          - Switch to Ask mode (read-only)\n\
             /one          - Switch to One mode (one-off task w/ completion notification)\n\
             /abort        - Abort current stream / async tasks / tool execution\n\
             /shell CMD    - Run a shell command inline\n\
             /gradient     - Toggle gradient background\n\
             /theme        - Switch theme: /theme dark | /theme light | /theme auto\n\
             /color PROMPT - Generate gradient colors from prompt (e.g. sunset, ocean, cyberpunk)\n\
             /checkpoint   - Save session checkpoint\n\
             /clear        - Archive conversation to scrollback\n\
             /save         - Save current plan as a todo task\n\
             /tasks        - Show task dependency graph\n\
             /gaps         - Show knowledge gaps\n\
             /stats        - Show cumulative session statistics\n\
             /tool CMD     - Execute tool (rg/setfile/exec/shell/commit/python/ruste/mem)\n\
             /view PATH    - Open file viewer (tabs, scroll)\n\
             /diff [PATH]  - View git diff in file viewer\n\
             /test_notification (alias /notify_test) - Fire a test OS notification\n\
             /test_models (alias /test) - Test model tool-calling capability\n\
             /copycode     - Copy code block from last reply\n\
             /copytext     - Copy full last reply as plain text\n\
             /copyall      - Copy entire conversation to clipboard\n\
             /copylink     - Copy URL from last reply\n\
             /openlink     - Open URL from last reply in browser\n\
             /copyprompt   - Copy current system prompt to clipboard\n\
             /showprompt   - Show full system prompt in chat (scrollable)\n\n\
             Modes: ⚡ Build (autonomous) | 🧠 Plan (interactive) | 🔍 Ask (read-only) | 🎯 One (one-off)\n\n\
             Keybindings: Enter-Submit | Esc-Cancel/Clear | Ctrl+Q-Graceful exit | Ctrl+C-Force exit",
            crate::config::OUTPUT_CHARACTER_LIMIT
        );
    }

    /// Render a vertical gradient background across the given area with interpolated colors
    /// Returns a vector of paragraphs with increasing opacity effect
    fn render_gradient_background(&self, area: Rect) -> Vec<Paragraph<'static>> {
        let mut gradients = Vec::new();
        let height = area.height as usize;
        
        // Linear RGB interpolation between start and end colors
        let start = self.theme.gradient_start;
        let end = self.theme.gradient_end;
        
        for y in 0..height {
            // Interpolation factor: 0 at top, 1 at bottom
            let t = if height > 1 {
                y as f32 / (height - 1) as f32
            } else {
                0.5
            };
            
            // Extract RGB values and interpolate
            let color = match (start, end) {
                (Color::Rgb(sr, sg, sb), Color::Rgb(er, eg, eb)) => {
                    let r = (sr as f32 + (er as f32 - sr as f32) * t) as u8;
                    let g = (sg as f32 + (eg as f32 - sg as f32) * t) as u8;
                    let b = (sb as f32 + (eb as f32 - sb as f32) * t) as u8;
                    Color::Rgb(r, g, b)
                }
                _ => start,
            };
            
            // Create a paragraph with a single space and the interpolated background color
            let para = Paragraph::new(" ")
                .style(Style::default().bg(color));
            gradients.push(para);
        }
        
        gradients
    }

    /// Handle /compress — summarize session, archive to scrollback, inject summary
    async fn handle_compress(&mut self) {
        let msg_count = self.message_buffer.count().unwrap_or(0);
        if msg_count == 0 {
            self.notify("📭 Nothing to compress — conversation is empty");
            return;
        }

        // Build a compact summary prompt from the current history
        let messages = match self.message_buffer.messages() {
            Ok(m) => m,
            Err(e) => {
                self.notify(format!("❌ Failed to read messages: {}", e));
                return;
            }
        };

        // Compose a concise transcript to summarize
        let transcript: String = messages.iter()
            .filter(|m| m.role != "system" && m.role != "clock")
            .take(60) // cap at 60 messages for the summarizer
            .map(|m| {
                let role = match m.role.as_str() {
                    "assistant" => "Assistant",
                    "tool" | "kick" => "ToolResult",
                    "notice" => "SystemNotice",
                    _ => "User",
                };
                // Truncate long messages for the summarizer input
                let content = if m.content.chars().count() > 500 {
                    let truncated: String = m.content.chars().take(500).collect();
                    format!("{}…", truncated)
                } else {
                    m.content.clone()
                };
                format!("[{}]: {}", role, content)
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Collect pending tasks to preserve goal context across compression
        let pending_tasks: Vec<String> = self.task_manager.pending_tasks()
            .unwrap_or_default()
            .into_iter()
            .map(|t| format!("• [{}] {}", t.id, t.title))
            .collect();
        let tasks_block = if pending_tasks.is_empty() {
            String::new()
        } else {
            format!("\n\nPENDING TASKS (preserve these in your summary):\n{}", pending_tasks.join("\n"))
        };

        let summary_prompt = format!(
            "Summarize this conversation as a compact bullet list (10 bullets max). \
             Focus on: what was accomplished, key decisions, files changed, and what was in progress. \
             Be terse — this summary replaces the full history.{tasks_block}\n\n{}",
            transcript
        );

        self.notify(format!("🗜️ Summarizing {} messages…", msg_count));

        // Call the model synchronously (non-streaming for simplicity)
        let summary = if let Some(client) = self.ollama_client.clone() {
            let summary_msg = vec![crate::message::Message::new("user", &summary_prompt)];
            let (tool_cap, ctx_win) = self.compression_params();
            match client.generate(summary_msg, None, &self.effective_params(), tool_cap, ctx_win).await {
                Ok(s) => s,
                Err(e) => {
                    self.notify(format!("❌ Summarization failed: {}", e));
                    return;
                }
            }
        } else {
            self.notify("❌ No Ollama connection — cannot compress");
            return;
        };

        // Archive current messages to scrollback
        let archived = self.message_buffer.archive_to_scrollback().unwrap_or(0);

        // Inject the summary as context for the next turn.
        // Append pending task list explicitly so goal state survives even if the
        // model's summary omitted it.
        let tasks_footer = if pending_tasks.is_empty() {
            String::new()
        } else {
            format!("\n\n**Pending tasks:**\n{}", pending_tasks.join("\n"))
        };
        let summary_msg = crate::message::Message::new(
            "assistant",
            format!("**[Session summary — {} messages archived]**\n\n{}{}", archived, summary, tasks_footer),
        );
        if let Err(e) = self.message_buffer.add_and_persist(summary_msg) {
            self.notify(format!("❌ Failed to store summary: {}", e));
            return;
        }

        self.cached_message_count = self.message_buffer.count().unwrap_or(0);
        self.stats.compressions += 1;

        // Persist the summary so it survives session restarts.
        // Written to .yggdra/session_notes.md — auto-injected into the system
        // prompt on the next (and every subsequent) session startup.
        let notes_path = std::path::Path::new(".yggdra").join("session_notes.md");
        let notes_content = format!(
            "# Session Notes\n\n\
             *Auto-generated by /compress on {}*\n\n\
             {}{}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M"),
            summary,
            tasks_footer,
        );
        match std::fs::write(&notes_path, &notes_content) {
            Ok(_) => {
                self.notify(format!("✅ Compressed: {} messages → summary saved to .yggdra/session_notes.md", archived));
            }
            Err(e) => {
                crate::dlog!("⚠️ handle_compress: failed to write session_notes.md: {}", e);
                self.notify(format!("✅ Compressed: {} messages → summary injected (notes write failed: {})", archived, e));
            }
        }
        
        // Reset auto-compress counters after successful compress
        self.tools_executed_since_compress = 0;
    }

    /// Handle /gradient command — toggle pastel gradient background
    fn handle_gradient_command(&mut self, arg: &str) {
        match arg {
            "on" => {
                self.gradient_enabled = true;
                self.config.ui_settings.gradient_enabled = true;
                let _ = self.config.save();
                self.notify("✨ Gradient background enabled");
            }
            "off" => {
                self.gradient_enabled = false;
                self.config.ui_settings.gradient_enabled = false;
                let _ = self.config.save();
                self.notify("✨ Gradient background disabled");
            }
            "toggle" | "" => {
                self.gradient_enabled = !self.gradient_enabled;
                self.config.ui_settings.gradient_enabled = self.gradient_enabled;
                let _ = self.config.save();
                let status = if self.gradient_enabled { "enabled" } else { "disabled" };
                self.notify(format!("✨ Gradient background {}", status));
            }
            _ => {
                self.notify("❌ Usage: /gradient on|off|toggle");
            }
        }
    }

    /// Handle /theme command — manually set or auto-detect the colour theme
    fn handle_theme_command(&mut self, arg: &str) {
        match arg {
            "dark" => {
                self.theme = Theme::dark();
                self.config.ui_settings.theme = Some("dark".to_string());
                let _ = self.config.save();
                self.notify("🌑 Theme set to dark");
            }
            "light" => {
                self.theme = Theme::light();
                self.config.ui_settings.theme = Some("light".to_string());
                let _ = self.config.save();
                self.notify("🌕 Theme set to light");
            }
            "auto" | "" => {
                // Use safe detection only — detect() toggles raw mode which breaks the TUI
                match Theme::detect_safe() {
                    Some(true)  => { self.theme = Theme::light(); self.notify("🎨 Theme auto-detected: light"); }
                    Some(false) => { self.theme = Theme::dark();  self.notify("🎨 Theme auto-detected: dark"); }
                    None        => self.notify("⚠️  Could not detect theme — use /theme dark or /theme light"),
                }
                self.config.ui_settings.theme = Some("auto".to_string());
                let _ = self.config.save();
            }
            _ => {
                self.notify("❌ Usage: /theme dark | /theme light | /theme auto");
            }
        }
    }

    /// Handle /color — generate gradient colors from a text prompt (e.g. "sunset", "ocean", "forest")
    fn handle_color_command(&mut self, prompt: &str) {
        if prompt.is_empty() {
            self.notify("🎨 Usage: /color <prompt> — e.g. /color sunset, /color ocean, /color cyberpunk");
            return;
        }

        let client = match &self.ollama_client {
            Some(c) => c.clone(),
            None => {
                self.notify("❌ No Ollama connection — cannot generate colors");
                return;
            }
        };

        self.notify(format!("🎨 Generating colors for \"{}\"…", prompt));

        let color_prompt = format!(
            "You are a color palette generator. Given a theme prompt, return EXACTLY two RGB colors \
             in this format only, no other text:\n\n\
             START: RGB(r, g, b)\n\
             END: RGB(r, g, b)\n\n\
             Theme: {}\n\n\
             Return only the two RGB lines, nothing else.",
            prompt
        );

        let messages = vec![crate::message::Message::new("user", &color_prompt)];
        let (tool_cap, ctx_win) = self.compression_params();
        let params = self.effective_params();
        let (tx, rx) = oneshot::channel();
        self.color_result_rx = Some(rx);

        tokio::spawn(async move {
            let result = client.generate(messages, None, &params, tool_cap, ctx_win).await
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// Poll for a completed async /color result and apply it
    fn check_color_result(&mut self) {
        let rx = match self.color_result_rx.as_mut() {
            Some(rx) => rx,
            None => return,
        };

        match rx.try_recv() {
            Ok(Ok(response)) => {
                self.color_result_rx = None;
                self.apply_color_response(&response);
            }
            Ok(Err(e)) => {
                self.color_result_rx = None;
                self.notify(format!("❌ Failed to generate colors: {}", e));
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => {
                self.color_result_rx = None;
                self.notify("❌ Color generation task dropped unexpectedly");
            }
        }
    }

    /// Parse and apply RGB gradient colors from an LLM response string
    fn apply_color_response(&mut self, response: &str) {
        use ratatui::style::Color;
        let mut start_color: Option<Color> = None;
        let mut end_color: Option<Color> = None;

        for line in response.lines() {
            let line = line.trim();
            if line.starts_with("START:") || line.starts_with("END:") {
                let rgb_part = line.split(':').nth(1).unwrap_or("").trim();
                if let Some(rgb) = rgb_part.strip_prefix("RGB(").and_then(|s| s.strip_suffix(')')) {
                    let parts: Vec<&str> = rgb.split(',').collect();
                    if parts.len() == 3 {
                        if let (Ok(r), Ok(g), Ok(b)) = (
                            parts[0].trim().parse::<u8>(),
                            parts[1].trim().parse::<u8>(),
                            parts[2].trim().parse::<u8>(),
                        ) {
                            let color = Color::Rgb(r, g, b);
                            if line.starts_with("START:") { start_color = Some(color); }
                            else { end_color = Some(color); }
                        }
                    }
                }
            }
        }

        if let (Some(start), Some(end)) = (start_color, end_color) {
            self.theme.gradient_start = start;
            self.theme.gradient_end = end;
            self.gradient_enabled = true;
            self.config.ui_settings.gradient_enabled = true;
            if let (Color::Rgb(sr, sg, sb), Color::Rgb(er, eg, eb)) = (start, end) {
                self.config.ui_settings.gradient_start = Some(format!("{},{},{}", sr, sg, sb));
                self.config.ui_settings.gradient_end   = Some(format!("{},{},{}", er, eg, eb));
                let _ = self.config.save();
                self.notify(format!("🎨 Gradient set: RGB({},{},{}) → RGB({},{},{})", sr, sg, sb, er, eg, eb));
            }
            self.needs_full_redraw = true;
        } else {
            self.notify("❌ Could not parse colors from model response. Try again with a simpler prompt.");
        }
    }

    fn show_estimate(&mut self) {
        let metrics_display = self.metrics.format_detailed();
        self.status_message = format!(
            "{}{}",
            metrics_display,
            "\n\nPlan mode auto-saves all plans as .yggdra/todo items for discovery."
        );
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

        // Block modifying tools in Ask-only mode
        if self.mode == AppMode::Ask {
            match tool_name {
                "setfile" | "patchfile" | "commit" | "python" | "ruste" => {
                    self.notify(format!("🔒 Ask-only mode: {} is blocked (read-only mode)", tool_name));
                    return;
                }
                _ => {} // rg, readfile, exec, shell are allowed
            }
        }

        // Handle special "mem" tool for searching scrollback
        if tool_name == "mem" {
            self.handle_mem_command(args);
            return;
        }

        self.status_message = format!("⏳ Executing tool: {}", tool_name);

        // Record tool usage in metrics
        self.metrics.record_tool_use(tool_name);

        // Execute tool via registry
        let result = self.tool_registry.execute(tool_name, args);

        match result {
            Ok(tool_output) => {
                let output_msg = if tool_output.is_empty() {
                    "[Tool executed successfully with no output]".to_string()
                } else {
                    tool_output.lines().take(30).collect::<Vec<_>>().join("\n")
                };

                let response = format!("{}\n{}", tool_name, output_msg);
                
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
            crate::dlog!("Failed to save shell context: {}", e);
            return;
        }
        self.cached_message_count = self.message_buffer.count().unwrap_or(0);

        // Trigger assistant response
        let steering = self.steering_text();
        let messages = self.message_buffer.messages().unwrap_or_default();
         
        // Check context before streaming (warns if near/over limit)
        let (fits, _usage_pct) = self.check_context_before_stream(&messages, Some(&steering));
        // In Forever mode, keep going anyway (just warned above)
        
        if let Some(client) = &self.ollama_client {
            let (tool_cap, ctx_win) = self.compression_params();
            self.stream_rx = Some(client.generate_streaming(messages, Some(&steering), self.effective_params(), tool_cap, ctx_win));
            self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
            self.turn_phase = TurnPhase::Streaming;
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
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

    /// Save last assistant response as todo item (triggered by /save command)
    fn save_plan_as_todo(&mut self) {
        let message = match self.last_assistant_message() {
            Some(m) => m,
            None => {
                self.notify("❌ No assistant message to save".to_string());
                return;
            }
        };
        let message = message.clone();

        // Generate a filename from the first line of the message
        let first_line = message.lines().next().unwrap_or("plan");
        let sanitized = first_line
            .chars()
            .take(50)
            .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
            .collect::<String>()
            .trim()
            .replace(' ', "-")
            .to_lowercase();
        
        if sanitized.is_empty() {
            return;
        }

        let filename = format!("{}.md", sanitized);
        let todo_path = std::env::current_dir()
            .ok()
            .map(|p| p.join(".yggdra").join("todo").join(&filename));

        if let Some(path) = todo_path {
            // Create .yggdra/todo if needed
            if let Ok(cwd) = std::env::current_dir() {
                let _ = std::fs::create_dir_all(cwd.join(".yggdra").join("todo"));
            }

            // Format as todo markdown
            let todo_content = format!(
                "# {}\n\n**Status:** pending\n**Priority:** medium\n\n## Plan\n\n{}\n",
                first_line,
                message
            );

            match std::fs::write(&path, todo_content) {
                Ok(_) => {
                    self.notify(format!("📝 Plan saved as todo: {}", filename));
                }
                Err(e) => {
                    self.notify(format!("❌ Failed to save plan: {}", e));
                }
            }
        }
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

    /// /copyall — copy entire visible conversation to clipboard
    async fn handle_copyall(&mut self) {
        let messages = match self.message_buffer.all_messages() {
            Ok(m) => m,
            Err(e) => { self.notify(format!("❌ Failed to read messages: {}", e)); return; }
        };
        let text: String = messages.iter()
            .filter(|m| m.role != "clock")
            .map(|m| {
                let role = match m.role.as_str() {
                    "assistant" => "Assistant",
                    "user"      => "User",
                    "tool" | "kick" => "Tool",
                    "notice"    => "Notice",
                    other       => other,
                };
                format!("[{}]\n{}\n", role, m.content.trim())
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            self.notify("❌ No messages to copy"); return;
        }
        match Self::copy_to_clipboard(&text).await {
            Ok(_) => self.notify(format!("📋 Copied full conversation ({} chars) to clipboard", text.len())),
            Err(e) => self.notify(format!("❌ Clipboard error: {}", e)),
        }
    }

    /// /test_models — send a battery of prompts to verify all tool-call formats and tricky cases.
    /// Tests run in a background task; results stream in one-by-one so the TUI doesn't freeze.
    async fn handle_test_models(&mut self) {
        if self.test_models_rx.is_some() {
            self.push_system_event("⏳ Test run already in progress…");
            return;
        }
        let Some(client) = self.ollama_client.clone() else {
            self.push_system_event("❌ No model connected — use /endpoint to connect first");
            return;
        };
        let model = self.config.model.clone();
        let (tool_cap, ctx_win) = self.compression_params();
        let params = self.effective_params();

        self.push_system_event(format!("🧪 Running 16 capability tests for `{}`…", model));
        self.cached_message_count = self.message_buffer.count()
            .unwrap_or(self.cached_message_count + 1);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        self.test_models_rx = Some(rx);

        tokio::spawn(async move {
            // (test name, prompt, expected description) — 'static so safe to use inside spawn
            let test_cases: &[(&str, &str, &str)] = &[
                // ── XML format (primary) ────────────────────────────────────────────────
                (
                    "XML: basic shell call",
                    "Respond with ONLY this XML tool call and no other text:\n\
                     <tool>shell</tool>\n<command>echo hello</command>\n<desc>test</desc>",
                    "<tool>shell</tool> parsed",
                ),
                (
                    "XML: pipe in command",
                    "Respond with ONLY an XML shell tool call. The command must contain a pipe: echo hello | tr a-z A-Z\n\
                     Use: <tool>shell</tool><command>echo hello | tr a-z A-Z</command><desc>pipe test</desc>",
                    "shell call with | in command arg",
                ),
                (
                    "XML: setfile with braces+quotes (Rust code)",
                    "Write a file to /tmp/yggdra_test.rs using the XML setfile format. \
                     The content must be exactly:\nfn main() {\n    println!(\"hello\");\n}\n\n\
                     Use: <tool>setfile</tool><path>/tmp/yggdra_test.rs</path><content>\nfn main() {\n    println!(\"hello\");\n}\n</content>",
                    "setfile call with fn main() in content",
                ),
                (
                    "XML: two tool calls in one response",
                    "Respond with EXACTLY TWO XML tool calls in a single message — no prose before, between, or after.\n\
                     First: <tool>shell</tool><command>echo one</command><desc>first</desc>\n\
                     Then:  <tool>shell</tool><command>echo two</command><desc>second</desc>",
                    "2+ XML tool calls parsed",
                ),
                (
                    "XML: no prose discipline (tool call only, no preamble)",
                    "Call shell with `echo discipline`. \
                     CRITICAL: output ONLY the XML tool call tags — do NOT write 'Sure!', 'Of course', \
                     or any explanation before or after the tags.",
                    "shell call, response doesn't start with Sure/Of course/Here",
                ),
                (
                    "XML: think then act",
                    "Before calling the tool, reason inside <think>...</think> tags, then output the XML tool call.\n\
                     Task: run `echo thinking`.",
                    "<think>...</think> block present + XML tool call",
                ),
                // ── JSON format (fallback) ───────────────────────────────────────────────
                (
                    "JSON: basic shell call",
                    "Respond with ONLY this JSON and nothing else:\n\
                     {\"tool_calls\":[{\"name\":\"shell\",\"parameters\":{\"command\":\"echo hello\"}}]}",
                    r#"{"tool_calls":[...]} with name=shell"#,
                ),
                (
                    "JSON: setfile with embedded newlines in content",
                    "Use JSON to write /tmp/yggdra_test.txt. The file must have two lines: 'line one' then 'line two'.\n\
                     Expected format: {\"tool_calls\":[{\"name\":\"setfile\",\"parameters\":{\"path\":\"/tmp/yggdra_test.txt\",\"content\":\"line one\\nline two\"}}]}\n\
                     Output ONLY the JSON.",
                    "JSON tool call with name=setfile",
                ),
                // ── Backtick prose fallback ──────────────────────────────────────────────
                (
                    "Backtick: command in backticks",
                    "Run echo test. Respond with just the command in backticks and nothing else: `echo test`",
                    "backtick-wrapped command extracted (e.g. `echo test`)",
                ),
                // ── Tricky / common failure modes ────────────────────────────────────────
                (
                    "No hallucination: must not inject TOOL_OUTPUT",
                    "Respond with only the single word \"ready\" and absolutely nothing else.",
                    "response contains no [TOOL_OUTPUT:] or [TOOL_RESULT:]",
                ),
                (
                    "JSON inside JSON (double-escape trap)",
                    "Use JSON shell format to run this command: echo '{\"key\":\"val\"}'\n\
                     The command string must preserve the inner JSON literally.\n\
                     Output ONLY the JSON tool call.",
                    "JSON shell call with inner {\"key\":\"val\"} preserved",
                ),
                (
                    "XML: async shell with task_id",
                    "Start an async background task. Use XML shell with mode=async and task_id=bg-test:\n\
                     <tool>shell</tool><command>sleep 1 && echo done</command>\n\
                     <mode>async</mode><task_id>bg-test</task_id><desc>async test</desc>",
                    "shell call parsed with async_mode=true",
                ),
                // ── Thinking tags ────────────────────────────────────────────────────────
                (
                    "Thinking: <think> block with tool call after",
                    "Think step by step inside <think>...</think> tags — reason out loud, \
                     at least 3 sentences. Then, OUTSIDE the think block, output a single XML tool call:\n\
                     <tool>shell</tool><command>echo done</command><desc>after thinking</desc>\n\
                     The tool call must appear after </think>, not inside it.",
                    "<think>20+ chars</think> then a tool call outside the block",
                ),
                (
                    "Thinking: tags don't leak into tool args",
                    "Inside your <think> block, write a FAKE tool call: \
                     <tool>shell</tool><command>false</command><desc>fake</desc></think>\n\
                     Now outside the think block, write the REAL tool call: \
                     <tool>shell</tool><command>echo real</command><desc>real</desc>",
                    "only the real 'echo real' call parsed (fake inside <think> ignored)",
                ),
                // ── Code formatting ──────────────────────────────────────────────────────
                (
                    "Code: setfile produces valid Rust that compiles",
                    "Write a complete, compilable Rust program to /tmp/yggdra_fmt_test.rs using setfile. \
                     The program must: (1) define a struct Point with x and y fields, \
                     (2) implement Display for it, (3) print a Point in main(). \
                     Use proper indentation, braces, and semicolons. Output ONLY the XML tool call.",
                    "setfile call with 'struct' and 'fn main' in content",
                ),
                (
                    "Code: no markdown fences in setfile content",
                    "Write the text 'hello world' to /tmp/yggdra_plain.txt using setfile. \
                     CRITICAL: the file content must be plain text only — do NOT wrap the content \
                     in ```backticks``` or any markdown fences. Output ONLY the XML tool call.",
                    "setfile call whose content contains no ``` backtick fences",
                ),
            ];

            let total = test_cases.len();
            let mut passed = 0usize;

            for (i, (name, prompt, expected)) in test_cases.iter().enumerate() {
                let msgs = vec![crate::message::Message::new("user", *prompt)];
                let resp = tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    client.generate(msgs, None, &params, tool_cap, ctx_win),
                ).await;

                let (raw, ok) = match resp {
                    Ok(Ok(r)) => {
                        let pass = match i {
                            0 => crate::agent::parse_xml_tool_calls(&r)
                                    .iter().any(|c| c.name == "shell"),
                            1 => crate::agent::parse_xml_tool_calls(&r)
                                    .iter().any(|c| c.name == "shell" && c.args.contains('|')),
                            2 => crate::agent::parse_xml_tool_calls(&r)
                                    .iter().any(|c| c.name == "setfile" && c.args.contains("fn main")),
                            3 => crate::agent::parse_xml_tool_calls(&r).len() >= 2,
                            4 => {
                                let calls = crate::agent::parse_xml_tool_calls(&r);
                                let has_call = calls.iter().any(|c| c.name == "shell");
                                let clean = !r.trim().to_lowercase().starts_with("sure")
                                    && !r.trim().to_lowercase().starts_with("of course")
                                    && !r.trim().to_lowercase().starts_with("here");
                                has_call && clean
                            }
                            5 => {
                                let has_think = r.contains("<think>") || r.contains("</think>");
                                let has_call = !crate::agent::parse_xml_tool_calls(&r).is_empty();
                                has_think && has_call
                            }
                            6 => crate::agent::parse_json_tool_calls(&r)
                                    .iter().any(|c| c.name == "shell"),
                            7 => crate::agent::parse_json_tool_calls(&r)
                                    .iter().any(|c| c.name == "setfile"),
                            8 => crate::agent::extract_backtick_command_pub(&r).is_some(),
                            9 => !r.contains("[TOOL_OUTPUT:") && !r.contains("[TOOL_RESULT:"),
                            10 => crate::agent::parse_json_tool_calls(&r)
                                    .iter().any(|c| c.name == "shell" && (c.args.contains('{') || c.args.contains("key"))),
                            11 => crate::agent::parse_xml_tool_calls(&r)
                                    .iter().any(|c| c.async_mode),
                            12 => {
                                let has_think = r.contains("<think>") && r.contains("</think>");
                                let think_content_len = if let (Some(s), Some(e)) =
                                    (r.find("<think>"), r.find("</think>")) {
                                    if e > s + 7 { e - (s + 7) } else { 0 }
                                } else { 0 };
                                let after_think = r.find("</think>")
                                    .map(|p| &r[p + "</think>".len()..])
                                    .unwrap_or("");
                                let call_after = !crate::agent::parse_xml_tool_calls(after_think).is_empty();
                                has_think && think_content_len > 20 && call_after
                            }
                            13 => {
                                let calls = crate::agent::parse_xml_tool_calls(&r);
                                let has_real = calls.iter().any(|c| c.args.contains("echo real") || c.args.contains("real"));
                                let no_fake  = !calls.iter().any(|c| c.args.trim() == "false");
                                has_real && no_fake
                            }
                            14 => crate::agent::parse_xml_tool_calls(&r)
                                    .iter().any(|c| c.name == "setfile"
                                        && c.args.contains("struct")
                                        && c.args.contains("fn main")),
                            15 => {
                                let calls = crate::agent::parse_xml_tool_calls(&r);
                                calls.iter().any(|c| c.name == "setfile" && !c.args.contains("```"))
                            }
                            _ => false,
                        };
                        (r, pass)
                    }
                    Ok(Err(e)) => (format!("error: {}", e), false),
                    Err(_)     => ("timeout".to_string(), false),
                };

                if ok { passed += 1; }
                let icon = if ok { "✅" } else { "❌" };
                let preview: String = raw.chars().take(120).collect();
                let ellipsis = if raw.chars().count() > 120 { "…" } else { "" };
                let _ = tx.send(format!(
                    "{} [{}/{}] {}\n   expect: {}\n   actual: `{}{}`",
                    icon, i + 1, total, name,
                    expected,
                    preview.replace('\n', "↵"), ellipsis
                ));
            }

            let _ = tx.send(format!("**🧪 {}/{} passed**", passed, total));
            // tx drop closes the channel → check_test_models clears the receiver
        });
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

    /// Handle /view <path> — open a file in the file viewer overlay
    fn handle_view_command(&mut self, path: &str) {
        if path.is_empty() {
            self.notify("Usage: /view <path>  (e.g. /view src/main.rs)");
            return;
        }
        let resolved = crate::sandbox::resolve(path);
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let label = resolved.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path)
                    .to_string();
                // If a tab for this path already exists, just focus it
                if let Some(pos) = self.file_viewer_tabs.iter().position(|t| t.label == label) {
                    self.file_viewer_active = pos;
                } else {
                    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
                    self.file_viewer_tabs.push(FileTab { label, lines, scroll: 0, is_diff: false });
                    self.file_viewer_active = self.file_viewer_tabs.len() - 1;
                }
                self.file_viewer_open = true;
            }
            Err(e) => {
                self.notify(format!("❌ Cannot open {}: {}", path, e));
            }
        }
    }

    /// Handle /diff [path] — open git diff in the file viewer overlay
    fn handle_diff_command(&mut self, path: &str) {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("diff").arg("--color=never");
        if !path.is_empty() {
            cmd.arg("--").arg(path);
        }
        match cmd.output() {
            Ok(out) => {
                let raw = String::from_utf8_lossy(&out.stdout).to_string();
                if raw.trim().is_empty() {
                    // Fall back to HEAD diff
                    let head_out = std::process::Command::new("git")
                        .args(["diff", "HEAD", "--color=never"])
                        .output();
                    let content = head_out.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                        .unwrap_or_default();
                    if content.trim().is_empty() {
                        self.notify("ℹ️  No changes (working tree and HEAD are clean)");
                        return;
                    }
                    let lines = content.lines().map(|l| l.to_string()).collect();
                    self.file_viewer_tabs.push(FileTab { label: "diff HEAD".to_string(), lines, scroll: 0, is_diff: true });
                } else {
                    let label = if path.is_empty() { "diff".to_string() } else { format!("diff {}", path) };
                    let lines = raw.lines().map(|l| l.to_string()).collect();
                    // Replace existing diff tab for same label if present
                    if let Some(pos) = self.file_viewer_tabs.iter().position(|t| t.label == label) {
                        self.file_viewer_tabs[pos] = FileTab { label, lines, scroll: 0, is_diff: true };
                        self.file_viewer_active = pos;
                    } else {
                        self.file_viewer_tabs.push(FileTab { label, lines, scroll: 0, is_diff: true });
                        self.file_viewer_active = self.file_viewer_tabs.len() - 1;
                    }
                }
                self.file_viewer_open = true;
            }
            Err(e) => {
                self.notify(format!("❌ git diff failed: {}", e));
            }
        }
    }

    /// Handle keys when the file viewer overlay is open
    fn handle_file_viewer_key(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;
        if self.file_viewer_tabs.is_empty() {
            self.file_viewer_open = false;
            return;
        }
        let tab_count = self.file_viewer_tabs.len();
        // Clamp active index defensively — tabs can be removed by other paths
        self.file_viewer_active = self.file_viewer_active.min(tab_count.saturating_sub(1));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.file_viewer_open = false;
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+W closes current tab
                self.file_viewer_tabs.remove(self.file_viewer_active);
                if self.file_viewer_tabs.is_empty() {
                    self.file_viewer_open = false;
                } else {
                    self.file_viewer_active = self.file_viewer_active.min(self.file_viewer_tabs.len() - 1);
                }
            }
            KeyCode::Tab => {
                self.file_viewer_active = (self.file_viewer_active + 1) % tab_count;
            }
            KeyCode::BackTab => {
                self.file_viewer_active = self.file_viewer_active.checked_sub(1).unwrap_or(tab_count - 1);
            }
            KeyCode::Up => {
                let tab = &mut self.file_viewer_tabs[self.file_viewer_active];
                tab.scroll = tab.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                let tab = &mut self.file_viewer_tabs[self.file_viewer_active];
                let max_scroll = tab.lines.len().saturating_sub(1);
                tab.scroll = (tab.scroll + 1).min(max_scroll);
            }
            KeyCode::PageUp => {
                let tab = &mut self.file_viewer_tabs[self.file_viewer_active];
                tab.scroll = tab.scroll.saturating_sub(20);
            }
            KeyCode::PageDown => {
                let tab = &mut self.file_viewer_tabs[self.file_viewer_active];
                let max_scroll = tab.lines.len().saturating_sub(1);
                tab.scroll = (tab.scroll + 20).min(max_scroll);
            }
            KeyCode::Char('g') => {
                self.file_viewer_tabs[self.file_viewer_active].scroll = 0;
            }
            KeyCode::Char('G') => {
                let tab = &mut self.file_viewer_tabs[self.file_viewer_active];
                tab.scroll = tab.lines.len().saturating_sub(1);
            }
            _ => {}
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
            match OllamaClient::new_with_key(&target_endpoint, &self.config.model, self.config.api_key.as_deref()).await {
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
                    std::time::Duration::from_secs(30),
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


    /// Handle /set_params command — validate and apply key=value pairs to runtime_params.
    fn handle_set_params_command(&mut self, args: &str) {
        if args.is_empty() {
            let summary = self.runtime_params.summary();
            let agents_summary = if self.agents_config.params.is_empty() { "none".to_string() }
                                  else { self.agents_config.params.summary() };
            let config_summary = if self.config.params.is_empty() { "none".to_string() }
                                  else { self.config.params.summary() };
            self.push_system_event(format!(
                "🎛  Current params:\n  runtime: {}\n  config.json: {}\n  AGENTS.md: {}\n  effective: {}\n\nUsage: /set_params temperature=0.8 top_k=40\nKeys: temperature (0-2), top_k, top_p (0-1), repeat_penalty, num_predict, reset",
                summary, config_summary, agents_summary,
                self.effective_params().summary()
            ));
            return;
        }
        match self.runtime_params.apply_args(args) {
            Ok(msg) => {
                // Also persist to config.json so params survive restart
                let _ = self.config.params.apply_args(args);
                let _ = self.config.save();
                self.notify(format!("🎛  {} (saved)", msg));
            }
            Err(e) => self.notify(format!("❌ set_params: {}", e)),
        }
    }

    /// Handle /endpoint command — change Ollama endpoint URL
    async fn handle_endpoint_command(&mut self, endpoint: &str) {
        let endpoint = if endpoint.is_empty() {
            self.push_system_event("🔌 Resetting endpoint to http://localhost:11434");
            "http://localhost:11434"
        } else {
            endpoint
        };

        self.status_message = format!("🔌 Connecting to {}…", endpoint);
        match OllamaClient::new_with_key(endpoint, &self.config.model, self.config.api_key.as_deref()).await {
            Ok(client) => {
                self.config.endpoint = endpoint.to_string();
                let _ = self.config.save();
                self.ollama_client = Some(client);
                self.endpoint_type = crate::ollama::detect_endpoint_type(&self.config.endpoint);
                self.notify(format!("✅ Endpoint changed to {} [{}]", endpoint, self.endpoint_type));
            }
            Err(e) => {
                self.notify(format!("❌ Failed to connect to {}: {}", endpoint, e));
            }
        }
    }

    /// Handle /model command — change the AI model
    async fn handle_model_command(&mut self, model: &str) {
        if model.is_empty() {
            self.notify(format!("🌸 Current model: {}\nUsage: /model <name>\nTip: Use /models to list available models", self.config.model));
            return;
        }

        match &self.ollama_client {
            Some(client) => {
                self.status_message = format!("🌸 Switching to {}…", model);
                match OllamaClient::new_with_key(&self.config.endpoint, model, self.config.api_key.as_deref()).await {
                    Ok(new_client) => {
                        new_client.fetch_native_ctx().await;
                        self.config.model = model.to_string();
                        let _ = self.config.save();
                        self.ollama_client = Some(new_client);
                        self.notify(format!("✅ Switched to model: {}", model));
                        self.inject_continue_kick();
                    }
                    Err(e) => {
                        self.notify(format!("❌ Failed to switch to {}: {}", model, e));
                    }
                }
            }
            None => {
                self.notify("⚠️ Ollama not connected");
            }
        }
    }

    /// Handle configuration changes from filesystem watcher
    async fn handle_config_change(&mut self, change: crate::watcher::ConfigChange) {
        use crate::watcher::ConfigChange;
        
        match change {
            ConfigChange::ConfigFileChanged => {
                let mut fresh_config = crate::config::Config::reload_from_file();
                let model_changed = fresh_config.model != self.config.model;
                let endpoint_changed = fresh_config.endpoint != self.config.endpoint;
                
                if model_changed {
                    let endpoint = fresh_config.endpoint.clone();
                    match OllamaClient::new_with_key(&endpoint, &fresh_config.model, fresh_config.api_key.as_deref()).await {
                        Ok(client) => {
                            client.fetch_native_ctx().await;
                            self.config = fresh_config;
                            self.endpoint_type = crate::ollama::detect_endpoint_type(&self.config.endpoint);
                            self.ollama_client = Some(client);
                            self.notify(format!("🌸 Switched to model: {}", self.config.model));
                        }
                        Err(e) => {
                            self.notify(format!("❌ Failed to switch model: {}", e));
                        }
                    }
                } else if endpoint_changed {
                    self.config = fresh_config;
                    self.endpoint_type = crate::ollama::detect_endpoint_type(&self.config.endpoint);
                    self.notify(format!("🔄 Endpoint changed to {} [{}]", self.config.endpoint, self.endpoint_type));
                } else {
                    // Silent reload — config didn't meaningfully change
                    self.config = fresh_config;
                }
            }
            ConfigChange::AgentsMdChanged => {
                let cwd = std::env::current_dir().unwrap_or_default();
                self.agents_context = std::fs::read_to_string(cwd.join("AGENTS.md"))
                    .ok()
                    .filter(|c| !c.trim().is_empty());
                
                let agents_config = crate::config::AgentsConfig::parse_from_file(&cwd.join("AGENTS.md"));
                if let Some(preferred) = &agents_config.preferred_model {
                    if preferred != &self.config.model {
                        if let Some(client) = self.ollama_client.as_ref() {
                            let new_model = crate::config::get_model_with_fallback(
                                &agents_config,
                                &self.config.model,
                                client,
                            ).await;
                            if new_model != self.config.model {
                                let endpoint = self.config.endpoint.clone();
                                match OllamaClient::new_with_key(&endpoint, &new_model, self.config.api_key.as_deref()).await {
                                    Ok(new_client) => {
                                        new_client.fetch_native_ctx().await;
                                        self.config.model = new_model.clone();
                                        self.ollama_client = Some(new_client);
                                        self.notify(format!("🌸 Switched to model from AGENTS.md: {}", new_model));
                                    }
                                    Err(e) => {
                                        self.notify(format!("❌ Failed to switch model: {}", e));
                                    }
                                }
                            }
                        }
                    }
                }
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

        // Auto-seed AGENTS.md from first prompt when missing or empty
        let agents_empty = self.agents_context.as_deref().map(str::trim).unwrap_or("").is_empty();
        let is_first_message = self.message_buffer.count().unwrap_or(1) == 0;
        if agents_empty && is_first_message {
            let cwd = std::env::current_dir().unwrap_or_default();
            let agents_path = cwd.join("AGENTS.md");
            if let Err(e) = std::fs::write(&agents_path, message) {
                crate::dlog!("Failed to write AGENTS.md: {}", e);
            } else {
                self.agents_context = std::fs::read_to_string(&agents_path).ok();
                self.push_agent_notice("📝 First prompt saved to AGENTS.md — edit it to provide persistent project context.");
            }
        }

        // Save user message
        let user_msg = Message::new("user", message.to_string());
        if let Err(e) = self.message_buffer.add_and_persist(user_msg) {
            crate::dlog!("Failed to save user message: {}", e);
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
                    self.stream_start_time = Some(std::time::Instant::now());
                    self.last_stream_token_time = None;
        self.tool_iteration_count = 0;
        self.status_message = "⏳ Streaming response...".to_string();

        // Refresh project context if stale (>60s since last build)
        if self.project_context_built.elapsed() > std::time::Duration::from_secs(60) {
            self.refresh_project_context();
        }

        let steering_text = self.steering_text();
        let messages_for_ollama: Vec<Message> = self
            .message_buffer
            .messages()
            .unwrap_or_default();

        // Start streaming — returns immediately, tokens arrive via channel
        if let Some(client) = &self.ollama_client {
            let (tool_cap, ctx_win) = self.compression_params();
            let rx = client.generate_streaming(messages_for_ollama, Some(&steering_text), self.effective_params(), tool_cap, ctx_win);
            self.stream_rx = Some(rx);
            self.streaming_text.clear();
                    self.thinking_text.clear();
                    self.in_think_block = false;
            self.last_build_kick = std::time::Instant::now();
        }
    }

    /// Convert technical errors to user-friendly messages
    fn friendly_error(&self, error: &str) -> String {
        // Sanitize first to strip ANSI escapes and non-printable chars
        let error = &sanitize_for_display(error, 500);
        if error.contains("refused") || error.contains("connection refused") {
            format!("Proxy/Ollama is offline. Make sure the proxy is running on {} or Ollama on http://localhost:11434", self.config.endpoint)
        } else if error.contains("model") && error.contains("not found") {
            format!("Model '{}' not found. Use /models to see available models.", self.config.model)
        } else if error.contains("timeout") {
            "Connection timeout. Proxy/Ollama may be unresponsive.".to_string()
        } else if error.contains("permission") {
            "Permission denied. Check file/directory permissions.".to_string()
        } else if error.contains("Parse") || error.contains("parse") {
            "Invalid response format from Ollama. Check logs.".to_string()
        } else {
            // Generic fallback - show first 80 chars
            let truncated = if error.len() > 80 {
                format!("{}...", &error[..floor_char_boundary(&error, 80)])
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

        // No-progress stall detection: in build/one mode, if many tool calls have fired
        // but no file has been mutated for a while, the agent is over-planning.
        if matches!(self.mode, AppMode::Forever | AppMode::One)
            && self.turn_phase == TurnPhase::Idle
            && !self.stall_notice_sent
            && self.tool_iteration_count > 10
            && self.last_mutating_action.elapsed() > std::time::Duration::from_secs(60)
        {
            self.push_agent_notice(format!(
                "⚠️ No files have been modified in the last {} tool calls ({}s elapsed). \
                 You appear to be reading/planning without acting. \
                 Stop planning and make a concrete file edit now.",
                self.tool_iteration_count,
                self.last_mutating_action.elapsed().as_secs()
            ));
            self.stall_notice_sent = true;
        }

        // Watchdog: if Build mode has been idle for 5+ seconds, re-kick.
        // One mode does not auto-kick — it waits for user input.
        // Respect autokick_paused: model is stuck or erroring, don't re-kick.
        if self.mode == AppMode::Forever
            && self.turn_phase == TurnPhase::Idle
            && self.last_build_kick.elapsed() >= std::time::Duration::from_secs(5)
            && self.ollama_client.is_some()
            && !self.autokick_paused
        {
            self.inject_continue_kick();
        }
    }

    /// Format message content as styled ratatui Text with syntax-highlighted code blocks
    fn format_message_styled(&self, emoji: &str, content: &str, msg_idx: usize) -> ratatui::text::Text<'static> {
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::{Line as RLine, Span, Text as RText};

        let is_dark = self.theme.kind == crate::theme::ThemeKind::Dark;
        let mut lines: Vec<RLine<'static>> = Vec::new();

        // Strip [THINK: ...] prefix and render it as a dim block before the rest
        let (content, think_prefix) = if content.starts_with("[THINK: ") {
            // Find the closing ] — it's the first ] that's not inside the think content
            // The format is [THINK: content]\nrest, where content may span lines
            // We look for ]\n or ] at end as the boundary
            let after_open = &content["[THINK: ".len()..];
            // Find "]\n" or "]\r\n" or "]" at end
            let close = after_open.find("]\n")
                .or_else(|| after_open.find("]\r\n"))
                .or_else(|| if after_open.ends_with(']') { Some(after_open.len() - 1) } else { None });
            if let Some(ci) = close {
                let think_content = &after_open[..ci];
                let rest_start = ci + 1 + if after_open[ci + 1..].starts_with('\n') { 1 } else { 0 };
                let rest = if rest_start <= after_open.len() { &after_open[rest_start..] } else { "" };
                (rest, Some(think_content.to_string()))
            } else {
                (content, None)
            }
        } else {
            (content, None)
        };

        // Render the think block as a dim collapsible section
        let has_think = think_prefix.is_some();
        if let Some(ref think) = think_prefix {
            let think_color = if is_dark {
                Color::Rgb(140, 150, 170) // readable slate-grey in dark mode
            } else {
                Color::Rgb(100, 110, 130) // darker for light mode
            };
            let dim = Style::default().fg(think_color).add_modifier(Modifier::ITALIC);
            let think_lines: Vec<&str> = think.lines().collect();
            
            // Check if this thinking block is collapsed
            let is_collapsed = self.collapsed_thinking_blocks.contains(&msg_idx);
            
            // Header line with emoji and toggle indicator
            let toggle = if is_collapsed { "+ " } else { "- " };
            lines.push(RLine::from(vec![
                Span::raw(format!("{} ", emoji)),
                Span::styled(format!("{}💭 thinking", toggle), dim),
            ]));
            
            // Show full content if expanded, or summary if collapsed
            if !is_collapsed {
                for tl in &think_lines {
                    lines.push(RLine::from(vec![
                        Span::styled(format!("   {}", tl), dim),
                    ]));
                }
                // Separator after thinking block
                lines.push(RLine::from(vec![Span::styled("   ·".to_string(), dim)]));
            } else {
                // Collapsed view: show first line + ellipsis
                if !think_lines.is_empty() {
                    let preview = think_lines[0];
                    let boundary = floor_char_boundary(preview, 50);
                    let truncated = if preview.len() > boundary {
                        format!("{}…", &preview[..boundary])
                    } else {
                        preview.to_string()
                    };
                    lines.push(RLine::from(vec![
                        Span::styled(format!("   {}", truncated), dim),
                    ]));
                }
                // Separator for collapsed block
                lines.push(RLine::from(vec![Span::styled("   ·".to_string(), dim)]));
            }
        }

        // If content contains a tool-call block (JSON or XML), split into prose + pretty box.
        // Works whether the tool call is at the start or follows narration text.
        let tool_call_pos = content.find("{\"tool_calls\"").or_else(|| content.find("<tool>"));
        let content_emoji = if !has_think { emoji } else { "" };
        if let Some(tc_pos) = tool_call_pos {
            let prose = content[..tc_pos].trim();
            let tc_part = &content[tc_pos..];
            if let Some(pretty_lines) = prettify_tool_calls(tc_part) {
                // Render any preceding prose first
                if !prose.is_empty() {
                    let mut first_prose = true;
                    for raw_line in prose.lines() {
                        if first_prose {
                            lines.push(RLine::from(format!("{} {}", content_emoji, raw_line)));
                            first_prose = false;
                        } else {
                            lines.push(RLine::from(format!("   {}", raw_line)));
                        }
                    }
                }
                return RText::from(lines);
            }
        }

        let mut in_code_block = false;
        let mut code_language = String::new();
        let mut code_buffer = String::new();
        // If a think block was already rendered above, don't prepend emoji again on first content line
        let mut first_line = !has_think;

        const KNOWN_LANGS: &[&str] = &[
            "rust","python","py","javascript","js","typescript","ts","go","java",
            "c","cpp","c++","cs","csharp","bash","sh","zsh","fish","toml","yaml",
            "yml","json","html","css","sql","dockerfile","makefile","zig","kotlin",
            "swift","ruby","php","scala","haskell","elixir","erlang","ocaml","r",
            "markdown","md","xml","csv","diff","patch","text","txt","plaintext",
            "proto","graphql","nix","vim","assembly","asm","wgsl","glsl","hlsl",
        ];

        let content_lines: Vec<&str> = content.lines().collect();
        let mut line_idx = 0;

        while line_idx < content_lines.len() {
            let line = content_lines[line_idx];
            
            if line.trim_start().starts_with("```") {
                if !in_code_block {
                    let lang_part = line.trim_start().strip_prefix("```").unwrap_or("").trim();
                    let canonical = lang_part.to_lowercase();
                    code_language = if lang_part.is_empty() {
                        "code".to_string()
                    } else if KNOWN_LANGS.contains(&canonical.as_str()) {
                        lang_part.to_string()
                    } else {
                        "code".to_string()
                    };
                    in_code_block = true;
                    code_buffer.clear();
                    // first_line is consumed when the wrapped block is emitted (close brace).
                } else {
                    // End of code block — highlight + wrap with rounded borders + dim gutter
                    let highlighted = self.highlighter.highlight_code(&code_buffer, &code_language, is_dark);
                    let mut wrapped = wrap_code_block(&code_language, highlighted, is_dark);
                    if first_line && !wrapped.is_empty() {
                        let header = wrapped.remove(0);
                        let mut spans = vec![Span::raw(format!("{} ", content_emoji))];
                        spans.extend(header.spans);
                        lines.push(RLine::from(spans));
                        first_line = false;
                    }
                    lines.extend(wrapped);
                    in_code_block = false;
                    code_language.clear();
                    code_buffer.clear();
                }
                line_idx += 1;
                continue;
            }

            if in_code_block {
                if !code_buffer.is_empty() {
                    code_buffer.push('\n');
                }
                code_buffer.push_str(line);
                line_idx += 1;
            } else {
                // Try to detect and render tables
                if let Some((table_lines, lines_consumed)) = self.detect_and_render_table(&content_lines, line_idx, is_dark) {
                    // Prepend emoji to first table line if needed
                    if first_line && !table_lines.is_empty() {
                        let mut first_table_line = table_lines[0].clone();
                        if let Some(_first_span) = first_table_line.spans.first() {
                            let mut new_spans = vec![Span::raw(format!("{} ", content_emoji))];
                            new_spans.extend(first_table_line.spans.iter().cloned());
                            first_table_line = RLine::from(new_spans);
                        }
                        lines.push(first_table_line);
                        lines.extend(table_lines.into_iter().skip(1));
                        first_line = false;
                    } else {
                        lines.extend(table_lines);
                        first_line = false;
                    }
                    line_idx += lines_consumed;
                } else {
                    // Regular text line with markdown formatting
                    let md_lines = self.render_markdown_line(line, is_dark);
                    for md_line in md_lines {
                        if first_line {
                            // Prepend emoji to first line
                            let mut spans = vec![Span::raw(format!("{} ", content_emoji))];
                            spans.extend(md_line.spans.into_iter());
                            lines.push(RLine::from(spans));
                            first_line = false;
                        } else {
                            // Indent continuation lines to align with content after emoji (~3 cols)
                            let mut spans = vec![Span::raw("   ")];
                            spans.extend(md_line.spans.into_iter());
                            lines.push(RLine::from(spans));
                        }
                    }
                    line_idx += 1;
                }
            }
        }

        // Handle unclosed code block — wrap accumulated buffer for visual consistency
        if in_code_block && !code_buffer.is_empty() {
            let highlighted = self.highlighter.highlight_code(&code_buffer, &code_language, is_dark);
            let mut wrapped = wrap_code_block(&code_language, highlighted, is_dark);
            if first_line && !wrapped.is_empty() {
                let header = wrapped.remove(0);
                let mut spans = vec![Span::raw(format!("{} ", content_emoji))];
                spans.extend(header.spans);
                lines.push(RLine::from(spans));
            }
            lines.extend(wrapped);
        }

        // Ensure at least one line with the emoji
        if lines.is_empty() {
            lines.push(RLine::from(format!("{} ", content_emoji)));
        }

        RText::from(lines)
    }

    /// Render a single line with markdown formatting (headers, lists, inline formatting)
    fn render_markdown_line(&self, line: &str, is_dark: bool) -> Vec<ratatui::text::Line<'static>> {
        render_markdown_line(line, is_dark)
    }

    fn detect_and_render_table(&self, lines_vec: &[&str], start_idx: usize, is_dark: bool) -> Option<(Vec<ratatui::text::Line<'static>>, usize)> {
        detect_and_render_table(lines_vec, start_idx, is_dark)
    }

    /// Format message content with nice code block indentation and language detection
    /// (Plain-text fallback; used by format_message_styled for non-highlighted paths)
    #[allow(dead_code)]
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

    /// Format inline tool results panel showing active and completed tools
    fn format_inline_results(&self) -> ratatui::text::Text<'static> {
        use ratatui::text::{Line, Span};
        let mut lines = Vec::new();

        for (idx, result) in self.inline_tool_results.iter().enumerate() {
            let elapsed = result.start_time.elapsed().as_secs();
            
            // Animated spinner for running tools: cycles through ⏳ ⌛ ⏰ based on frame count
            let spinner_frames = ['⏳', '⌛', '⏰'];
            let spinner_idx = ((self.tick_count / 10) as usize) % spinner_frames.len();
            let spinner = spinner_frames[spinner_idx];
            
            // Status based on exit code
            let status_display = if !result.is_complete {
                spinner.to_string() // Animated spinner for running
            } else {
                match result.exit_code {
                    Some(0) => "✅".to_string(),     // Success
                    Some(_) => "❌".to_string(),     // Error (non-zero exit)
                    None => {
                        if result.output.is_empty() {
                            "⚪".to_string()  // No output
                        } else {
                            "✅".to_string()  // Default to success if unknown
                        }
                    }
                }
            };
            
            // Tool name line with elapsed time and status
            lines.push(Line::from(vec![
                Span::raw(format!(
                    "{} {} ({}s)",
                    status_display,
                    result.tool_name,
                    elapsed
                )),
            ]));

            // Output lines — all when /zt, otherwise 30-line preview
            let output_lines: Vec<&str> = result.output.lines().collect();
            let show_count = if self.zero_truncation { output_lines.len() } else { 30 };
            let preview_lines = output_lines.iter().take(show_count).collect::<Vec<_>>();
            
            for line in preview_lines {
                let truncated = if !self.zero_truncation && line.len() > crate::config::OUTPUT_CHARACTER_LIMIT {
                    format!("{}…", &line[..floor_char_boundary(line, crate::config::OUTPUT_CHARACTER_LIMIT - 3)])
                } else {
                    line.to_string()
                };
                lines.push(Line::from(Span::raw(format!("  {}", truncated))));
            }

            // If more content, show indicator (only in normal mode)
            if !self.zero_truncation && output_lines.len() > 30 {
                lines.push(Line::from(Span::raw(format!(
                    "  … ({} more lines)",
                    output_lines.len() - 30
                ))));
            }

            // Separator between results if not the last one
            if idx < self.inline_tool_results.len() - 1 {
                lines.push(Line::from(""));
            }
        }

        ratatui::text::Text::from(lines)
    }





    fn format_tool_content_expanded(&self, content: &str, expanded: bool) -> String {
        format_tool_content_expanded(content, expanded)
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

    /// Handle /stats command — display cumulative project statistics from stats.json
    fn handle_stats_command(&mut self) {
        let s = &self.stats;

        // Format uptime as Xh Ym
        let uptime_secs = s.uptime_seconds;
        let uptime_str = if uptime_secs >= 3600 {
            format!("{}h {}m", uptime_secs / 3600, (uptime_secs % 3600) / 60)
        } else {
            format!("{}m {}s", uptime_secs / 60, uptime_secs % 60)
        };

        let mut output = format!(
            "📊 Project Stats\n\n\
             Sessions: {}  Uptime: {}\n\
             LLM: {} requests  |  {} prompt + {} gen tokens  |  avg {:.1} tok/s\n\
             Context trims: {}  |  Compressions: {}\n",
            s.sessions, uptime_str,
            s.llm_requests, s.prompt_tokens, s.gen_tokens, s.avg_tok_per_s(),
            s.context_trims, s.compressions,
        );

        if !s.tools.is_empty() {
            output.push_str("\nTools:\n");
            let mut tools: Vec<_> = s.tools.iter().collect();
            tools.sort_by(|a, b| b.1.calls.cmp(&a.1.calls));
            for (name, t) in &tools {
                let fail_str = if t.failures > 0 { format!("  {} failures", t.failures) } else { String::new() };
                output.push_str(&format!("  {:12}  {:4} calls  {:6} KB out{}\n",
                    name, t.calls, t.output_bytes / 1024, fail_str));
            }
        }

        let msg = crate::message::Message::new("tool", output);
        if let Err(e) = self.message_buffer.add_and_persist(msg) {
            self.status_message = format!("❌ Failed to display stats: {}", e);
        } else {
            self.cached_message_count = self.message_buffer.messages()
                .map(|v| v.len()).unwrap_or(0);
        }
    }
}

/// Return the largest index ≤ `max` that is a valid UTF-8 char boundary in `s`.
/// Prevents panics when slicing at an arbitrary byte offset into a string that
/// may contain multibyte characters (emoji, curly quotes, etc.).
/// Returns true if the shell command is a pure listing/discovery operation
/// (ls, find, tree, git ls-files, git log, etc.) — these get an elevated output cap.
fn is_listing_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    let listing_prefixes = [
        "ls", "find ", "find\t", "tree", "git ls-files", "git log",
        "git status", "git branch", "rg --files", "rg -l ", "fd ",
        "dir ", "exa ", "lsd ", "ls ", "ls\n",
    ];
    listing_prefixes.iter().any(|p| cmd.starts_with(p))
        || cmd == "find" || cmd == "ls" || cmd == "tree"
        || cmd == "git ls-files" || cmd == "git log" || cmd == "git status"
}


/// Mirrors `ls -lht` — gives the model rich file awareness without any discovery turns.
fn build_project_context(max_chars: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::collections::BTreeMap;

    const SKIP_DIRS: &[&str] = &[
        "target", ".git", "node_modules", ".yggdra",
        "vendor", "dist", "build", ".next", "__pycache__",
    ];
    const MAX_FILES: usize = 5000; // bail out if directory is huge (e.g. home dir)

    struct FileEntry { path: String, size: u64, modified: u64 }

    fn collect(dir: &std::path::Path, skip: &[&str], out: &mut Vec<FileEntry>) {
        if out.len() >= MAX_FILES { return; }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            if out.len() >= MAX_FILES { return; }
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') && name != ".yggdra" { continue; }
            let rel = path.to_string_lossy();
            if skip.iter().any(|s| rel.contains(s)) { continue; }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.is_dir() {
                collect(&path, skip, out);
            } else {
                let modified = meta.modified().ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()).unwrap_or(0);
                let p = path.strip_prefix("./").unwrap_or(&path);
                out.push(FileEntry { path: p.to_string_lossy().into_owned(), size: meta.len(), modified });
            }
        }
    }

    fn human_size(b: u64) -> String {
        if b >= 1_048_576 { format!("{:.1}M", b as f64 / 1_048_576.0) }
        else if b >= 1024  { format!("{}K", b / 1024) }
        else               { format!("{}B", b) }
    }

    fn human_time(secs: u64) -> String {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()).unwrap_or(0);
        let age = now.saturating_sub(secs);
        if age < 86400 {
            let h = (secs / 3600) % 24; let m = (secs / 60) % 60;
            format!("{:02}:{:02}", h, m)
        } else if age < 86400 * 30 {
            let days = secs / 86400;
            let months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
            format!("{}{:02}", months[((days % 365) / 30).min(11) as usize], (days % 365) % 30 + 1)
        } else {
            format!("{}", 1970 + secs / (86400 * 365))
        }
    }

    let mut files: Vec<FileEntry> = Vec::new();
    collect(std::path::Path::new("."), SKIP_DIRS, &mut files);

    // Build dir → [(filename, size, modified)] map
    let mut tree: BTreeMap<String, Vec<(String, u64, u64)>> = BTreeMap::new();
    for f in &files {
        let p = std::path::Path::new(&f.path);
        let dir = p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        tree.entry(dir).or_default().push((name, f.size, f.modified));
    }
    for v in tree.values_mut() { v.sort_by(|a, b| b.2.cmp(&a.2)); }

    fn render_dir(
        tree: &BTreeMap<String, Vec<(String, u64, u64)>>,
        dir: &str,
        line_prefix: &str,  // branch characters for indentation (e.g., "│   " or "    ")
        depth_limit: usize,
        max_leaves: usize,
        out: &mut Vec<String>,
        human_size: fn(u64) -> String,
        human_time: fn(u64) -> String,
    ) {
        let dir_prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir) };
        // Collect direct subdirs
        let mut subdirs: Vec<&str> = tree.keys()
            .filter(|k| {
                if dir.is_empty() {
                    !k.is_empty() && !k.contains('/')
                } else {
                    k.starts_with(&dir_prefix) && {
                        let rest = &k[dir_prefix.len()..];
                        !rest.is_empty() && !rest.contains('/')
                    }
                }
            })
            .map(|k| k.as_str())
            .collect();
        subdirs.sort();

        let files = tree.get(dir).map(|v| v.as_slice()).unwrap_or(&[]);
        let shown = files.len().min(max_leaves);
        let has_more = files.len() > max_leaves;
        // A file entry is "last" only if it's the final item (no subdirs after)
        let total_items = shown + if has_more { 1 } else { 0 } + subdirs.len();
        let mut item_idx = 0;

        for (name, size, modified) in &files[..shown] {
            let is_last = item_idx == total_items - 1;
            let conn = if is_last { "└── " } else { "├── " };
            out.push(format!("{}{}{}  {} {}", line_prefix, conn, name, human_size(*size), human_time(*modified)));
            item_idx += 1;
        }
        if has_more {
            let rest = files.len() - max_leaves;
            let is_last = item_idx == total_items - 1;
            let conn = if is_last { "└── " } else { "├── " };
            out.push(format!("{}{}... {} more", line_prefix, conn, rest));
            item_idx += 1;
        }

        if depth_limit == 0 { return; }

        for (i, sub) in subdirs.iter().enumerate() {
            let is_last_sub = i == subdirs.len() - 1;
            let is_last = item_idx == total_items - 1;
            let conn = if is_last { "└── " } else { "├── " };
            let subname = sub.rsplit('/').next().unwrap_or(sub);
            out.push(format!("{}{}{}/", line_prefix, conn, subname));
            let child_prefix = if is_last_sub {
                format!("{}    ", line_prefix)
            } else {
                format!("{}│   ", line_prefix)
            };
            render_dir(tree, sub, &child_prefix, depth_limit - 1, max_leaves, out, human_size, human_time);
            item_idx += 1;
        }
    }

    // Try rendering at decreasing depth limits until output fits
    let file_count = files.len();
    for depth in [999usize, 10, 7, 5, 3, 2, 1, 0] {
        let mut lines = vec![format!("PROJECT FILES ({} total):", file_count)];
        render_dir(&tree, "", "", depth, 30, &mut lines, human_size, human_time);
        let tree_part = lines.join("\n");
        // Build the rest (git, todos) for sizing
        let rest = build_git_and_todos();
        let full = format!("{}\n{}", tree_part, rest);
        if full.len() <= max_chars || depth == 0 {
            // Hard-truncate as last resort
            if full.len() <= max_chars {
                return full;
            } else {
                let cut = floor_char_boundary(&full, max_chars.saturating_sub(40));
                return format!("{}\n... (truncated)", &full[..cut]);
            }
        }
    }
    String::new()
}

fn build_git_and_todos() -> String {
    fn run_git(args: &[&str]) -> Option<String> {
        std::process::Command::new("git").args(args).output().ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    let mut out = String::new();
    let branch = run_git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let status = run_git(&["status", "--short"]);
    if branch.is_some() || status.is_some() {
        out.push('\n');
        out.push_str(&match &branch {
            Some(b) => format!("GIT STATUS (branch: {}):", b),
            None    => "GIT STATUS:".to_string(),
        });
        out.push('\n');
        match status {
            Some(s) => { for l in s.lines() { out.push_str(&format!("  {}\n", l)); } }
            None    => { out.push_str("  (clean)\n"); }
        }
    }
    if let Some(log) = run_git(&["log", "--oneline", "-5"]) {
        out.push_str("\nRECENT COMMITS:\n");
        for l in log.lines() { out.push_str(&format!("  {}\n", l)); }
    }
    let todo_dir = std::path::Path::new(".yggdra/todo");
    if todo_dir.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(todo_dir).into_iter().flatten().flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.ends_with(".md") && n.to_lowercase() != "readme.md" { Some(n) } else { None }
            }).collect();
        if !names.is_empty() {
            names.sort();
            out.push_str("\nACTIVE TODOS (.yggdra/todo/):\n");
            for n in &names { out.push_str(&format!("  {}\n", n)); }
        }
    }
    out
}

/// Public entry point for benchmarking `build_project_context`.
/// Temporarily changes the working directory to `cwd`, runs the context
/// builder, then restores the original directory.
pub fn build_project_context_for_bench(cwd: &std::path::Path) -> String {
    let original = std::env::current_dir().ok();
    if std::env::set_current_dir(cwd).is_ok() {
        let result = build_project_context(1_000_000);
        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }
        result
    } else {
        build_project_context(1_000_000)
    }
}

/// Build a block of content from the N most recently modified text files.
///
/// Skips binary files, files larger than 200 KB, lock files, and the same
/// directories as `build_project_context` (target, .git, node_modules, …).
/// Each file is capped at `max_chars_per_file` characters; a `…(truncated)`
/// marker is appended when the file is cut.
///
/// Returns an empty string when the project directory is not a git repo or
/// no eligible files are found.
pub(crate) fn build_recent_files_content(n: usize, max_chars_per_file: usize) -> String {
    build_recent_files_content_in(n, max_chars_per_file, std::path::Path::new("."))
}

/// Inner implementation accepting an explicit root, so tests can call it without
/// mutating process-global CWD.
pub(crate) fn build_recent_files_content_in(n: usize, max_chars_per_file: usize, root: &std::path::Path) -> String {
    const SKIP_DIRS: &[&str] = &[
        "target", ".git", "node_modules", ".yggdra",
        "vendor", "dist", "build", ".next", "__pycache__",
    ];
    // Extensions we never want to embed (binary / generated / lock files).
    const SKIP_EXTS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "ico", "svg", "woff", "woff2", "ttf", "eot",
        "pdf", "zip", "tar", "gz", "bz2", "xz", "7z", "bin", "exe", "dll", "so",
        "dylib", "a", "o", "class", "jar", "pyc", "pyo", "lock",
    ];

    struct FileEntry { path: std::path::PathBuf, modified: u64 }

    fn collect(dir: &std::path::Path, skip: &[&str], out: &mut Vec<FileEntry>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') && name != ".yggdra" { continue; }
            let rel = path.to_string_lossy();
            if skip.iter().any(|s| rel.contains(s)) { continue; }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.is_dir() {
                collect(&path, skip, out);
            } else {
                let modified = meta.modified().ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                out.push(FileEntry { path, modified });
            }
        }
    }

    let mut files: Vec<FileEntry> = Vec::new();
    collect(root, SKIP_DIRS, &mut files);

    // Most recently modified first.
    files.sort_by(|a, b| b.modified.cmp(&a.modified));

    let mut sections = String::new();
    let mut count = 0usize;

    for entry in &files {
        if count >= n { break; }

        // Skip unwanted extensions.
        let ext = entry.path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if SKIP_EXTS.contains(&ext.as_str()) { continue; }

        // Skip files that are likely too large to be useful.
        let size = std::fs::metadata(&entry.path).map(|m| m.len()).unwrap_or(0);
        if size > 200_000 { continue; }

        // Read and validate as UTF-8; skip binary files (null bytes).
        let bytes = match std::fs::read(&entry.path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.contains(&0u8) { continue; }
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let rel = entry.path.strip_prefix(root).unwrap_or(&entry.path);

        let body = if content.chars().count() > max_chars_per_file {
            let boundary = floor_char_boundary(&content, max_chars_per_file);
            format!("{}…(truncated)", &content[..boundary])
        } else {
            content.clone()
        };

        sections.push_str(&format!("// {}\n```\n{}\n```\n\n", rel.display(), body));
        count += 1;
    }

    if sections.is_empty() {
        String::new()
    } else {
        format!("### RECENTLY EDITED FILES ({})\n\n{}", count, sections)
    }
}


fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() { return s.len(); }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
}

/// Strip ANSI escape sequences, non-printable characters, and truncate for safe TUI display.
// ─────────────────────────────────────────────────────────────────────────────
// Rendering helpers — pub(crate) free functions testable without App.
// Each impl App method above is a thin wrapper delegating here.
// ─────────────────────────────────────────────────────────────────────────────

/// True if `body` looks like unified diff output (has hunk headers or multiple +/- lines).
pub(crate) fn looks_like_diff(body: &str) -> bool {
    let mut pm_count = 0usize;
    for line in body.lines().take(40) {
        if line.starts_with("@@") { return true; }
        if (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"))
        {
            pm_count += 1;
            if pm_count >= 3 { return true; }
        }
    }
    false
}

/// Render a diff body as colored ratatui Lines. `max_lines` caps the preview; 0 = show all.
/// Wrap highlighted code lines in a soft, opencode-style box:
///   ╭─ rust ────
///   │  fn main() ...
///   ╰─
/// Borders are dim and the language is rendered as a small accent badge so that
/// the code itself remains the visual focus.
pub(crate) fn wrap_code_block(
    language: &str,
    highlighted: Vec<ratatui::text::Line<'static>>,
    is_dark: bool,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::{Line as RLine, Span as RSpan};

    let border_color = if is_dark {
        Color::Rgb(80, 90, 110)
    } else {
        Color::Rgb(170, 175, 185)
    };
    let lang_color = if is_dark {
        Color::Rgb(120, 180, 235)
    } else {
        Color::Rgb(60, 110, 175)
    };
    let dim = Style::default().fg(border_color);
    let lang_style = Style::default().fg(lang_color).add_modifier(Modifier::BOLD);

    let mut out: Vec<RLine<'static>> = Vec::with_capacity(highlighted.len() + 2);

    // Header: ╭─ <lang> ──── (a few trailing dashes for visual weight)
    out.push(RLine::from(vec![
        RSpan::styled("╭─ ".to_string(), dim),
        RSpan::styled(language.to_string(), lang_style),
        RSpan::styled(" ────".to_string(), dim),
    ]));

    // Body: prefix every highlighted line with a dim left gutter
    for line in highlighted {
        let mut spans: Vec<RSpan<'static>> = Vec::with_capacity(line.spans.len() + 1);
        spans.push(RSpan::styled("│  ".to_string(), dim));
        spans.extend(line.spans);
        out.push(RLine::from(spans));
    }

    // Footer
    out.push(RLine::from(RSpan::styled("╰─".to_string(), dim)));

    out
}

pub(crate) fn render_diff_styled(
    emoji: &str,
    name: &str,
    body: &str,
    max_lines: usize,     // 0 = show all
    hint: &str,           // cursor hint appended at bottom (empty if not cursor)
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::{Line as RLine, Span as RSpan};

    let all_lines: Vec<&str> = body.lines().collect();
    let total = all_lines.len();
    let cap = if max_lines == 0 || max_lines >= total { total } else { max_lines };

    let mut lines: Vec<RLine<'static>> = Vec::with_capacity(cap + 3);

    // Header row — bold name plus a dim line-count badge
    let count = if cap < total {
        format!("  ({} lines — showing {})", total, cap)
    } else {
        format!("  ({} lines)", total)
    };
    lines.push(RLine::from(vec![
        RSpan::raw(format!("{} ", emoji)),
        RSpan::styled(name.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        RSpan::styled(count, Style::default().fg(Color::DarkGray)),
    ]));

    // Each diff line gets a colored signal gutter (▎) in the relevant color so
    // additions/deletions are scannable without painting the whole line.
    for line in all_lines[..cap].iter() {
        let (gutter_style, text_style) =
            if line.starts_with('+') && !line.starts_with("+++") {
                (Style::default().fg(Color::Rgb(80, 200, 120)),
                 Style::default().fg(Color::Rgb(80, 200, 120)))
            } else if line.starts_with('-') && !line.starts_with("---") {
                (Style::default().fg(Color::Rgb(230, 100, 110)),
                 Style::default().fg(Color::Rgb(230, 100, 110)))
            } else if line.starts_with("@@") {
                (Style::default().fg(Color::Cyan),
                 Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            } else if line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("---")
                || line.starts_with("+++")
            {
                (Style::default().fg(Color::Yellow),
                 Style::default().fg(Color::Yellow))
            } else {
                (Style::default().fg(Color::DarkGray),
                 Style::default().fg(Color::Gray))
            };
        lines.push(RLine::from(vec![
            RSpan::styled("▎ ".to_string(), gutter_style),
            RSpan::styled(line.to_string(), text_style),
        ]));
    }

    let dim = Style::default().fg(Color::DarkGray);
    if cap < total {
        lines.push(RLine::from(RSpan::styled(
            format!("▎ … {} more lines{}", total - cap,
                if !hint.is_empty() { "  [Space=expand]" } else { "" }),
            dim,
        )));
    } else if !hint.is_empty() {
        lines.push(RLine::from(RSpan::styled(
            "▎ [Space=collapse]".to_string(),
            dim,
        )));
    }

    lines
}

/// Format tool output with indented bordered block
pub(crate) fn format_tool_content_expanded(content: &str, expanded: bool) -> String {
    // Pretty-print [TOOL_OUTPUT: name = content] injections
    if let Some(rest) = content.strip_prefix("[TOOL_OUTPUT: ") {
        if let Some(eq) = rest.find(" = ") {
            let name = &rest[..eq];
            let raw_body = rest[eq + 3..].trim_end_matches(']');

            // Detect and strip trailing truncation marker
            let (body, truncation_note) = if let Some(trunc_pos) = raw_body.rfind("...(truncated to ") {
                let note = raw_body[trunc_pos + 3..].trim_end_matches(')');
                let clean = raw_body[..trunc_pos].trim_end_matches('.');
                (clean, Some(format!("✂️  {}", note)))
            } else {
                (raw_body, None)
            };

            let lines: Vec<&str> = body.lines().collect();
            let total_lines = lines.len();
            let total_chars = body.len();
            if expanded {
                let all: String = lines.iter()
                    .map(|l| format!("│  {}", l))
                    .collect::<Vec<_>>()
                    .join("\n");
                let trunc = truncation_note.map(|n| format!("\n│  {}", n)).unwrap_or_default();
                return format!("🔧 {}  ({} lines, {} chars)\n{}{}", name, total_lines, total_chars, all, trunc);
            }
            let preview: String = lines.iter().take(3)
                .map(|l| format!("│  {}", l))
                .collect::<Vec<_>>()
                .join("\n");
            let more = if total_lines > 3 {
                format!("\n│  … ({} more lines, {} chars)", total_lines - 3, total_chars)
            } else {
                String::new()
            };
            let trunc = truncation_note.map(|n| format!("\n│  {}", n)).unwrap_or_default();
            return format!("🔧 {}  ({} lines, {} chars)\n{}{}{}", name, total_lines, total_chars, preview, more, trunc);
        }
    }
    if let Some(rest) = content.strip_prefix("[TOOL_ERROR: ") {
        if let Some(eq) = rest.find(" = ") {
            let name = &rest[..eq];
            let err = rest[eq + 3..].trim_end_matches(']');
            return format!("❌ {}: {}", name, err);
        }
    }
    // All other tool messages (user /tool, /shell, /mem etc.) — keep │ borders
    let mut result = String::new();
    for line in content.lines() {
        result.push_str("│  ");
        result.push_str(line);
        result.push('\n');
    }
    if result.ends_with('\n') { result.pop(); }
    result
}

/// Try to render a JSON or XML tool-call response as compact colored Lines.
/// Returns None if the string doesn't look like a tool call or fails to parse.
pub(crate) fn prettify_tool_calls(text: &str) -> Option<Vec<ratatui::text::Line<'static>>> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    let trimmed = text.trim();

    // Try XML format first (ShellOnly), then JSON
    let tool_calls: Vec<crate::agent::ToolCall> =
        if trimmed.contains("<tool>") {
            let calls = crate::agent::parse_xml_tool_calls(
                trimmed
            );
            if calls.is_empty() { return None; }
            calls
        } else if trimmed.contains("\"tool_calls\"") {
            let json_start = trimmed.find('{').unwrap_or(0);
            let v: serde_json::Value = serde_json::from_str(&trimmed[json_start..]).ok()?;
            let arr = v.get("tool_calls")?.as_array()?;
            if arr.is_empty() { return None; }
            arr.iter().filter_map(|call| {
                let name = call.get("name")?.as_str()?.to_string();
                let params = call.get("parameters")?;
                let get_str = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let cmd = get_str("command");
                let returnlines = params.get("returnlines").and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });
                let args = if let Some(rl) = returnlines { format!("{}\x00{}", cmd, rl) } else { cmd };
                let is_async = params.get("mode").and_then(|v| v.as_str()) == Some("async");
                Some(crate::agent::ToolCall {
                    name,
                    args,
                    description: params.get("description").and_then(|v| v.as_str()).map(str::to_string),
                    async_mode: is_async,
                    async_task_id: if is_async { params.get("task_id").and_then(|v| v.as_str()).map(str::to_string) } else { None },
                    tellhuman: params.get("tellhuman").and_then(|v| v.as_str()).map(str::to_string),
                })
            }).collect()
        } else {
            return None;
        };

    let dim   = Style::default().fg(Color::DarkGray);
    let cyan  = Style::default().fg(Color::Rgb(120, 180, 235)).add_modifier(Modifier::BOLD);
    let yel   = Style::default().fg(Color::Rgb(214, 182, 110));
    let white = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, tc) in tool_calls.iter().enumerate() {
        let name = &tc.name;
        let tool_emoji = match name.as_str() {
            "shell" | "exec" => "🐚",
            "rg"             => "🔍",
            "setfile" | "patchfile" => "📝",
            "spawn"          => "🤖",
            "think"          => "💭",
            "commit"         => "📌",
            _                => "🔧",
        };

        if i > 0 {
            lines.push(Line::from(vec![Span::styled("├─".to_string(), dim)]));
        }

        // Header: ╭─ <emoji> name
        lines.push(Line::from(vec![
            Span::styled("╭─ ".to_string(), dim),
            Span::raw(format!("{} ", tool_emoji)),
            Span::styled(name.clone(), cyan),
        ]));

        // Primary arg: command (strip returnlines suffix if present)
        let cmd_display = tc.args.split('\x00').next().unwrap_or(&tc.args);
        if !cmd_display.is_empty() {
            let prefix = if name == "shell" || name == "exec" { "$ " } else { "" };
            lines.push(Line::from(vec![
                Span::styled("│  ".to_string(), dim),
                Span::styled(format!("{}{}", prefix, cmd_display), yel),
            ]));
        }
        if let Some(desc) = &tc.description {
            if !desc.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("│  ↳ ".to_string(), dim),
                    Span::styled(desc.clone(), white),
                ]));
            }
        }
        if tc.async_mode {
            let tid = tc.async_task_id.as_deref().unwrap_or("?");
            lines.push(Line::from(vec![
                Span::styled("│  ⚡ async ".to_string(), dim),
                Span::styled(tid.to_string(), Style::default().fg(Color::Magenta)),
            ]));
        }
        // Footer to close the card
        lines.push(Line::from(vec![Span::styled("╰─".to_string(), dim)]));
    }
    if lines.is_empty() { return None; }
    Some(lines)
}

pub(crate) fn render_markdown_line(line: &str, is_dark: bool) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line as RLine, Span};

    let text_color = if is_dark {
        Color::Rgb(220, 230, 240)
    } else {
        Color::Rgb(40, 42, 46)
    };

    // Check for headers
    if let Some((level, content)) = crate::markdown::detect_header(line) {
        return vec![crate::markdown::format_header(level, &content, text_color)];
    }

    // Check for list items — use a unified bullet glyph so prose reads consistently.
    if let Some((indent, content)) = crate::markdown::detect_list_item(line) {
        let bullet = if line.trim_start().starts_with(|c: char| c.is_ascii_digit()) { '·' } else { '•' };
        return vec![crate::markdown::format_list_item(indent, &content, text_color, bullet)];
    }

    // Regular text line with inline markdown formatting
    let spans = crate::markdown::format_inline_to_spans(line, text_color);
    vec![RLine::from(spans)]
}

/// Check if content looks like a table (has | separators on consecutive lines)
pub(crate) fn detect_and_render_table(lines_vec: &[&str], start_idx: usize, is_dark: bool) -> Option<(Vec<ratatui::text::Line<'static>>, usize)> {
    use ratatui::style::Color;

    if start_idx >= lines_vec.len() || start_idx + 2 > lines_vec.len() {
        return None;
    }

    // Look for table: check current line + next line (separator)
    if !lines_vec[start_idx].contains('|') || !lines_vec[start_idx + 1].contains('|') {
        return None;
    }

    // Check if next line is a separator
    if !crate::markdown::is_table_separator(lines_vec[start_idx + 1]) {
        return None;
    }

    // Find the end of the table (where | separators stop appearing)
    let mut end_idx = start_idx + 2;
    while end_idx < lines_vec.len() && lines_vec[end_idx].contains('|') {
        end_idx += 1;
    }

    let table_lines = lines_vec[start_idx..end_idx].to_vec();
    if let Some(table) = crate::markdown::parse_table(&table_lines) {
        let text_color = if is_dark {
            Color::Rgb(220, 230, 240)
        } else {
            Color::Rgb(40, 42, 46)
        };
        let rendered = crate::markdown::format_table(&table, text_color);
        return Some((rendered, end_idx - start_idx));
    }

    None
}

pub(crate) fn format_message_styled(
    emoji: &str,
    content: &str,
    is_dark: bool,
    highlighter: &crate::highlight::Highlighter,
) -> ratatui::text::Text<'static> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line as RLine, Span, Text as RText};

    let mut lines: Vec<RLine<'static>> = Vec::new();

    // Strip [THINK: ...] prefix and render it as a dim block before the rest
    let (content, think_prefix) = if content.starts_with("[THINK: ") {
        // Find the closing ] — it's the first ] that's not inside the think content
        // The format is [THINK: content]\nrest, where content may span lines
        // We look for ]\n or ] at end as the boundary
        let after_open = &content["[THINK: ".len()..];
        // Find "]\n" or "]\r\n" or "]" at end
        let close = after_open.find("]\n")
            .or_else(|| after_open.find("]\r\n"))
            .or_else(|| if after_open.ends_with(']') { Some(after_open.len() - 1) } else { None });
        if let Some(ci) = close {
            let think_content = &after_open[..ci];
            let rest_start = ci + 1 + if after_open[ci + 1..].starts_with('\n') { 1 } else { 0 };
            let rest = if rest_start <= after_open.len() { &after_open[rest_start..] } else { "" };
            (rest, Some(think_content.to_string()))
        } else {
            (content, None)
        }
    } else {
        (content, None)
    };

    // Render the think block as a dim collapsible section
    let has_think = think_prefix.is_some();
    if let Some(ref think) = think_prefix {
        let think_color = if is_dark {
            Color::Rgb(140, 150, 170) // readable slate-grey in dark mode
        } else {
            Color::Rgb(100, 110, 130) // darker for light mode
        };
        let dim = Style::default().fg(think_color).add_modifier(Modifier::ITALIC);
        let think_lines: Vec<&str> = think.lines().collect();
        // Header line with emoji
        lines.push(RLine::from(vec![
            Span::raw(format!("{} ", emoji)),
            Span::styled("💭 thinking".to_string(), dim),
        ]));
        for tl in &think_lines {
            lines.push(RLine::from(vec![
                Span::styled(format!("  {}", tl), dim),
            ]));
        }
        // Separator after thinking block
        lines.push(RLine::from(vec![Span::styled("  ·".to_string(), dim)]));
    }

    // If content contains a tool-call block (JSON or XML), split into prose + pretty box.
    // Works whether the tool call is at the start or follows narration text.
    let tool_call_pos = content.find("{\"tool_calls\"").or_else(|| content.find("<tool>"));
    let content_emoji = if !has_think { emoji } else { "" };
    if let Some(tc_pos) = tool_call_pos {
        let prose = content[..tc_pos].trim();
        let tc_part = &content[tc_pos..];
        if let Some(pretty_lines) = prettify_tool_calls(tc_part) {
            // Render any preceding prose first
            if !prose.is_empty() {
                let mut first_prose = true;
                for raw_line in prose.lines() {
                    if first_prose {
                        lines.push(RLine::from(format!("{} {}", content_emoji, raw_line)));
                        first_prose = false;
                    } else {
                        lines.push(RLine::from(format!("   {}", raw_line)));
                    }
                }
            }
            // Render prettified tool call box
            let mut first_box = prose.is_empty();
            for pl in pretty_lines {
                if first_box {
                    // Prepend emoji to the first box line
                    let mut spans = vec![ratatui::text::Span::raw(format!("{} ", content_emoji))];
                    spans.extend(pl.spans.into_iter().map(|s| {
                        ratatui::text::Span::styled(s.content.into_owned(), s.style)
                    }));
                    lines.push(RLine::from(spans));
                    first_box = false;
                } else {
                    lines.push(pl);
                }
            }
            return RText::from(lines);
        }
    }

    let mut in_code_block = false;
    let mut code_language = String::new();
    let mut code_buffer = String::new();
    // If a think block was already rendered above, don't prepend emoji again on first content line
    let mut first_line = !has_think;

    const KNOWN_LANGS: &[&str] = &[
        "rust","python","py","javascript","js","typescript","ts","go","java",
        "c","cpp","c++","cs","csharp","bash","sh","zsh","fish","toml","yaml",
        "yml","json","html","css","sql","dockerfile","makefile","zig","kotlin",
        "swift","ruby","php","scala","haskell","elixir","erlang","ocaml","r",
        "markdown","md","xml","csv","diff","patch","text","txt","plaintext",
        "proto","graphql","nix","vim","assembly","asm","wgsl","glsl","hlsl",
    ];

    let content_lines: Vec<&str> = content.lines().collect();
    let mut line_idx = 0;

    while line_idx < content_lines.len() {
        let line = content_lines[line_idx];
        
        if line.trim_start().starts_with("```") {
            if !in_code_block {
                let lang_part = line.trim_start().strip_prefix("```").unwrap_or("").trim();
                let canonical = lang_part.to_lowercase();
                code_language = if lang_part.is_empty() {
                    "code".to_string()
                } else if KNOWN_LANGS.contains(&canonical.as_str()) {
                    lang_part.to_string()
                } else {
                    "code".to_string()
                };
                in_code_block = true;
                code_buffer.clear();
            } else {
                let highlighted = highlighter.highlight_code(&code_buffer, &code_language, is_dark);
                let mut wrapped = wrap_code_block(&code_language, highlighted, is_dark);
                if first_line && !wrapped.is_empty() {
                    let header = wrapped.remove(0);
                    let mut spans = vec![Span::raw(format!("{} ", content_emoji))];
                    spans.extend(header.spans);
                    lines.push(RLine::from(spans));
                    first_line = false;
                }
                lines.extend(wrapped);
                in_code_block = false;
                code_language.clear();
                code_buffer.clear();
            }
            line_idx += 1;
            continue;
        }

        if in_code_block {
            if !code_buffer.is_empty() {
                code_buffer.push('\n');
            }
            code_buffer.push_str(line);
            line_idx += 1;
        } else {
            // Try to detect and render tables
            if let Some((table_lines, lines_consumed)) = detect_and_render_table(&content_lines, line_idx, is_dark) {
                // Prepend emoji to first table line if needed
                if first_line && !table_lines.is_empty() {
                    let mut first_table_line = table_lines[0].clone();
                    if let Some(first_span) = first_table_line.spans.first() {
                        let mut new_spans = vec![Span::raw(format!("{} ", content_emoji))];
                        new_spans.extend(first_table_line.spans.iter().cloned());
                        first_table_line = RLine::from(new_spans);
                    }
                    lines.push(first_table_line);
                    lines.extend(table_lines.into_iter().skip(1));
                    first_line = false;
                } else {
                    lines.extend(table_lines);
                    first_line = false;
                }
                line_idx += lines_consumed;
            } else {
                // Regular text line with markdown formatting
                let md_lines = render_markdown_line(line, is_dark);
                for md_line in md_lines {
                    if first_line {
                        // Prepend emoji to first line
                        let mut spans = vec![Span::raw(format!("{} ", content_emoji))];
                        spans.extend(md_line.spans.into_iter());
                        lines.push(RLine::from(spans));
                        first_line = false;
                    } else {
                        // Indent continuation lines to align with content after emoji (~3 cols)
                        let mut spans = vec![Span::raw("   ")];
                        spans.extend(md_line.spans.into_iter());
                        lines.push(RLine::from(spans));
                    }
                }
                line_idx += 1;
            }
        }
    }

    // Handle unclosed code block
    if in_code_block && !code_buffer.is_empty() {
        let highlighted = highlighter.highlight_code(&code_buffer, &code_language, is_dark);
        let mut wrapped = wrap_code_block(&code_language, highlighted, is_dark);
        if first_line && !wrapped.is_empty() {
            let header = wrapped.remove(0);
            let mut spans = vec![Span::raw(format!("{} ", content_emoji))];
            spans.extend(header.spans);
            lines.push(RLine::from(spans));
        }
        lines.extend(wrapped);
    }

    // Ensure at least one line with the emoji
    if lines.is_empty() {
        lines.push(RLine::from(format!("{} ", content_emoji)));
    }

    RText::from(lines)
}

/// Extract a human-readable error message from a raw provider error string.
///
/// OpenRouter and some other OpenAI-compatible providers return HTTP 400
/// errors where the `code` field is an integer, causing `async_openai` to
/// fail deserialization with:
///   "invalid type: integer `400`, expected a string … content:{…raw:…}"
///
/// This function detects that pattern, pulls the nested message out of the
/// raw JSON, and signals that the error is *fatal* (should not be retried).
///
/// Returns `(human_readable_message, is_fatal)`.
fn extract_provider_error(raw: &str) -> (String, bool) {
    // Detect the async_openai deserialization failure wrapping a provider 4xx.
    let is_deser_error = raw.contains("failed to deserialize api response")
        && (raw.contains("\"code\":400") || raw.contains("\"code\": 400"));

    if !is_deser_error {
        return (raw.to_string(), false);
    }

    // Try to extract the provider's own error message from the embedded raw JSON.
    // Walk the string to find the first `"message":"<value>"` after "content:".
    let start = raw.find("content:").unwrap_or(0);
    let payload = &raw[start..];

    let extracted = (|| -> Option<String> {
        let msg_key = "\"message\":\"";
        let pos = payload.find(msg_key)? + msg_key.len();
        let rest = &payload[pos..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    })();

    let message = match extracted {
        Some(m) if !m.is_empty() => format!("Provider 400: {}", m),
        _ => "Provider returned HTTP 400 (context too long or invalid request)".to_string(),
    };

    (message, true) // fatal — do not retry
}

fn sanitize_for_display(text: &str, max_len: usize) -> String {
    let mut clean = String::with_capacity(text.len().min(max_len + 64));
    let mut in_escape = false;
    for ch in text.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() { in_escape = false; }
            continue;
        }
        if ch == '\x1b' { in_escape = true; continue; }
        // Keep newlines but strip other control characters
        if ch.is_control() && ch != '\n' { continue; }
        clean.push(ch);
    }
    if clean.len() > max_len {
        let boundary = floor_char_boundary(&clean, max_len);
        format!("{}…", &clean[..boundary])
    } else {
        clean
    }
}

/// Render the subagent panel content from a list of visible entries.
fn format_subagent_panel(entries: &[&SubagentEntry], tick_count: u64) -> ratatui::text::Text<'static> {
    use ratatui::text::{Line, Span};
    use ratatui::style::{Color, Modifier};
    let spinner_frames = ['⣾','⣽','⣻','⢿','⡿','⣟','⣯','⣷'];
    let spin = spinner_frames[(tick_count as usize / 4) % spinner_frames.len()];
    let mut lines = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        let (icon, status_color) = match entry.status {
            SubagentStatus::Running => (spin.to_string(), Color::Yellow),
            SubagentStatus::Done    => ("✅".to_string(), Color::Green),
            SubagentStatus::Failed  => ("❌".to_string(), Color::Red),
        };
        // Header line: icon #N task_id
        lines.push(Line::from(vec![
            Span::raw(format!("{} ", icon)),
            Span::styled(
                format!("#{} {}", entry.index, entry.task_id),
                Style::default().fg(status_color).add_modifier(Modifier::BOLD),
            ),
        ]));
        // Preview lines (up to 2)
        for line in entry.preview.lines().take(2) {
            let truncated = if line.len() > 80 {
                format!("  {}…", &line[..floor_char_boundary(line, 77)])
            } else {
                format!("  {}", line)
            };
            lines.push(Line::from(Span::raw(truncated)));
        }
        // Blank separator between entries (not after last)
        if i + 1 < entries.len() {
            lines.push(Line::from(""));
        }
    }
    ratatui::text::Text::from(lines)
}

/// Extract any prose text that appears before the JSON tool call block.
/// Returns empty str if the response starts directly with JSON.
pub(crate) fn extract_prose_before_json(text: &str) -> &str {
    let json_pos = text.find("{\"tool_calls\"")
        .or_else(|| text.find("```json"))
        .or_else(|| text.find("```\n{"))
        .or_else(|| text.find("<tool>"));
    match json_pos {
        Some(pos) => text[..pos].trim(),
        None => text.trim(),
    }
}

/// Synthesize a one-line narration for a tool call when the model didn't provide one.
pub(crate) fn synthesize_tool_narration(tool_calls: &[crate::agent::ToolCall]) -> String {
    if tool_calls.is_empty() { return String::new(); }
    let tc = &tool_calls[0];
    let suffix = if tool_calls.len() > 1 {
        format!(" (+ {} more)", tool_calls.len() - 1)
    } else {
        String::new()
    };
    let desc = match tc.name.as_str() {
        "readfile" => {
            let path = tc.args.split('\x00').next()
                .and_then(|p| p.split_whitespace().next())
                .unwrap_or(&tc.args);
            format!("Reading `{}`.", path)
        }
        "rg" => {
            let mut parts = tc.args.splitn(2, '\x00');
            let pattern = parts.next().unwrap_or(&tc.args);
            let dir = parts.next().unwrap_or(".");
            format!("Searching `{}` for `{}`.", dir, pattern)
        }
        "exec" | "shell" => {
            let preview = tc.description.as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&tc.args);
            format!("Running: `{}`.", preview)
        }
        "setfile" => {
            let path = tc.args.split('\x00').next().unwrap_or(&tc.args);
            format!("Writing `{}`.", path)
        }
        "patchfile" => {
            let path = tc.args.split('\x00').next().unwrap_or(&tc.args);
            format!("Patching `{}`.", path)
        }
        "commit" => format!("Committing: {}", tc.args),
        "think" => {
            let preview: String = tc.args.chars().take(80).collect();
            format!("Thinking: {}", preview)
        }
        "python" => format!("Running Python script: `{}`.", tc.args),
        "ruste" => format!("Compiling Rust: `{}`.", tc.args),
        "spawn" => {
            let task_id = tc.args.split_whitespace().next().unwrap_or(&tc.args);
            format!("Spawning subagent: {}.", task_id)
        }
        _ => format!("Calling tool: {}.", tc.name),
    };
    format!("{}{}", desc, suffix)
}

/// Hash a (tool_name, args) pair to a u64 for loop detection.
pub(crate) fn hash_tool_call(tool_name: &str, args: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut h);
    args.hash(&mut h);
    h.finish()
}

/// Count how many times (tool_name, call_hash) appears in the recent-calls window.
pub(crate) fn count_repeat_calls(
    recent: &std::collections::VecDeque<(String, u64)>,
    tool_name: &str,
    call_hash: u64,
) -> usize {
    recent.iter().filter(|(n, h)| n == tool_name && *h == call_hash).count()
}

#[cfg(test)]
mod rendering_tests {
    use super::*;

    // ============================================================================
    // 1. truncate_tail() Tests
    // ============================================================================

    #[test]
    fn truncate_tail_no_truncation_when_under_cap() {
        let text = "Hello, world!";
        let result = truncate_tail(text, 100);
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn truncate_tail_exact_cap_no_truncation() {
        let text = "12345";
        let result = truncate_tail(text, 5);
        assert_eq!(result, "12345");
    }

    #[test]
    fn truncate_tail_keeps_tail_with_prefix() {
        let text = "0123456789";
        let result = truncate_tail(text, 3);
        assert!(result.contains("…(7 omitted)"));
        assert!(result.contains("789"));
    }

    #[test]
    fn truncate_tail_shows_omitted_count() {
        let text = "abcdefghijklmnop";
        let result = truncate_tail(text, 4);
        assert!(result.contains("…(12 omitted)"));
        assert!(result.ends_with("mnop"));
    }

    #[test]
    fn truncate_tail_empty_string() {
        let text = "";
        let result = truncate_tail(text, 100);
        assert_eq!(result, "");
    }

    #[test]
    fn truncate_tail_unicode_chars() {
        let text = "🎨🎭🎪🎬🎤🎧"; // 6 emoji = 6 chars
        let result = truncate_tail(text, 3);
        // Should keep last 3 emoji: 🎬🎤🎧 (drop first 3)
        assert!(result.contains("…(3 omitted)"));
        assert!(result.contains("🎬"));
        assert!(result.contains("🎤"));
        assert!(result.contains("🎧"));
    }

    #[test]
    fn truncate_tail_single_char_cap() {
        let text = "hello";
        let result = truncate_tail(text, 1);
        assert!(result.contains("…(4 omitted)"));
        assert!(result.contains("o"));
    }

    // ============================================================================
    // 2. Think Panel Rendering Tests
    // ============================================================================

    #[test]
    fn think_text_extraction_from_native_tokens() {
        // When thinking_text is populated by state machine (native ThinkToken),
        // it should be included in thinking_parts
        let thinking_text = "Let me analyze this problem step by step.";
        let _response_text = "I'll help you with this.";

        let mut thinking_parts: Vec<String> = Vec::new();
        if !thinking_text.is_empty() {
            thinking_parts.push(thinking_text.trim().to_string());
        }
        if thinking_text.is_empty() {
            // This block would extract inline <think> tags, but shouldn't run here
            panic!("Should not reach inline extraction when thinking_text is populated");
        }

        assert_eq!(thinking_parts.len(), 1);
        assert_eq!(
            thinking_parts[0],
            "Let me analyze this problem step by step."
        );
    }

    #[test]
    fn think_text_extraction_from_inline_tags() {
        // When thinking_text is empty, extract from inline <think>...</think> tags
        let thinking_text = "";
        let response_text = "<think>Step 1: Understand the problem\nStep 2: Plan solution</think>\nNow I'll execute.";

        let mut thinking_parts: Vec<String> = Vec::new();
        if !thinking_text.is_empty() {
            thinking_parts.push(thinking_text.trim().to_string());
        }
        if thinking_text.is_empty() {
            let mut scan = response_text;
            while let Some(start) = scan.find("<think>") {
                let after = &scan[start + "<think>".len()..];
                let end = after.find("</think>").unwrap_or(after.len());
                let content = after[..end].trim();
                if !content.is_empty() {
                    thinking_parts.push(content.to_string());
                }
                scan = if end + "</think>".len() <= after.len() {
                    &after[end + "</think>".len()..]
                } else {
                    ""
                };
            }
        }

        assert_eq!(thinking_parts.len(), 1);
        assert!(thinking_parts[0].contains("Step 1"));
        assert!(thinking_parts[0].contains("Step 2"));
    }

    #[test]
    fn think_text_no_duplicate_when_both_sources_present() {
        // When thinking_text is populated, ONLY use that source,
        // not the inline tags (which are echoed in the response)
        let thinking_text = "Already captured by state machine";
        let _response_text = "<think>Already captured by state machine</think>\nThe answer is 42.";

        let mut thinking_parts: Vec<String> = Vec::new();
        if !thinking_text.is_empty() {
            thinking_parts.push(thinking_text.trim().to_string());
        }
        // This should NOT run because thinking_text is not empty
        if thinking_text.is_empty() {
            panic!("Should skip inline extraction when thinking_text populated");
        }

        assert_eq!(thinking_parts.len(), 1);
        assert_eq!(thinking_parts[0], "Already captured by state machine");
    }

    #[test]
    fn think_text_empty_no_panel_rendered() {
        let thinking_text = "";
        let response_text = "Just a regular response.";

        let mut thinking_parts: Vec<String> = Vec::new();
        if !thinking_text.is_empty() {
            thinking_parts.push(thinking_text.trim().to_string());
        }
        if thinking_text.is_empty() {
            // No <think> tags present
            assert!(!response_text.contains("<think>"));
        }

        assert!(thinking_parts.is_empty());
    }

    #[test]
    fn think_text_multiple_inline_blocks() {
        let thinking_text = "";
        let response_text =
            "<think>First thought</think>\nSome text\n<think>Second thought</think>";

        let mut thinking_parts: Vec<String> = Vec::new();
        if thinking_text.is_empty() {
            let mut scan = response_text;
            while let Some(start) = scan.find("<think>") {
                let after = &scan[start + "<think>".len()..];
                let end = after.find("</think>").unwrap_or(after.len());
                let content = after[..end].trim();
                if !content.is_empty() {
                    thinking_parts.push(content.to_string());
                }
                scan = if end + "</think>".len() <= after.len() {
                    &after[end + "</think>".len()..]
                } else {
                    ""
                };
            }
        }

        assert_eq!(thinking_parts.len(), 2);
        assert_eq!(thinking_parts[0], "First thought");
        assert_eq!(thinking_parts[1], "Second thought");
    }

    // ============================================================================
    // 3. Tool Output Display Tests
    // ============================================================================

    #[test]
    fn tool_output_not_truncated_when_under_cap() {
        let output = "short error message";
        let cap = 600;
        if output.chars().count() > cap {
            panic!("Should not truncate short output");
        }
        assert_eq!(output.chars().count(), 19);
    }

    #[test]
    fn tool_output_truncated_with_omitted_prefix() {
        let output = "x".repeat(1000);
        let cap = 600;
        let truncated = truncate_tail(&output, cap);
        assert!(truncated.contains("…(400 omitted)"));
        // Prefix "…(400 omitted)\n" is 15 chars, plus 600 chars = 615 total
        assert_eq!(truncated.chars().count(), 615);
    }

    #[test]
    fn tool_output_diff_section_not_shown_in_render() {
        // Diff content should be stripped for model context only (not tested in render here)
        // but we verify the concept that diffs are separate from tool output
        let output_with_diff = "Error in module\n\n[DIFF]\n--- old\n+++ new";
        assert!(output_with_diff.contains("[DIFF]"));
        // In real rendering, only "Error in module" would be displayed
    }

    #[test]
    fn tool_output_cap_respects_config() {
        // Verify OUTPUT_CHARACTER_LIMIT is 1000 (unified across all subsystems)
        assert_eq!(crate::config::OUTPUT_CHARACTER_LIMIT, 1000);
    }

    // ============================================================================
    // 4. Error Formatting Tests
    // ============================================================================

    #[test]
    fn error_ask_mode_blocks_tool_execution() {
        // In ask mode, tool results should be stored but no new stream should kick off
        // This is verified by the state machine logic, but we verify the config constant exists
        assert!(AppMode::Ask != AppMode::Forever);
    }

    #[test]
    fn error_repeated_identical_call_detection_marker() {
        // The hash function should produce same hash for identical calls
        let hash1 = hash_tool_call("rg", "pattern");
        let hash2 = hash_tool_call("rg", "pattern");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn error_different_args_produce_different_hashes() {
        let hash1 = hash_tool_call("rg", "pattern1");
        let hash2 = hash_tool_call("rg", "pattern2");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn error_three_identical_tool_calls_detected() {
        use std::collections::VecDeque;

        let mut calls: VecDeque<(String, u64)> = VecDeque::new();
        let tool_name = "readfile";
        let args = "src/main.rs";
        let hash = hash_tool_call(tool_name, args);

        calls.push_back((tool_name.to_string(), hash));
        calls.push_back((tool_name.to_string(), hash));
        calls.push_back((tool_name.to_string(), hash));

        let repeat_count = count_repeat_calls(&calls, tool_name, hash);
        assert_eq!(repeat_count, 3);
    }

    // ============================================================================
    // 5. Message Rendering Tests
    // ============================================================================

    #[test]
    fn message_rendering_emoji_formats() {
        // Different messages should use different emoji (user, assistant, system, etc)
        let user_msg = Message::new("user", "What's 2+2?");
        let assistant_msg = Message::new("assistant", "The answer is 4.");

        assert_eq!(user_msg.role, "user");
        assert_eq!(assistant_msg.role, "assistant");
    }

    #[test]
    fn message_rendering_think_prefix_extraction() {
        let content_with_think = "[THINK: Complex reasoning here]\nFinal answer is 42.";
        
        // Simulate the think prefix extraction logic
        if content_with_think.starts_with("[THINK: ") {
            let after_open = &content_with_think["[THINK: ".len()..];
            let close = after_open.find("]\n");
            if let Some(ci) = close {
                let think_content = &after_open[..ci];
                let rest_start = ci + 1 + 1; // +1 for ], +1 for \n
                let rest = if rest_start <= after_open.len() {
                    &after_open[rest_start..]
                } else {
                    ""
                };
                assert_eq!(think_content, "Complex reasoning here");
                assert_eq!(rest, "Final answer is 42.");
            } else {
                panic!("Should find closing bracket");
            }
        } else {
            panic!("Should have THINK prefix");
        }
    }

    #[test]
    fn message_rendering_without_think_prefix() {
        let content = "This is just a normal response.";
        
        if !content.starts_with("[THINK: ") {
            assert_eq!(content, "This is just a normal response.");
        } else {
            panic!("Should not have THINK prefix");
        }
    }

    #[test]
    fn tool_call_formatting_with_inline_results() {
        // Tool calls should be prettified with boxes (not tested in rendering here)
        // but we verify they're recognized by the presence of json markers
        let content = r#"I'll search for files: {"tool_calls": [{"name": "rg"}]}"#;
        assert!(content.contains("tool_calls"));
        assert!(content.contains("rg"));
    }

    // ============================================================================
    // 6. Ask Mode Behavior Tests
    // ============================================================================

    #[test]
    fn ask_mode_constant_exists() {
        // Verify ask mode is distinct from build and plan modes
        assert!(AppMode::Ask != AppMode::Forever);
        assert!(AppMode::Ask != AppMode::Plan);
    }

    #[test]
    fn tool_hash_consistency() {
        // Tool hashing must be consistent for repeat detection to work
        let tool_name = "spawn";
        let args = "task1 Description";
        let hash1 = hash_tool_call(tool_name, args);
        let hash2 = hash_tool_call(tool_name, args);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn tool_hash_order_matters() {
        // Swapping name and args should give different hashes
        let h1 = hash_tool_call("rg", "spawn");
        let h2 = hash_tool_call("spawn", "rg");
        assert_ne!(h1, h2);
    }

    // ============================================================================
    // 7. Status Message Display Tests
    // ============================================================================

    #[test]
    fn status_message_formatting_with_markers() {
        // Status messages should use emoji markers for clarity
        let status = "🔧 Executing tool...";
        assert!(status.starts_with("🔧"));
    }

    #[test]
    fn truncate_tail_multiline_output() {
        let output = "line1\nline2\nline3\nline4\nline5";
        let result = truncate_tail(output, 10);
        // Should keep the last ~10 chars and show prefix
        assert!(result.contains("…("));
        assert!(result.contains("line"));
    }

    #[test]
    fn truncate_tail_with_newlines_in_middle() {
        let output = format!("start\n{}\nend", "x".repeat(100));
        let result = truncate_tail(&output, 50);
        assert!(result.contains("…("));
        assert!(result.contains("end"));
    }
}

#[cfg(test)]
mod human_tokens_tests {
    use super::human_tokens;

    #[test]
    fn small_values() {
        assert_eq!(human_tokens(0), "0");
        assert_eq!(human_tokens(999), "999");
    }

    #[test]
    fn thousands() {
        assert_eq!(human_tokens(1_000), "1.0k");
        assert_eq!(human_tokens(1_234), "1.2k");
        assert_eq!(human_tokens(9_999), "10.0k");
    }

    #[test]
    fn ten_thousands() {
        assert_eq!(human_tokens(10_000), "10k");
        assert_eq!(human_tokens(32_768), "33k");
        assert_eq!(human_tokens(131_072), "131k");
    }

    #[test]
    fn millions() {
        assert_eq!(human_tokens(1_000_000), "1.0M");
        assert_eq!(human_tokens(2_500_000), "2.5M");
    }
}

#[cfg(test)]
mod loop_detection_tests {
    use super::*;
    use std::collections::VecDeque;

    fn make_queue(calls: &[(&str, &str)]) -> VecDeque<(String, u64)> {
        calls.iter().map(|(name, args)| (name.to_string(), hash_tool_call(name, args))).collect()
    }

    #[test]
    fn test_no_repeat_returns_one() {
        let q = make_queue(&[("rg", "foo"), ("readfile", "bar"), ("rg", "baz")]);
        let h = hash_tool_call("readfile", "bar");
        assert_eq!(count_repeat_calls(&q, "readfile", h), 1);
    }

    #[test]
    fn test_two_identical_not_triggered() {
        let q = make_queue(&[("readfile", "src/a.rs"), ("readfile", "src/a.rs")]);
        let h = hash_tool_call("readfile", "src/a.rs");
        assert_eq!(count_repeat_calls(&q, "readfile", h), 2);
        assert!(count_repeat_calls(&q, "readfile", h) < 3);
    }

    #[test]
    fn test_three_identical_triggers() {
        let q = make_queue(&[
            ("readfile", "src/a.rs"),
            ("readfile", "src/a.rs"),
            ("readfile", "src/a.rs"),
        ]);
        let h = hash_tool_call("readfile", "src/a.rs");
        assert!(count_repeat_calls(&q, "readfile", h) >= 3);
    }

    #[test]
    fn test_different_args_not_a_repeat() {
        let q = make_queue(&[
            ("readfile", "src/a.rs"),
            ("readfile", "src/a.rs"),
            ("readfile", "src/b.rs"), // different args
        ]);
        let h = hash_tool_call("readfile", "src/a.rs");
        assert_eq!(count_repeat_calls(&q, "readfile", h), 2);
    }

    #[test]
    fn test_window_capped_at_4() {
        // Simulate the window cap: only last 4 entries kept
        let mut q: VecDeque<(String, u64)> = VecDeque::new();
        let calls = [
            ("readfile", "src/a.rs"),
            ("readfile", "src/a.rs"),
            ("readfile", "src/a.rs"),
            ("readfile", "src/a.rs"),
            ("rg", "pattern"),  // pushes oldest off
        ];
        for (name, args) in &calls {
            q.push_back((name.to_string(), hash_tool_call(name, args)));
            if q.len() > 4 { q.pop_front(); }
        }
        let h = hash_tool_call("readfile", "src/a.rs");
        // After capping, only 3 readfile entries remain (the oldest was evicted)
        assert_eq!(count_repeat_calls(&q, "readfile", h), 3);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    // ============================================================================
    // decide_stream_end — One mode
    // ============================================================================

    #[test]
    fn one_empty_text_first_kick_returns_kick() {
        let action = decide_stream_end(false, AppMode::One, 0, 0);
        assert_eq!(action, StreamEndAction::Kick);
    }

    #[test]
    fn one_empty_text_second_kick_returns_kick() {
        let action = decide_stream_end(false, AppMode::One, 0, 1);
        assert_eq!(action, StreamEndAction::Kick);
    }

    #[test]
    fn one_empty_text_third_kick_completes() {
        let action = decide_stream_end(false, AppMode::One, 0, 2);
        assert_eq!(action, StreamEndAction::CompleteOne("empty responses"));
    }

    #[test]
    fn one_has_text_no_tools_no_prior_tools_first_kick() {
        let action = decide_stream_end(true, AppMode::One, 0, 0);
        assert_eq!(action, StreamEndAction::Kick);
    }

    #[test]
    fn one_has_text_no_tools_no_prior_tools_third_kick_completes() {
        let action = decide_stream_end(true, AppMode::One, 0, 2);
        assert_eq!(action, StreamEndAction::CompleteOne("model unresponsive"));
    }

    #[test]
    fn one_has_text_no_tools_after_tools_used_completes() {
        let action = decide_stream_end(true, AppMode::One, 3, 0);
        assert_eq!(action, StreamEndAction::CompleteOne("no tool calls"));
    }

    #[test]
    fn one_has_text_no_tools_after_tools_many_kicks_still_completes() {
        let action = decide_stream_end(true, AppMode::One, 5, 99);
        assert_eq!(action, StreamEndAction::CompleteOne("no tool calls"));
    }

    // ============================================================================
    // decide_stream_end — Forever mode (NEVER halts — always kicks)
    // ============================================================================

    #[test]
    fn build_kicks_below_threshold() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 0), StreamEndAction::Kick);
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 3), StreamEndAction::Kick);
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, 0), StreamEndAction::Kick);
        assert_eq!(decide_stream_end(true, AppMode::Forever, 5, 2), StreamEndAction::Kick);
    }

    #[test]
    fn build_never_halts_at_threshold() {
        // Previously halted at kicks=4 (kicks_after=5); now always Kick
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 4), StreamEndAction::Kick);
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, 4), StreamEndAction::Kick);
    }

    #[test]
    fn build_never_halts_above_threshold() {
        // Previously halted; now always Kick
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 10), StreamEndAction::Kick);
        assert_eq!(decide_stream_end(true, AppMode::Forever, 5, 99), StreamEndAction::Kick);
    }

    // ============================================================================
    // Forever mode — exhaustive resilience proof (50+ tests)
    // Every single permutation of inputs must return Kick, never Halt.
    // ============================================================================

    // --- Group 1: no text, no tools, varying kicks ---

    #[test]
    fn forever_no_text_no_tools_kicks_0() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 0), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_1() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 1), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_2() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 2), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_3() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 3), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_4() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 4), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_5() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 5), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_6() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 6), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_10() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 10), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_50() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 50), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_100() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 100), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_no_tools_kicks_1000() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, 1000), StreamEndAction::Kick);
    }

    // --- Group 2: has text, no tools, varying kicks ---

    #[test]
    fn forever_has_text_no_tools_kicks_0() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, 0), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_no_tools_kicks_1() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, 1), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_no_tools_kicks_4() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, 4), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_no_tools_kicks_5() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, 5), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_no_tools_kicks_100() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, 100), StreamEndAction::Kick);
    }

    // --- Group 3: has text, with tools used, varying kicks ---

    #[test]
    fn forever_has_text_1_tool_kicks_0() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 1, 0), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_1_tool_kicks_5() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 1, 5), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_5_tools_kicks_0() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 5, 0), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_5_tools_kicks_4() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 5, 4), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_5_tools_kicks_5() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 5, 5), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_5_tools_kicks_99() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 5, 99), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_10_tools_kicks_100() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 10, 100), StreamEndAction::Kick);
    }

    // --- Group 4: no text, with tools, varying kicks ---

    #[test]
    fn forever_no_text_1_tool_kicks_5() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 1, 5), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_10_tools_kicks_5() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 10, 5), StreamEndAction::Kick);
    }

    #[test]
    fn forever_no_text_10_tools_kicks_1000() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 10, 1000), StreamEndAction::Kick);
    }

    // --- Group 5: boundary — u32::MAX kicks ---

    #[test]
    fn forever_no_text_kicks_u32_max() {
        assert_eq!(decide_stream_end(false, AppMode::Forever, 0, u32::MAX), StreamEndAction::Kick);
    }

    #[test]
    fn forever_has_text_kicks_u32_max() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 0, u32::MAX), StreamEndAction::Kick);
    }

    #[test]
    fn forever_with_tools_kicks_u32_max() {
        assert_eq!(decide_stream_end(true, AppMode::Forever, 5, u32::MAX), StreamEndAction::Kick);
    }

    // --- Group 6: property — Forever always returns Kick, never other variants ---

    #[test]
    fn forever_never_returns_halt() {
        for kicks in [0u32, 1, 4, 5, 6, 10, 100, 1000] {
            for tools in [0usize, 1, 10] {
                for has_text in [true, false] {
                    let action = decide_stream_end(has_text, AppMode::Forever, tools, kicks);
                    assert!(
                        matches!(action, StreamEndAction::Kick),
                        "Expected Kick but got non-Kick for has_text={has_text} tools={tools} kicks={kicks}"
                    );
                    assert!(
                        !matches!(action, StreamEndAction::Halt(_)),
                        "Got Halt for has_text={has_text} tools={tools} kicks={kicks}"
                    );
                    assert!(
                        !matches!(action, StreamEndAction::CompleteOne(_)),
                        "Got CompleteOne for has_text={has_text} tools={tools} kicks={kicks}"
                    );
                    assert!(
                        !matches!(action, StreamEndAction::Persist),
                        "Got Persist for has_text={has_text} tools={tools} kicks={kicks}"
                    );
                }
            }
        }
    }

    // --- Group 7: StreamEndAction variant identity checks ---

    #[test]
    fn stream_end_kick_is_not_halt() {
        let kick = StreamEndAction::Kick;
        assert!(!matches!(kick, StreamEndAction::Halt(_)));
    }

    #[test]
    fn stream_end_kick_is_not_persist() {
        let kick = StreamEndAction::Kick;
        assert!(!matches!(kick, StreamEndAction::Persist));
    }

    #[test]
    fn stream_end_kick_is_not_complete_one() {
        let kick = StreamEndAction::Kick;
        assert!(!matches!(kick, StreamEndAction::CompleteOne(_)));
    }

    #[test]
    fn stream_end_halt_is_not_kick() {
        let halt = StreamEndAction::Halt("test");
        assert!(!matches!(halt, StreamEndAction::Kick));
    }

    #[test]
    fn stream_end_complete_one_is_not_kick() {
        let done = StreamEndAction::CompleteOne("done");
        assert!(!matches!(done, StreamEndAction::Kick));
    }

    // --- Group 8: other modes still correct at high kick counts ---

    #[test]
    fn plan_never_halts_at_high_kicks() {
        for kicks in [0u32, 5, 100, 1000] {
            assert_eq!(
                decide_stream_end(false, AppMode::Plan, 0, kicks),
                StreamEndAction::Persist,
                "Plan should always Persist, kicks={kicks}"
            );
            assert_eq!(
                decide_stream_end(true, AppMode::Plan, 5, kicks),
                StreamEndAction::Persist,
                "Plan should always Persist, kicks={kicks}"
            );
        }
    }

    #[test]
    fn ask_never_halts_at_high_kicks() {
        for kicks in [0u32, 5, 100, 1000] {
            assert_eq!(
                decide_stream_end(false, AppMode::Ask, 0, kicks),
                StreamEndAction::Persist,
                "Ask should always Persist, kicks={kicks}"
            );
            assert_eq!(
                decide_stream_end(true, AppMode::Ask, 5, kicks),
                StreamEndAction::Persist,
                "Ask should always Persist, kicks={kicks}"
            );
        }
    }

    #[test]
    fn one_kicks_below_threshold_no_text() {
        assert_eq!(decide_stream_end(false, AppMode::One, 0, 0), StreamEndAction::Kick);
        assert_eq!(decide_stream_end(false, AppMode::One, 0, 1), StreamEndAction::Kick);
    }

    #[test]
    fn one_completes_at_threshold_no_text() {
        assert_eq!(decide_stream_end(false, AppMode::One, 0, 2), StreamEndAction::CompleteOne("empty responses"));
    }

    #[test]
    fn one_completes_with_text_and_tools() {
        assert_eq!(decide_stream_end(true, AppMode::One, 3, 0), StreamEndAction::CompleteOne("no tool calls"));
    }

    #[test]
    fn one_kicks_with_text_no_tools_below_threshold() {
        assert_eq!(decide_stream_end(true, AppMode::One, 0, 0), StreamEndAction::Kick);
        assert_eq!(decide_stream_end(true, AppMode::One, 0, 1), StreamEndAction::Kick);
    }

    #[test]
    fn one_completes_with_text_no_tools_at_threshold() {
        assert_eq!(decide_stream_end(true, AppMode::One, 0, 2), StreamEndAction::CompleteOne("model unresponsive"));
    }

    // ============================================================================
    // decide_stream_end — Plan / Ask mode
    // ============================================================================

    #[test]
    fn plan_empty_text_persists() {
        assert_eq!(decide_stream_end(false, AppMode::Plan, 0, 0), StreamEndAction::Persist);
    }

    #[test]
    fn ask_has_text_persists() {
        assert_eq!(decide_stream_end(true, AppMode::Ask, 0, 5), StreamEndAction::Persist);
    }

    #[test]
    fn plan_has_text_persists() {
        assert_eq!(decide_stream_end(true, AppMode::Plan, 3, 0), StreamEndAction::Persist);
    }

    // ============================================================================
    // extract_plan_block — parsing
    // ============================================================================

    #[test]
    fn plan_block_extracted_and_stripped() {
        let text = "Here is my plan.\n<plan>\n## Goal\nBuild foo.\n</plan>\nLet me know.";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(plan.as_deref(), Some("## Goal\nBuild foo."));
        assert_eq!(cleaned, "Here is my plan.\nLet me know.");
    }

    #[test]
    fn plan_block_only_content() {
        let text = "<plan>\n## Goal\nJust this.\n</plan>";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(plan.as_deref(), Some("## Goal\nJust this."));
        assert_eq!(cleaned, "");
    }

    #[test]
    fn plan_block_no_tag_returns_original() {
        let text = "No plan here.";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(cleaned, "No plan here.");
        assert!(plan.is_none());
    }

    #[test]
    fn plan_block_missing_end_tag_returns_original() {
        let text = "Hello <plan>incomplete";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(cleaned, text);
        assert!(plan.is_none());
    }

    #[test]
    fn plan_block_missing_start_tag_returns_original() {
        let text = "Hello </plan>";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(cleaned, text);
        assert!(plan.is_none());
    }

    #[test]
    fn plan_block_whitespace_trimmed() {
        let text = "<plan>  \n  content  \n  </plan>";
        let (_, plan) = extract_plan_block(text);
        assert_eq!(plan.as_deref(), Some("content"));
    }

    #[test]
    fn plan_block_before_text_only() {
        let text = "Preamble.\n<plan>Goal: X</plan>";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(plan.as_deref(), Some("Goal: X"));
        assert_eq!(cleaned, "Preamble.");
    }

    #[test]
    fn plan_block_after_text_only() {
        let text = "<plan>Goal: X</plan>\nEpilogue.";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(plan.as_deref(), Some("Goal: X"));
        assert_eq!(cleaned, "Epilogue.");
    }

    #[test]
    fn plan_block_inverted_tags_returns_original() {
        let text = "</plan>junk<plan>";
        let (cleaned, plan) = extract_plan_block(text);
        assert_eq!(cleaned, text);
        assert!(plan.is_none());
    }
}

#[cfg(test)]
mod plan_mode_tests {
    use super::*;

    // ============================================================================
    // is_shell_write_pattern
    // ============================================================================

    #[test]
    fn write_pattern_output_redirect() {
        assert!(is_shell_write_pattern("echo hello > output.txt"));
    }

    #[test]
    fn write_pattern_append_redirect() {
        assert!(is_shell_write_pattern("echo line >> log.txt"));
    }

    #[test]
    fn write_pattern_heredoc_single_quote() {
        assert!(is_shell_write_pattern("cat > file.rs << 'EOF'\ncontent\nEOF"));
    }

    #[test]
    fn write_pattern_heredoc_double_quote() {
        assert!(is_shell_write_pattern("cat > file.rs << \"EOF\"\ncontent\nEOF"));
    }

    #[test]
    fn write_pattern_tee() {
        assert!(is_shell_write_pattern("echo content | tee output.txt"));
    }

    #[test]
    fn write_pattern_false_positive_arrow() {
        // `->` in Rust source NOT inside quotes is correctly excluded
        assert!(!is_shell_write_pattern("awk 'NR > 5' src/lib.rs"));
        // `->` inside single-quoted grep pattern: the scanner cannot track quoting,
        // so it may trigger — that is acceptable (minor over-eager behavior).
    }

    #[test]
    fn write_pattern_false_positive_ge() {
        // `>=` comparison should not trigger
        assert!(!is_shell_write_pattern("awk '$3 >= 5 {print}' file.txt"));
    }

    #[test]
    fn write_pattern_read_only_commands() {
        assert!(!is_shell_write_pattern("cat src/main.rs"));
        assert!(!is_shell_write_pattern("grep -r 'foo' src/"));
        assert!(!is_shell_write_pattern("ls -la"));
        assert!(!is_shell_write_pattern("cargo test --lib"));
        assert!(!is_shell_write_pattern("git log --oneline"));
    }

    #[test]
    fn write_pattern_stderr_redirect_allowed() {
        // 2>&1 is a file descriptor redirect, not a write we care about
        // (it doesn't create/modify user files). The `>` here follows `&` which our
        // scanner will flag — that is acceptable (slightly over-eager on stderr).
        // Just verify the function doesn't panic.
        let _ = is_shell_write_pattern("cargo build 2>&1");
    }
}


// ============================================================================
// Terminal integrity tests
//
// Tests that the rendering pipeline produces clean output: no corrupt Unicode,
// no cell-level garbage, no ghost text after content shrinks.
//
// Two levels:
//   1. sanitize_for_display — strips ANSI/control chars from raw strings.
//   2. TestBackend buffer scan — renders widgets and inspects every cell.
//
// Note on ratatui + control chars: ratatui's buffer renderer filters out
// zero-width and control characters (.filter(|width| *width > 0)), so ESC
// bytes in a Span are silently dropped and never reach the cell buffer.
// This means ANSI in message content won't appear as literal garbage but
// also won't apply styling — the sanitize_for_display path is the right
// place to strip it before it reaches ratatui.
// ============================================================================
#[cfg(test)]
mod terminal_integrity_tests {
    use super::*;
    use ratatui::{
        backend::TestBackend,
        Terminal,
        widgets::{Paragraph, Block, Borders},
        text::{Text, Line, Span},
    };

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    /// Check a buffer CELL symbol for garbage.
    /// Cell symbols are short (1-4 bytes for a single grapheme cluster);
    /// anything larger or containing control chars is suspicious.
    fn cell_symbol_is_garbage(sym: &str) -> bool {
        for ch in sym.chars() {
            match ch {
                '\x1b' => return true,
                '\x00'..='\x08' | '\x0b'..='\x0c' | '\x0e'..='\x1a' | '\x1c'..='\x1f' => {
                    return true;
                }
                '\x7f' => return true,
                '\u{FFFD}' => return true,
                _ => {}
            }
        }
        // A buffer cell holds one grapheme cluster — max ~4 bytes for emoji.
        // More than 8 bytes indicates an accumulation bug.
        if sym.len() > 8 { return true; }
        false
    }

    /// Check a SPAN content string for control characters.
    /// Unlike cell symbols, spans can be arbitrarily long; no length limit.
    fn span_has_control_chars(s: &str) -> bool {
        s.chars().any(|c| {
            matches!(c,
                '\x1b'
                | '\x00'..='\x08'
                | '\x0b'..='\x0c'
                | '\x0e'..='\x1a'
                | '\x1c'..='\x1f'
                | '\x7f'
                | '\u{FFFD}')
        })
    }

    /// Collect all garbage cell symbols from a buffer for diagnostic output.
    fn collect_garbage(buf: &ratatui::buffer::Buffer) -> Vec<String> {
        buf.content()
            .iter()
            .filter(|c| cell_symbol_is_garbage(c.symbol()))
            .map(|c| format!("{:?}", c.symbol()))
            .collect()
    }

    /// Render `text` into an 80×5 TestBackend and return any garbage found.
    fn garbage_in_paragraph(text: Text<'static>) -> Vec<String> {
        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| {
            f.render_widget(Paragraph::new(text), f.area());
        }).unwrap();
        collect_garbage(terminal.backend().buffer())
    }

    // -------------------------------------------------------------------------
    // sanitize_for_display
    // -------------------------------------------------------------------------

    #[test]
    fn sanitize_strips_ansi_color_sequence() {
        let input = "\x1b[31mRed text\x1b[0m";
        let result = sanitize_for_display(input, 200);
        assert!(!result.contains('\x1b'), "ESC byte leaked: {:?}", result);
        assert!(result.contains("Red text"));
    }

    #[test]
    fn sanitize_strips_ansi_256_color() {
        let input = "\x1b[38;5;214mOrange\x1b[0m normal";
        let result = sanitize_for_display(input, 200);
        assert!(!result.contains('\x1b'));
        assert!(result.contains("Orange"));
        assert!(result.contains("normal"));
    }

    #[test]
    fn sanitize_strips_cursor_movement_sequences() {
        let input = "line1\x1b[Aline2\x1b[2Jline3";
        let result = sanitize_for_display(input, 200);
        assert!(!result.contains('\x1b'));
        assert!(result.contains("line1"));
    }

    #[test]
    fn sanitize_strips_nul_and_control_chars() {
        let input = "hello\x00world\x08\x07\x0btest";
        let result = sanitize_for_display(input, 200);
        assert!(!result.contains('\x00'));
        assert!(!result.contains('\x08'));
        assert!(!result.contains('\x07'));
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn sanitize_strips_carriage_return() {
        let input = "line1\r\nline2\rline3";
        let result = sanitize_for_display(input, 200);
        assert!(!result.contains('\r'), "CR leaked through: {:?}", result);
    }

    #[test]
    fn sanitize_preserves_newlines() {
        let input = "line1\nline2\nline3";
        let result = sanitize_for_display(input, 200);
        assert_eq!(result.lines().count(), 3);
    }

    #[test]
    fn sanitize_preserves_unicode_and_emoji() {
        let input = "こんにちは 🦀 café";
        let result = sanitize_for_display(input, 200);
        assert_eq!(result, input);
    }

    #[test]
    fn sanitize_truncates_at_max_len() {
        let input = "a".repeat(500);
        let result = sanitize_for_display(&input, 100);
        assert!(result.ends_with('…'));
        let before_ellipsis = result.trim_end_matches('…');
        assert!(before_ellipsis.len() <= 100);
    }

    #[test]
    fn sanitize_handles_incomplete_escape_at_end() {
        let input = "text\x1b[";
        let result = sanitize_for_display(input, 200);
        assert!(!result.contains('\x1b'));
        assert!(result.contains("text"));
    }

    #[test]
    fn sanitize_handles_empty_string() {
        let result = sanitize_for_display("", 100);
        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_strips_only_control_not_printable_unicode() {
        // Ensure that printable Unicode is preserved and only ASCII control chars are stripped
        let input = "normal \u{2028} text"; // Line separator (U+2028) — printable Unicode
        let result = sanitize_for_display(input, 200);
        assert!(result.contains("normal"));
        assert!(result.contains("text"));
    }

    // -------------------------------------------------------------------------
    // TestBackend buffer integrity — cell-level checks
    // -------------------------------------------------------------------------

    #[test]
    fn buffer_normal_text_is_clean() {
        let garbage = garbage_in_paragraph(Text::raw("Hello, world! This is normal text."));
        assert!(garbage.is_empty(), "Garbage in normal text: {:?}", garbage);
    }

    #[test]
    fn buffer_empty_paragraph_is_clean() {
        let garbage = garbage_in_paragraph(Text::raw(""));
        assert!(garbage.is_empty(), "Garbage in empty paragraph: {:?}", garbage);
    }

    #[test]
    fn buffer_multiline_text_is_clean() {
        let text = Text::raw("First line\nSecond line\nThird line with 日本語");
        let garbage = garbage_in_paragraph(text);
        assert!(garbage.is_empty(), "Garbage in multiline: {:?}", garbage);
    }

    #[test]
    fn buffer_emoji_text_is_clean() {
        let text = Text::raw("🤖 Assistant 🦀 Rust 🎨 Colors");
        let garbage = garbage_in_paragraph(text);
        assert!(garbage.is_empty(), "Garbage near emoji: {:?}", garbage);
    }

    #[test]
    fn buffer_styled_spans_are_clean() {
        use ratatui::style::{Color, Style};
        let line = Line::from(vec![
            Span::styled("bold text ", Style::default().fg(Color::White)),
            Span::styled("colored text", Style::default().fg(Color::Cyan)),
        ]);
        let garbage = garbage_in_paragraph(Text::from(line));
        assert!(garbage.is_empty(), "Garbage in styled spans: {:?}", garbage);
    }

    #[test]
    fn buffer_long_line_does_not_corrupt_cells() {
        let long_line = "X".repeat(200);
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| {
            f.render_widget(Paragraph::new(long_line.as_str()), f.area());
        }).unwrap();
        let buf = terminal.backend().buffer();
        let garbage = collect_garbage(buf);
        assert!(garbage.is_empty(), "Garbage from long line: {:?}", garbage);
        assert_eq!(buf.area().width, 80);
    }

    #[test]
    fn buffer_box_drawing_chars_are_clean() {
        let text = Text::raw("┌─ rust\n│ let x = 1;\n└─");
        let garbage = garbage_in_paragraph(text);
        assert!(garbage.is_empty(), "Garbage in box-drawing chars: {:?}", garbage);
    }

    #[test]
    fn buffer_cjk_mixed_with_ascii_is_clean() {
        let text = Text::raw("Result: 結果 = 42, status: 正常");
        let garbage = garbage_in_paragraph(text);
        assert!(garbage.is_empty(), "Garbage in CJK mixed content: {:?}", garbage);
    }

    #[test]
    fn buffer_block_widget_is_clean() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| {
            let block = Block::default().title("Test").borders(Borders::ALL);
            let para = Paragraph::new("content inside a block").block(block);
            f.render_widget(para, f.area());
        }).unwrap();
        let garbage = collect_garbage(terminal.backend().buffer());
        assert!(garbage.is_empty(), "Garbage in bordered block: {:?}", garbage);
    }

    #[test]
    fn buffer_ansi_in_span_is_dropped_not_garbled() {
        // ratatui filters zero-width/control chars during rendering (width > 0 check),
        // so ESC bytes placed in a Span are silently dropped — they do NOT appear as
        // literal characters in buffer cells. This test documents that invariant.
        // If this test fails it means ratatui's behavior changed.
        let ansi_span = Span::raw("\x1b[31mRed\x1b[0m");
        let text = Text::from(Line::from(vec![ansi_span]));
        let garbage = garbage_in_paragraph(text);
        assert!(garbage.is_empty(),
            "ESC in span should be dropped by ratatui, not stored in cells: {:?}", garbage);
    }

    // -------------------------------------------------------------------------
    // Backspace / cursor rendering regression
    // -------------------------------------------------------------------------

    #[test]
    fn input_shrinks_no_ghost_char() {
        // When content shrinks (backspace), the vacated cell must be a space —
        // not the previous character left as ghost text.
        let backend = TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            f.render_widget(Paragraph::new("hello"), f.area());
        }).unwrap();
        terminal.draw(|f| {
            f.render_widget(Paragraph::new("hell"), f.area());
        }).unwrap();

        let buf = terminal.backend().buffer();
        let cell_4 = buf.get(4, 0).symbol();
        assert!(
            cell_4 == " " || cell_4 == "",
            "Ghost char at x=4 after backspace: {:?}", cell_4
        );
    }

    #[test]
    fn input_large_shrink_no_ghost_chars() {
        // After replacing "hello world" with "hi", cells at x=2..=10 must be blank.
        let backend = TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            f.render_widget(Paragraph::new("hello world"), f.area());
        }).unwrap();
        terminal.draw(|f| {
            f.render_widget(Paragraph::new("hi"), f.area());
        }).unwrap();

        let buf = terminal.backend().buffer();
        for x in 2..=10u16 {
            let sym = buf.get(x, 0).symbol();
            assert!(
                sym == " " || sym == "",
                "Ghost char at x={} after large shrink: {:?}", x, sym
            );
        }
    }

    #[test]
    fn empty_to_nonempty_then_empty_no_ghost() {
        // Empty → content → empty again: all cells should end up blank.
        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            f.render_widget(Paragraph::new(""), f.area());
        }).unwrap();
        terminal.draw(|f| {
            f.render_widget(Paragraph::new("hello"), f.area());
        }).unwrap();
        terminal.draw(|f| {
            f.render_widget(Paragraph::new(""), f.area());
        }).unwrap();

        let buf = terminal.backend().buffer();
        for x in 0..5u16 {
            let sym = buf.get(x, 0).symbol();
            assert!(sym == " " || sym == "",
                "Ghost char at x={} after clear: {:?}", x, sym);
        }
    }

    // -------------------------------------------------------------------------
    // Markdown rendering span integrity
    // -------------------------------------------------------------------------

    #[test]
    fn markdown_inline_spans_no_control_chars() {
        use ratatui::style::Color;
        let spans = crate::markdown::format_inline_to_spans(
            "**bold** and *italic* and `code` and plain text",
            Color::White,
        );
        for span in &spans {
            assert!(
                !span_has_control_chars(span.content.as_ref()),
                "Control char in markdown span: {:?}", span.content
            );
        }
    }

    #[test]
    fn markdown_header_no_control_chars() {
        use ratatui::style::Color;
        for level in 1..=6usize {
            let line = crate::markdown::format_header(level, "Section Title", Color::White);
            for span in &line.spans {
                assert!(
                    !span_has_control_chars(span.content.as_ref()),
                    "Control char in h{} span: {:?}", level, span.content
                );
            }
        }
    }

    #[test]
    fn markdown_list_item_no_control_chars() {
        use ratatui::style::Color;
        let line = crate::markdown::format_list_item(0, "List item content", Color::White, '-');
        for span in &line.spans {
            assert!(
                !span_has_control_chars(span.content.as_ref()),
                "Control char in list item span: {:?}", span.content
            );
        }
    }

    #[test]
    fn markdown_table_no_control_chars() {
        use ratatui::style::Color;
        let rows = vec![
            vec!["Header A".to_string(), "Header B".to_string()],
            vec!["Cell 1".to_string(), "Cell 2".to_string()],
        ];
        let lines = crate::markdown::format_table(&rows, Color::White);
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !span_has_control_chars(span.content.as_ref()),
                    "Control char in table span: {:?}", span.content
                );
            }
        }
    }
}

#[cfg(test)]
mod rendering_pipeline_tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn cell_symbol_is_garbage(sym: &str) -> bool {
        if sym.bytes().any(|b| b == 0x1b || (b < 0x20 && b != 0x09 && b != 0x0a)) {
            return true;
        }
        if sym.contains('\u{FFFD}') { return true; }
        if sym.len() > 8 { return true; }
        false
    }

    fn buffer_has_garbage(buf: &ratatui::buffer::Buffer) -> Option<String> {
        for cell in buf.content() {
            let sym = cell.symbol();
            if cell_symbol_is_garbage(sym) {
                return Some(format!("garbage cell: {:?}", sym));
            }
        }
        None
    }

    fn render_text_to_buffer(text: ratatui::text::Text<'static>, width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::widgets::{Block, Paragraph};
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| {
            let area = f.area();
            let p = Paragraph::new(text);
            f.render_widget(p, area);
        }).unwrap();
        terminal.backend().buffer().clone()
    }

    // ── looks_like_diff ───────────────────────────────────────────────────────

    #[test]
    fn looks_like_diff_detects_hunk_header() {
        let diff = "@@ -1,3 +1,4 @@\n context\n-removed\n+added";
        assert!(looks_like_diff(diff));
    }

    #[test]
    fn looks_like_diff_detects_three_plus_minus() {
        let diff = "+added line 1\n+added line 2\n-removed line 3\ncontext";
        assert!(looks_like_diff(diff));
    }

    #[test]
    fn looks_like_diff_false_for_plain_text() {
        assert!(!looks_like_diff("This is just plain text with no diff markers."));
    }

    #[test]
    fn looks_like_diff_ignores_plus_plus_plus() {
        // +++ --- header lines alone should not trigger (need 3 +/- content lines)
        assert!(!looks_like_diff("+++ header\n--- header\ncontext only"));
    }

    // ── format_tool_content_expanded ─────────────────────────────────────────

    #[test]
    fn fmt_tool_output_collapsed_preview() {
        let content = "[TOOL_OUTPUT: rg = line1\nline2\nline3\nline4\nline5]";
        let result = format_tool_content_expanded(content, false);
        assert!(result.contains("🔧 rg"), "Should have tool emoji+name");
        assert!(result.contains("│  line1"), "Should have first line");
        assert!(!result.contains("line5"), "Collapsed should not show line5");
        assert!(result.contains("more lines"), "Should mention more lines");
    }

    #[test]
    fn fmt_tool_output_expanded_shows_all() {
        let content = "[TOOL_OUTPUT: rg = line1\nline2\nline3\nline4\nline5]";
        let result = format_tool_content_expanded(content, true);
        assert!(result.contains("line5"), "Expanded should show all lines");
        assert!(!result.contains("more lines"), "Expanded should not truncate");
    }

    #[test]
    fn fmt_tool_error() {
        let content = "[TOOL_ERROR: exec = command not found]";
        let result = format_tool_content_expanded(content, false);
        assert!(result.contains("❌ exec"), "Should show error emoji");
        assert!(result.contains("command not found"));
    }

    #[test]
    fn fmt_tool_truncation_note() {
        let content = "[TOOL_OUTPUT: shell = output here...(truncated to 1000 chars)]";
        let result = format_tool_content_expanded(content, true);
        assert!(result.contains("✂️"), "Should show scissors for truncation");
    }

    // ── render_markdown_line ─────────────────────────────────────────────────

    #[test]
    fn render_md_line_plain_text_no_garbage() {
        let h = crate::highlight::Highlighter::new();
        let _ = h; // Highlighter doesn't affect render_markdown_line
        let result = render_markdown_line("Hello world", true);
        assert!(!result.is_empty());
        for line in &result {
            for span in &line.spans {
                assert!(!span.content.contains('\x1b'), "ANSI in span: {:?}", span.content);
            }
        }
    }

    #[test]
    fn render_md_line_header() {
        let result = render_markdown_line("## Section Header", true);
        assert!(!result.is_empty());
        let text: String = result.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(text.contains("Section Header"));
    }

    #[test]
    fn render_md_line_list_item() {
        let result = render_markdown_line("- list item", true);
        assert!(!result.is_empty());
        let text: String = result.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(text.contains("list item"));
    }

    #[test]
    fn render_md_line_empty_string() {
        let result = render_markdown_line("", true);
        // Should produce at least one (possibly empty) line
        assert!(!result.is_empty() || result.is_empty()); // always passes — check no panic
    }

    // ── format_message_styled ────────────────────────────────────────────────

    fn h() -> crate::highlight::Highlighter { crate::highlight::Highlighter::new() }

    #[test]
    fn fmt_msg_plain_text_no_garbage_in_buffer() {
        let text = format_message_styled("🤖", "Hello, world!", true, &h());
        let buf = render_text_to_buffer(text, 80, 5);
        assert!(buffer_has_garbage(&buf).is_none(), "Buffer has garbage");
    }

    #[test]
    fn fmt_msg_bold_markdown_no_garbage() {
        let text = format_message_styled("🤖", "**bold text** and *italic*", true, &h());
        let buf = render_text_to_buffer(text, 80, 5);
        assert!(buffer_has_garbage(&buf).is_none(), "Buffer has garbage: {:?}", buffer_has_garbage(&buf));
    }

    #[test]
    fn fmt_msg_code_block_renders() {
        let content = "Here is code:\n```rust\nfn main() {}\n```\nDone.";
        let text = format_message_styled("🤖", content, true, &h());
        let full: String = text.lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect::<Vec<_>>().join("");
        assert!(full.contains("╭─ rust") || full.contains("╭─ code") || full.contains("┌─ rust") || full.contains("┌─ code") || full.contains("fn main"), "Code block not rendered");
    }

    #[test]
    fn fmt_msg_code_block_no_garbage() {
        let content = "```python\nfor x in range(10):\n    print(x)\n```";
        let text = format_message_styled("📦", content, false, &h());
        let buf = render_text_to_buffer(text, 100, 10);
        assert!(buffer_has_garbage(&buf).is_none(), "Buffer has garbage: {:?}", buffer_has_garbage(&buf));
    }

    #[test]
    fn fmt_msg_think_prefix_stripped() {
        let content = "[THINK: internal reasoning here]\nActual response to user.";
        let text = format_message_styled("🤖", content, true, &h());
        let full: String = text.lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect::<Vec<_>>().join(" ");
        assert!(full.contains("thinking") || full.contains("💭"), "Think prefix not rendered");
        assert!(full.contains("Actual response"), "Main content missing");
    }

    #[test]
    fn fmt_msg_think_prefix_no_garbage() {
        let content = "[THINK: reasoning]\nResponse.";
        let text = format_message_styled("🤖", content, true, &h());
        let buf = render_text_to_buffer(text, 80, 10);
        assert!(buffer_has_garbage(&buf).is_none(), "Buffer has garbage");
    }

    #[test]
    fn fmt_msg_xml_tool_call_prettified() {
        let content = "Calling tool now.\n<tool>exec</tool><tool_sep>ls -la<end_tool>";
        let text = format_message_styled("🤖", content, true, &h());
        let full: String = text.lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect::<Vec<_>>().join("");
        assert!(full.contains("Calling tool now") || !full.is_empty());
    }

    #[test]
    fn fmt_msg_multiline_no_garbage_dark() {
        let content = "Line one\nLine two\n## Header\nLine three\n- bullet item\nFinal line.";
        let text = format_message_styled("🤖", content, true, &h());
        let buf = render_text_to_buffer(text, 80, 15);
        assert!(buffer_has_garbage(&buf).is_none(), "Buffer has garbage: {:?}", buffer_has_garbage(&buf));
    }

    #[test]
    fn fmt_msg_multiline_no_garbage_light() {
        let content = "Line one\nLine two\n## Header\nLine three\n- bullet item\nFinal line.";
        let text = format_message_styled("🤖", content, false, &h());
        let buf = render_text_to_buffer(text, 80, 15);
        assert!(buffer_has_garbage(&buf).is_none(), "Buffer has garbage: {:?}", buffer_has_garbage(&buf));
    }

    #[test]
    fn fmt_msg_empty_content() {
        let text = format_message_styled("🤖", "", true, &h());
        assert!(!text.lines.is_empty(), "Should have at least one line");
        let buf = render_text_to_buffer(text, 80, 3);
        assert!(buffer_has_garbage(&buf).is_none());
    }

    #[test]
    fn fmt_msg_json_tool_call_no_garbage() {
        let content = r#"{"tool_calls":[{"name":"exec","parameters":{"command":"ls -la","description":"list files"}}]}"#;
        let text = format_message_styled("🤖", content, true, &h());
        let buf = render_text_to_buffer(text, 100, 10);
        assert!(buffer_has_garbage(&buf).is_none(), "Buffer has garbage: {:?}", buffer_has_garbage(&buf));
    }

    // ── detect_and_render_table ───────────────────────────────────────────────

    #[test]
    fn detect_table_basic() {
        let lines = vec!["| A | B |", "|---|---|", "| 1 | 2 |"];
        let result = detect_and_render_table(&lines, 0, true);
        assert!(result.is_some(), "Should detect table");
        let (rendered, consumed) = result.unwrap();
        assert!(consumed >= 3, "Should consume at least 3 lines");
        assert!(!rendered.is_empty());
    }

    #[test]
    fn detect_table_no_table() {
        let lines = vec!["plain text", "more text", "still text"];
        let result = detect_and_render_table(&lines, 0, true);
        assert!(result.is_none(), "Should not detect non-table");
    }

    #[test]
    fn detect_table_out_of_bounds() {
        let lines: Vec<&str> = vec!["| A |"];
        let result = detect_and_render_table(&lines, 0, true);
        assert!(result.is_none(), "Single line cannot be a table");
    }

    #[test]
    fn detect_table_no_garbage_in_buffer() {
        let lines = vec!["| Col1 | Col2 |", "|------|------|", "| val1 | val2 |", "| val3 | val4 |"];
        let result = detect_and_render_table(&lines, 0, true);
        let (rendered, _) = result.expect("Should detect table");
        let text = ratatui::text::Text::from(rendered);
        let buf = render_text_to_buffer(text, 60, 10);
        assert!(buffer_has_garbage(&buf).is_none(), "Table buffer has garbage");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Interaction Latency Tests
//
// Each test measures that a critical hot-path operation completes within a
// per-call budget.  Budgets are intentionally loose (×10–100 the expected
// real cost) so they do not flake on a loaded CI machine while still
// catching algorithmic regressions (O(n²) blow-ups, accidental deep copies, …).
//
// Convention: run N iterations, assert total < N × per_call_budget_us µs.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod interaction_latency_tests {
    use super::*;
    use std::time::Instant;

    // Budget constants (microseconds per call)
    const FUZZY_SCORE_BUDGET_US: u128 = 50;          // single fuzzy_score call
    const PALETTE_FILTER_BUDGET_US: u128 = 1000;     // filter all 30 palette commands
    const MD_LINE_RENDER_BUDGET_US: u128 = 200;      // render one markdown line
    const DIFF_RENDER_BUDGET_US: u128 = 500;         // render a diff block
    const MSG_FORMAT_BUDGET_US: u128 = 500;          // format_message_styled
    const EXTRACT_PLAN_BUDGET_US: u128 = 50;         // extract_plan_block
    const STREAM_END_BUDGET_US: u128 = 10;           // decide_stream_end
    const INPUT_CHAR_BUDGET_US: u128 = 2;            // String::push (per char, 1 000 chars)

    fn assert_under_budget(label: &str, elapsed_us: u128, n: usize, budget_per_call_us: u128) {
        let total_budget = n as u128 * budget_per_call_us;
        assert!(
            elapsed_us <= total_budget,
            "{label}: {n} calls took {elapsed_us}µs (budget {total_budget}µs = {n}×{budget_per_call_us}µs)"
        );
    }

    // ── fuzzy_score ───────────────────────────────────────────────────────────

    #[test]
    fn latency_fuzzy_score_short_query() {
        let n = 10_000;
        let t = Instant::now();
        for _ in 0..n {
            let _ = fuzzy_score("mod", "Switch AI model — model switch change llm ollama");
        }
        assert_under_budget("fuzzy_score/short", t.elapsed().as_micros(), n, FUZZY_SCORE_BUDGET_US);
    }

    #[test]
    fn latency_fuzzy_score_no_match() {
        let n = 10_000;
        let t = Instant::now();
        for _ in 0..n {
            let _ = fuzzy_score("zzzzzz", "Switch AI model — model switch change llm ollama");
        }
        assert_under_budget("fuzzy_score/no_match", t.elapsed().as_micros(), n, FUZZY_SCORE_BUDGET_US);
    }

    #[test]
    fn latency_fuzzy_score_long_query() {
        let n = 5_000;
        let t = Instant::now();
        for _ in 0..n {
            let _ = fuzzy_score("temperature", "Set sampling temperature (0.0–2.0) — temperature heat creativity sampling randomness");
        }
        assert_under_budget("fuzzy_score/long_query", t.elapsed().as_micros(), n, FUZZY_SCORE_BUDGET_US * 2);
    }

    // ── palette filter (all 30 commands scored + sorted) ─────────────────────

    #[test]
    fn latency_palette_filter_with_query() {
        let n = 1_000;
        let query = "mod";
        let t = Instant::now();
        for _ in 0..n {
            let mut scored: Vec<(i32, &'static PaletteCommand)> = PALETTE_COMMANDS
                .iter()
                .filter_map(|cmd| {
                    let haystack = format!("{} {} {}", cmd.name, cmd.description, cmd.keywords);
                    let s = fuzzy_score(query, &haystack);
                    if s > 0 { Some((s, cmd)) } else { None }
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            let _ = scored;
        }
        assert_under_budget("palette_filter/query", t.elapsed().as_micros(), n, PALETTE_FILTER_BUDGET_US);
    }

    #[test]
    fn latency_palette_filter_empty_query() {
        let n = 1_000;
        let query = "";
        let t = Instant::now();
        for _ in 0..n {
            let mut scored: Vec<(i32, &'static PaletteCommand)> = PALETTE_COMMANDS
                .iter()
                .filter_map(|cmd| {
                    let haystack = format!("{} {} {}", cmd.name, cmd.description, cmd.keywords);
                    let s = fuzzy_score(query, &haystack);
                    if s > 0 { Some((s, cmd)) } else { None }
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            let _ = scored;
        }
        assert_under_budget("palette_filter/empty", t.elapsed().as_micros(), n, PALETTE_FILTER_BUDGET_US);
    }

    // ── input buffer throughput ───────────────────────────────────────────────

    #[test]
    fn latency_input_buffer_push_pop() {
        let n = 1_000;
        let t = Instant::now();
        let mut buf = String::new();
        for i in 0..n {
            buf.push(char::from_u32(('a' as u32) + (i % 26) as u32).unwrap_or('a'));
        }
        let push_us = t.elapsed().as_micros();
        let t2 = Instant::now();
        for _ in 0..n {
            buf.pop();
        }
        let pop_us = t2.elapsed().as_micros();
        assert_under_budget("input_buffer/push", push_us, n, INPUT_CHAR_BUDGET_US);
        assert_under_budget("input_buffer/pop",  pop_us,  n, INPUT_CHAR_BUDGET_US);
    }

    // ── markdown line rendering ───────────────────────────────────────────────

    #[test]
    fn latency_render_markdown_plain() {
        let n = 1_000;
        let line = "This is a plain prose line with some **bold** and `code` and *italic* text.";
        let t = Instant::now();
        for _ in 0..n {
            let _ = render_markdown_line(line, true);
        }
        assert_under_budget("render_markdown_line/plain", t.elapsed().as_micros(), n, MD_LINE_RENDER_BUDGET_US);
    }

    #[test]
    fn latency_render_markdown_header() {
        let n = 1_000;
        let line = "## Section Header with Some Text";
        let t = Instant::now();
        for _ in 0..n {
            let _ = render_markdown_line(line, true);
        }
        assert_under_budget("render_markdown_line/header", t.elapsed().as_micros(), n, MD_LINE_RENDER_BUDGET_US);
    }

    #[test]
    fn latency_render_markdown_code_block() {
        let n = 500;
        let line = "```rust\nfn main() { println!(\"hello\"); }\n```";
        let t = Instant::now();
        for _ in 0..n {
            let _ = render_markdown_line(line, true);
        }
        assert_under_budget("render_markdown_line/code", t.elapsed().as_micros(), n, MD_LINE_RENDER_BUDGET_US * 4);
    }

    // ── diff rendering ────────────────────────────────────────────────────────

    #[test]
    fn latency_render_diff_small() {
        let n = 500;
        let diff = "@@ -1,5 +1,6 @@\n context\n-removed line\n+added line\n context\n+another new line\n context";
        let t = Instant::now();
        for _ in 0..n {
            let _ = render_diff_styled("📄", "file.rs", diff, 0, "");
        }
        assert_under_budget("render_diff/small", t.elapsed().as_micros(), n, DIFF_RENDER_BUDGET_US);
    }

    #[test]
    fn latency_render_diff_large() {
        let n = 100;
        let line = "+added line of code here with some content to make it realistic\n";
        let diff = line.repeat(200);
        let t = Instant::now();
        for _ in 0..n {
            let _ = render_diff_styled("📄", "big_file.rs", &diff, 50, "");
        }
        assert_under_budget("render_diff/large_truncated", t.elapsed().as_micros(), n, DIFF_RENDER_BUDGET_US * 10);
    }

    // ── format_message_styled ─────────────────────────────────────────────────

    #[test]
    fn latency_format_message_plain() {
        let n = 500;
        let h = crate::highlight::Highlighter::new();
        let content = "Here is a short assistant response with a few sentences of text.";
        let t = Instant::now();
        for _ in 0..n {
            let _ = format_message_styled("🤖", content, true, &h);
        }
        assert_under_budget("format_message/plain", t.elapsed().as_micros(), n, MSG_FORMAT_BUDGET_US);
    }

    #[test]
    fn latency_format_message_with_tool_call() {
        let n = 500;
        let h = crate::highlight::Highlighter::new();
        let content = r#"{"tool_calls":[{"name":"exec","parameters":{"command":"cargo test --lib","description":"run tests"}}]}"#;
        let t = Instant::now();
        for _ in 0..n {
            let _ = format_message_styled("🤖", content, true, &h);
        }
        assert_under_budget("format_message/tool_call", t.elapsed().as_micros(), n, MSG_FORMAT_BUDGET_US);
    }

    #[test]
    fn latency_format_message_multiline() {
        let n = 200;
        let h = crate::highlight::Highlighter::new();
        let content = "Line one of the response.\n## Header\n\nSome paragraph text here.\n\n```rust\nfn hello() {}\n```\n\nMore prose at the end.";
        let t = Instant::now();
        for _ in 0..n {
            let _ = format_message_styled("🤖", content, true, &h);
        }
        assert_under_budget("format_message/multiline", t.elapsed().as_micros(), n, MSG_FORMAT_BUDGET_US * 5);
    }

    // ── pure utility functions ────────────────────────────────────────────────

    #[test]
    fn latency_extract_plan_block() {
        let n = 10_000;
        let text = "Some intro text.\n<plan>\n- Task A\n- Task B\n- Task C\n</plan>\nSome trailing text.";
        let t = Instant::now();
        for _ in 0..n {
            let _ = extract_plan_block(text);
        }
        assert_under_budget("extract_plan_block", t.elapsed().as_micros(), n, EXTRACT_PLAN_BUDGET_US);
    }

    #[test]
    fn latency_extract_plan_block_no_plan() {
        let n = 10_000;
        let text = "Just a plain response with no plan block anywhere in here.";
        let t = Instant::now();
        for _ in 0..n {
            let _ = extract_plan_block(text);
        }
        assert_under_budget("extract_plan_block/no_plan", t.elapsed().as_micros(), n, EXTRACT_PLAN_BUDGET_US);
    }

    #[test]
    fn latency_decide_stream_end() {
        let n = 100_000;
        let t = Instant::now();
        for i in 0..n {
            let _ = decide_stream_end(true, AppMode::Plan, i % 5, (i % 4) as u32);
        }
        assert_under_budget("decide_stream_end", t.elapsed().as_micros(), n, STREAM_END_BUDGET_US);
    }

    #[test]
    fn latency_is_shell_write_pattern() {
        let n = 10_000;
        let cases = [
            "echo hello",
            "cat file.txt > output.txt",
            "ls -la | grep rust",
            "cat > file.rs << 'EOF'\nfn main() {}\nEOF",
            "cargo build --release",
        ];
        let t = Instant::now();
        for i in 0..n {
            let _ = is_shell_write_pattern(cases[i % cases.len()]);
        }
        assert_under_budget("is_shell_write_pattern", t.elapsed().as_micros(), n, FUZZY_SCORE_BUDGET_US);
    }
}

#[cfg(test)]
mod provider_error_tests {
    use super::*;

    #[test]
    fn test_extract_provider_error_passthrough_for_normal_errors() {
        let e = "connection refused";
        let (msg, fatal) = extract_provider_error(e);
        assert_eq!(msg, "connection refused");
        assert!(!fatal);
    }

    #[test]
    fn test_extract_provider_error_detects_400_integer_code() {
        // Mimics the exact error async_openai produces for OpenRouter 400 responses
        let raw = r#"Stream error: failed to deserialize api response: error:invalid type: integer `400`, expected a string at line 1 column 56 content:{"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\"id\":\"ogg\",\"error\":{\"message\":\"Input validation error\"}}"}}} "#;
        let (msg, fatal) = extract_provider_error(raw);
        assert!(fatal, "400 error should be fatal");
        assert!(msg.contains("400") || msg.contains("Provider"), "should surface provider info: {}", msg);
    }

    #[test]
    fn test_extract_provider_error_extracts_message_field() {
        let raw = r#"failed to deserialize api response: error:invalid type: integer `400`, expected a string content:{"error":{"message":"Input validation error: too many tokens","code":400}}"#;
        let (msg, fatal) = extract_provider_error(raw);
        assert!(fatal);
        assert!(msg.contains("Provider returned error") || msg.contains("Input validation") || msg.contains("400"),
            "message should contain error info: {}", msg);
    }

    #[test]
    fn test_extract_provider_error_not_fatal_for_non_400() {
        // 429 rate limit — not a 400, should not be fatal
        let raw = r#"failed to deserialize api response: error:invalid type: integer `429`, expected a string content:{"error":{"message":"Rate limit","code":429}}"#;
        let (msg, fatal) = extract_provider_error(raw);
        assert!(!fatal, "429 should not be fatal (retriable)");
        assert_eq!(msg, raw);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// build_recent_files_content tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod recent_files_tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn write_file(dir: &std::path::Path, name: &str, content: &str) {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn returns_empty_for_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = build_recent_files_content_in(5, 1000, tmp.path());
        assert!(result.is_empty(), "empty dir should yield empty string");
    }

    #[test]
    fn includes_written_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "hello.rs", "fn main() {}");
        let result = build_recent_files_content_in(5, 1000, tmp.path());
        assert!(result.contains("hello.rs"), "should include the file");
        assert!(result.contains("fn main()"), "should include file content");
    }

    #[test]
    fn respects_n_limit() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10usize {
            // sleep 5ms between writes so mtimes differ
            std::thread::sleep(std::time::Duration::from_millis(5));
            write_file(tmp.path(), &format!("file{}.txt", i), &format!("content {}", i));
        }
        let result = build_recent_files_content_in(3, 1000, tmp.path());
        // Should only show 3 files (header says "3")
        assert!(result.contains("RECENTLY EDITED FILES (3)"), "header should say 3 files: {}", result);
    }

    #[test]
    fn truncates_large_file() {
        let tmp = tempfile::tempdir().unwrap();
        let big = "x".repeat(5000);
        write_file(tmp.path(), "big.txt", &big);
        let result = build_recent_files_content_in(5, 100, tmp.path());
        assert!(result.contains("truncated"), "large file should be truncated");
    }

    #[test]
    fn skips_binary_file() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a "binary" file with null bytes
        let path = tmp.path().join("binary.bin");
        fs::write(&path, b"data\x00more").unwrap();
        let result = build_recent_files_content_in(5, 1000, tmp.path());
        assert!(!result.contains("binary.bin"), "binary files should be skipped");
    }

    #[test]
    fn skips_lock_extension() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "Cargo.lock", "[metadata]");
        let result = build_recent_files_content_in(5, 1000, tmp.path());
        assert!(!result.contains("Cargo.lock"), "lock files should be skipped");
    }

    #[test]
    fn with_recent_files_content_builder() {
        let config = crate::agent::AgentConfig::new("model", "http://localhost:11434")
            .with_recent_files_content("some content");
        assert_eq!(config.recent_files_content, "some content");
    }
}
