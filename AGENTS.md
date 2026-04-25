# yggdra

You are working on **yggdra** — an airgapped agentic TUI written in Rust.
It connects to a local Ollama instance and lets an LLM use tools to work on
files without internet access.

## Build & test

```sh
cargo build --release      # release binary → target/release/yggdra
cargo test --lib           # 450 tests, must stay green
make install               # copies binary to ~/.local/bin/yggdra
```

Always run `cargo test --lib` after any change. Do not leave tests failing.
After completing a task, run `make install` to ensure the updated binary is available in the path.

## Architecture

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point: session init, Ollama connect, launch App, CLI arg parsing |
| `src/ui.rs` | TUI app (~6540 lines): event loop, rendering, command dispatch |
| `src/agent.rs` | Agentic loop: tool calls, subagent spawning, steering, JSON/Qwen/Gemma format parsing |
| `src/spawner.rs` | Hierarchical subagent execution (max depth 10) |
| `src/tools.rs` | Tool implementations: rg, exec, editfile, setfile, commit, shell, python, ruste, spawn |
| `src/notifications.rs` | Native OS notifications (macOS via osascript) |
| `src/watcher.rs` | Filesystem watching for live config reload |
| `src/knowledge_index.rs` | Offline doc indexing for the knowledge base |
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

- **Tool format**: JSON tool calling is the default (OpenWebUI-style). Legacy formats kept for compat:
  - Qwen/Gemma: `<|tool>name<|tool_sep>arg1<|tool_sep>arg2<|end_tool>`
  - Bracket: `[TOOL: name args]`
  - Parser: `agent::parse_tool_calls()` handles all three; takes a `CapabilityProfile`
- Subagent spawns: `spawn` tool (renamed from `spawn_agent`); shell/process exec is `exec` (renamed from `spawn`)
- Tool results injected as: `[TOOL_OUTPUT: name = result]`
- Completion signal: `[DONE]`
- Session data lives in `~/.yggdra/sessions/<uuid>/`
- Per-project data lives in `.yggdra/` (log/, gaps, session marker, todo/*, knowledge -> offlinebase)
- **Todos:** discoverable markdown files in `.yggdra/todo/` — see `.yggdra/todo/README.md`
- **Knowledge base:** symlink `.yggdra/knowledge` → `~/source/repos/offlinebase` (135,000+ offline docs)

## CLI Flags

```bash
yggdra --ask       # Ask mode: read-only, agent only answers questions
yggdra --plan      # Plan mode: interactive (default)
yggdra --build     # Build mode: autonomous execution, agent kicks itself
yggdra --one       # One mode: like build, but stops + notifies when task is done
yggdra --shell     # Shell-only capability profile (shell + setfile + commit)
yggdra --help      # Show available options
```

Mode is saved to `.yggdra/config.json` and persists between launches.
Mode cycle order in the UI: Plan → Build → One → Ask → Plan.

## Slash commands (selected)

- `/one` — switch to One mode for a single autonomous task
- `/abort` — kill stuck streams, async tasks, and tool execution
- `/shell` — switch to ShellOnly capability profile
- `/test_notification` — fire a test OS notification (verify macOS setup)
- `/help`, `/models` — see in-app help

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

- **No internet**: the `exec` tool blocks `/bin/`, `/usr/bin/`, `/usr/sbin/`
- **No shell injection**: tool args are validated before execution
- **No network code generation**: steering directives explicitly forbid it
- `cargo test --lib` must pass before any commit

## Common gotchas

- `OllamaClient` derives `Clone` (reqwest::Client is Arc-backed — safe)
- Module named `msglog` not `log` (conflicts with Rust std log crate)
- `exec` resolves binaries via PATH — bare names like `ls`, `cat` work fine
- `execute_simple()` is for subagents: identical to `execute_with_tools()` but no spawning (prevents recursive async futures)
- `cached_message_count` must be updated after every `add_and_persist` call or the UI won't redraw
