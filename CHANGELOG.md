# Changelog

All notable changes to Yggdra will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.5] - 2026-04-26

### Added

- **Session notes persistence**: `/compress` now writes the summary to
  `.yggdra/session_notes.md`. On startup, if that file exists, its contents
  are injected into the system prompt under `### SESSION NOTES` so the agent
  retains context across session restarts without re-reading the full history.
- **Untracked file detection in shell tool**: `git diff` is silent for new
  untracked files, causing the previous stale-diff fallback to show the last
  commit instead — confusing the agent into infinite retry loops (e.g. writing
  the same file 10+ times). The shell tool now runs
  `git ls-files --others --exclude-standard` and surfaces new files as
  `new files: src/foo.rs` in the tool output. The `git show HEAD` fallback is
  now scoped to commands that contain `git commit/push/merge/rebase`.

### Fixed

- **`<commit_message>` tag accepted**: The XML parser for the `commit` tool
  only accepted `<message>` but models frequently emit `<commit_message>`.
  Both tags are now accepted, preventing every agent commit attempt from
  failing with "empty commit message".
- **OpenRouter 400 errors no longer trigger retry loops**: When OpenRouter
  returns a 400 (e.g. context too long), `async_openai` fails to deserialize
  the response because the `code` field is an integer rather than a string.
  This previously produced a cryptic error that was treated as transient and
  retried up to 4 times before the circuit breaker fired. The new
  `extract_provider_error()` helper detects this pattern, extracts the
  human-readable message from the embedded JSON, and pauses immediately with
  `"⏸️ Provider error (will not retry): …"`.
- **macOS `/theme auto` now correctly detects dark mode**: Detection fell
  through to the `COLORFGBG` environment variable when `defaults read -g
  AppleInterfaceStyle` failed (e.g. sandbox, spawn error), returning the wrong
  theme. The macOS path now uses `osascript` (queries the live appearance API,
  handles auto-appearance and MDM) as the primary method, with `defaults` as a
  fallback, and never falls through to `COLORFGBG` on macOS.
- **`setfile` commit messages now use relative paths**: Previously committed as
  `setfile: /Users/name/source/repos/project/src/main.rs`; now
  `setfile: src/main.rs`.

### Tests

- 712 tests total (up from 706); 6 new tests covering untracked file
  detection, commit tag alias, and provider error extraction.

---

## [0.2.4] - 2026-04-25

### Added

- **async-openai integration**: OpenAI/OpenRouter backend now uses the
  `async-openai` library with the `byot` feature for proper SSE streaming.
  Replaces the hand-rolled reqwest streaming path.
- **Terminal integrity tests**: 35 tests using ratatui `TestBackend` for
  cell-level garbage detection — catches rendering corruption that unit tests
  miss.
- **Rendering pipeline tests**: 27 tests covering `format_message_styled`,
  `format_tool_content_expanded`, `render_markdown_line`,
  `detect_and_render_table`, `prettify_tool_calls`, and `looks_like_diff`.
- **Interaction latency tests**: 18 tests asserting per-call budgets for fuzzy
  score, palette filter, input buffer, markdown/diff rendering, and message
  formatting. Total test count: **472**.
- **Think auto-detection**: `OllamaClient` queries `/api/show` capabilities on
  connect and auto-enables `think: true` only for models that support it (e.g.
  qwen3.5-thinking). Non-thinking models are never sent `think: true`, avoiding
  the `"model does not support thinking"` error.
- **ESC partial save**: Pressing ESC mid-inference now saves the partial
  response as an assistant message with an `<esc/>` marker so the model retains
  context of the interrupted turn.

### Changed

- `/color` command is now async/non-blocking — no longer freezes the TUI
  event loop while cycling themes.
- Rendering pipeline: 7 rendering functions extracted from `impl App` to
  `pub(crate)` free functions for testability and reduced coupling.
- **Header removed, status bar consolidated**: The top "Status" header is gone.
  Mode, connection icon, model name, context usage, inference rate, message
  count, and endpoint are all shown in the bottom status bar with responsive
  width tiers (≥80 / ≥50 / ≥30 / minimal).
- `/endpoint` with no argument now resets the endpoint to
  `http://localhost:11434`.
- All Ollama/OpenAI timeouts set to infinite — no more mid-inference timeouts
  on slow or large models.
- Outside-inference warning banner is now lowercase.

### Removed

- JSON tests removed from the model gauntlet (`test_models`); only XML,
  discipline, and humor benchmarks remain.
- Redundant documentation: `MARKDOWN_IMPLEMENTATION_SUMMARY.md`,
  `MARKDOWN_RENDERING.md`, `IMPLEMENTATION_NOTES.md` consolidated and
  removed (session artifacts with overlapping content).

---

## [0.2.1] - 2026-04-21

### Added

- Global `~/AGENTS.md` support: loaded on every startup and prepended to the
  project-local `AGENTS.md`. Both files are live-watched — edits apply without
  restart. Useful for personal preferences, persona, or cross-project constraints.
- `merge_agents_md()` helper in the library crate with 4 unit tests.

## [0.2.0] - 2026-04-21

### Added

