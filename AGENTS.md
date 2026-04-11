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
| `src/main.rs` | Entry point: session init, Ollama connect, launch App |
| `src/ui.rs` | TUI app (~1250 lines): event loop, rendering, command dispatch |
| `src/agent.rs` | Agentic loop: tool calls, subagent spawning, steering |
| `src/spawner.rs` | Hierarchical subagent execution (max depth 10) |
| `src/tools.rs` | Tool implementations: rg, spawn, editfile, commit, python, ruste |
| `src/ollama.rs` | Ollama HTTP client (streaming + non-streaming) |
| `src/message.rs` | SQLite-backed message buffer + scrollback |
| `src/msglog.rs` | Async `.yggdra/log/YYYY/MM/DD/HHMM/SS-role.md` writer |
| `src/gaps.rs` | Knowledge gap detection via model self-reflection |
| `src/task.rs` | SQLite task tracking, checkpoints, dependency graph |
| `src/session.rs` | Per-directory session via `.yggdra_session_id` marker |
| `src/steering.rs` | Steering directive injection |
| `src/config.rs` | Config (model, endpoint, theme) |

## Key conventions

- Tool calls in model output: `[TOOL: name args]` — parsed by `agent::parse_tool_calls()`
- Subagent spawns: `[TOOL: spawn_agent task_id "description"]`
- Tool results injected as: `[TOOL_OUTPUT: name = result]`
- Completion signal: `[DONE]`
- Session data lives in `~/.yggdra/sessions/<uuid>/`
- Per-project data lives in `.yggdra/` (log/, gaps, session marker, todo/*, knowledge -> offlinebase)
- **Todos:** discoverable markdown files in `.yggdra/todo/` — see `.yggdra/todo/README.md`
- **Knowledge base:** symlink `.yggdra/knowledge` → `~/source/repos/offlinebase` (135,000+ offline docs)

## UI commands

`/checkpoint NAME`, `/clear`, `/tasks`, `/gaps`, `/tool mem QUERY`, `/models`, `/help`

## Knowledge Base Access

Agents can search the offline knowledge base at `.yggdra/knowledge` (symlink to `~/source/repos/offlinebase`):

```bash
# Search Rust docs
[TOOL: rg "async|trait|lifetime" .yggdra/knowledge/rust/]

# Search Godot tutorials
[TOOL: rg "Node3D|physics" .yggdra/knowledge/godot/]

# List categories
[TOOL: spawn ls .yggdra/knowledge/]

# Read a specific doc
[TOOL: editfile .yggdra/knowledge/README.md]
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
