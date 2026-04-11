# Changelog

All notable changes to Yggdra will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

### Planned for v0.2.0

- [ ] Plugin system for custom tools
- [ ] Agent mode with autonomous tool execution
- [ ] Output syntax highlighting for code blocks
- [ ] Streaming responses from Ollama
- [ ] Cross-machine session sync (via git or cloud storage)
- [ ] Config file support (TOML)
- [ ] Command history and autocomplete

### Under Consideration

- [ ] Web UI version (Leptos)
- [ ] Mobile companion app
- [ ] Integration with other LLMs (LM Studio, Vllm)
- [ ] Fine-tuning workflow support
- [ ] Persistent undo/redo for messages
- [ ] Template library for common prompts

---

For questions, bug reports, or feature requests, please open an issue on GitHub.