- `--one` mode for one-off tasks (autonomous + completion notification)
- `setfile` tool: full-file overwrites (replaces editfile for whole-file rewrites; git-tracked)
- `/abort` command: kill stuck streams, async tasks, and tool execution
- `/test_notification` command: manually fire a test OS notification
- `/shell` command and `--shell` capability profile (ShellOnly: agent restricted to shell + setfile + commit)
- `notifications` module: native OS notifications (model done, errors, agent_says, task complete)
- `watcher` module: live config reload via filesystem watching (`.yggdra/config.json`, `AGENTS.md`)
- `knowledge_index` module: offline doc indexing
- `battery`, `metrics`, `sysinfo`, `theme`, `epoch`, `dlog`, `stats`, `highlight`, `spawner` modules
- 237 new tests (27 → 264)
- Mode persistence to config (`~/.yggdra/config.json`)
- Filesystem watching for live config reload

### Changed

- macOS notifications now use `osascript` (notify-rust silently fails on unbundled CLIs)
- Tool format default switched to JSON tool calling (OpenWebUI-style); legacy `<|tool>` and `[TOOL:]` formats kept for compat
- `spawn` tool renamed to `exec`; subagent spawn renamed to `spawn`
- Mode cycling order: Plan → Build → One → Ask → Plan
- `parse_tool_calls()` now takes `CapabilityProfile`

### Fixed

- Ask mode no longer continues autonomously after tool results / async task completion
- Think panel duplication when both native ThinkToken events and inline `<think>` tags arrived
- Render cache invalidation during streaming (thinking text now updates live)
- Tool output truncation threshold raised 500 → 600 chars

---

## [0.1.0] - 2024-04-11

### MVP Release

**Yggdra v0.1.0 is the initial MVP release - a fully functional airgapped agentic TUI for local language model inference with integrated tool execution.**

### Added

#### Core Features
- **Local LLM Inference**: Full integration with Ollama API for chat-based inference
- **Session Management**: Per-directory sessions that persist across restarts
- **Multi-Window Sync**: Multiple Yggdra instances in same directory automatically sync via SQLite
- **Tool Execution**: Execute local tools (ripgrep, git, bash) directly from TUI
- **Error Handling**: User-friendly error messages with context and recovery suggestions
- **Steering Directives**: System-level constraint injection for consistent model behavior

#### Terminal UI
- **Clean Minimal Interface**: Built with Ratatui for responsive, distraction-free experience
- **Real-time Status**: Connection status, session info, message count, keybindings
- **Input Validation**: Command format checking and message length limits
- **Loading Indicators**: Visual feedback during Ollama requests and tool execution
- **Scrollable Messages**: Conversation history with emoji role indicators

#### Storage & Performance
- **SQLite Backend**: Transactional message storage with <16ms query times
- **Message Compression**: Hierarchical summarization for context window management
- **JSONL Export**: Portable message history format
- **Optimized Build**: Release binary ~3.5 MB with LTO and optimizations

#### Commands
- `/help` - Display all commands and keybindings
- `/models` - List available Ollama models
- `/tool CMD` - Execute local tools (ripgrep, git, bash scripts)
- Free text - Send message to Ollama

#### Configuration
- Environment variables: `OLLAMA_ENDPOINT`, `OLLAMA_MODEL`
- Smart model detection (last loaded model from Ollama)
- Sensible defaults for airgapped environments

#### Documentation
- **README.md**: Quick start, installation, usage guide, troubleshooting
- **ARCHITECTURE.md**: Deep dive into design, data flow, performance characteristics
- **CONTRIBUTING.md**: Development setup, code style, adding tools, testing

#### Testing
- 27 unit tests covering message buffer, Ollama client, session management
- 8 integration tests for full workflows
- Performance benchmarks validated
- Cross-platform testing (Linux, macOS)

### Technical Details

#### Dependencies
- **Tokio**: Async runtime
- **Ratatui**: TUI framework
- **Crossterm**: Terminal control
- **Reqwest**: HTTP client for Ollama
- **Rusqlite**: SQLite bindings
- **Serde**: JSON serialization

#### Known Limitations

1. **SQLite Limitations**:
   - Not recommended for 100+ concurrent writers
   - Not suitable for network filesystems (NFS, SMB)
   - Single-machine only (no replication)

2. **Model Constraints**:
   - Works with any Ollama model
   - Steering directives may be ignored by some model families
   - Context window limited by model and message buffer

3. **Tool Execution**:
   - 30-second timeout per tool execution
   - Output limited to first 10 KB
   - Executed in current directory context

4. **Performance**:
   - Tested up to 100+ messages in single session
   - Optimal performance on SSD storage
   - Network latency affects Ollama requests

### Breaking Changes

None (initial release)

### Deprecations

None (initial release)

### Security

- No external network calls (fully airgapped)
- No API keys or credentials stored
- Input validation prevents common injection attacks
- File operations isolated to session directory

### Performance

- **Binary Size**: 3.5 MB (stripped, LTO enabled)
- **Memory**: ~50 MB base + message buffer
- **Message Query**: <16ms for 100 messages
- **UI Render**: ~8-12ms per frame
- **Tool Execution**: 50-500ms typical

### Acknowledgments

Built with Rust, leveraging the excellent Ollama, Ratatui, and SQLite projects.

---

## Future Releases

### Under Consideration

- Web UI version (Leptos)
- Mobile companion app
- Integration with other LLMs (LM Studio, Vllm)
- Fine-tuning workflow support
- Persistent undo/redo for messages
- Template library for common prompts

---

For questions, bug reports, or feature requests, please open an issue on GitHub.
