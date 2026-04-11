# yggdra

You are working on **yggdra** — an airgapped agentic TUI written in Rust.
It connects to a local Ollama instance and lets an LLM use tools to work on
files without internet access.

## Build & test

```sh
cargo build --release      # release binary → target/release/yggdra
cargo test --lib           # 40 tests, must stay green
make install               # copies binary to ~/.local/bin/yggdra
```

Always run `cargo test --lib` after any change. Do not leave tests failing.

## Architecture

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point: session init, Ollama connect, launch App, CLI arg parsing |
| `src/ui.rs` | TUI app (~1250 lines): event loop, rendering, command dispatch |
| `src/agent.rs` | Agentic loop: tool calls, subagent spawning, steering, Qwen/Gemma format parsing |
| `src/spawner.rs` | Hierarchical subagent execution (max depth 10) |
| `src/tools.rs` | Tool implementations: rg, spawn, editfile, commit, python, ruste |
| `src/ollama.rs` | Ollama HTTP client (streaming + non-streaming) |
| `src/message.rs` | SQLite-backed message buffer + scrollback |
| `src/msglog.rs` | Async `.yggdra/log/YYYY/MM/DD/HHMM/SS-role.md` writer |
| `src/gaps.rs` | Knowledge gap detection via model self-reflection |
| `src/task.rs` | SQLite task tracking, checkpoints, dependency graph |
| `src/session.rs` | Per-directory session via `.yggdra_session_id` marker |
| `src/steering.rs` | Steering directive injection |
| `src/config.rs` | Config (model, endpoint, mode) with mode persistence |
| `src/metrics.rs` | Project completion metrics tracking and estimation |

## Key conventions

- **Tool format**: `<|tool>name<|tool_sep>arg1<|tool_sep>arg2<|end_tool>` (Qwen/Gemma format)
  - Also supports legacy format: `[TOOL: name args]` (for backward compatibility)
  - Parser: `agent::parse_tool_calls()` handles both formats
- Subagent spawns: `<|tool>spawn_agent<|tool_sep>task_id<|tool_sep>description<|end_tool>`
- Tool results injected as: `[TOOL_OUTPUT: name = result]`
- Completion signal: `[DONE]`
- Session data lives in `~/.yggdra/sessions/<uuid>/`
- Per-project data lives in `.yggdra/` (log/, gaps, session marker, todo/*, knowledge -> offlinebase)
- **Todos:** discoverable markdown files in `.yggdra/todo/` — see `.yggdra/todo/README.md`
- **Knowledge base:** symlink `.yggdra/knowledge` → `~/source/repos/offlinebase` (135,000+ offline docs)

## CLI Flags

```bash
yggdra --ask       # Start in ask-only mode (read-only, no file modifications)
yggdra --build     # Start in build mode (autonomous execution)
yggdra --plan      # Start in plan mode (interactive, default)
yggdra --help      # Show available options
```

Mode is saved to `.yggdra/config.json` and persists between launches.

## Knowledge Base Access

Agents can search the offline knowledge base at `.yggdra/knowledge` (symlink to `~/source/repos/offlinebase`):

```bash
# Search Rust docs
<|tool>rg<|tool_sep>async|trait|lifetime<|tool_sep>.yggdra/knowledge/rust/<|end_tool>

# Search Godot tutorials
<|tool>rg<|tool_sep>Node3D|physics<|tool_sep>.yggdra/knowledge/godot/<|end_tool>

# List categories
<|tool>spawn<|tool_sep>ls<|tool_sep>.yggdra/knowledge/<|end_tool>

# Read a specific doc
<|tool>editfile<|tool_sep>.yggdra/knowledge/README.md<|end_tool>
```

The knowledge base contains 135,000+ files across 73 categories: spacecraft systems, programming (Rust/Python), graphics (Godot), life support, navigation, and reference materials.

## Constraints — never break these

- **No internet**: the `spawn` tool blocks `/bin/`, `/usr/bin/`, `/usr/sbin/`
- **No shell injection**: tool args are validated before execution
- **No network code generation**: steering directives explicitly forbid it
- `cargo test --lib` must pass before any commit

## Common gotchas

- `OllamaClient` derives `Clone` (reqwest::Client is Arc-backed — safe)
- Module named `msglog` not `log` (conflicts with Rust std log crate)
- `spawn` resolves binaries via PATH — bare names like `ls`, `cat` work fine
- `execute_simple()` is for subagents: identical to `execute_with_tools()` but no spawning (prevents recursive async futures)
- `cached_message_count` must be updated after every `add_and_persist` call or the UI won't redraw
