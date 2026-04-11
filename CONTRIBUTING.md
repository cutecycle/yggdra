# Contributing to Yggdra

Thank you for your interest in contributing to Yggdra! This guide covers development setup, code style, and contribution process.

## Table of Contents

1. [Development Setup](#development-setup)
2. [Code Organization](#code-organization)
3. [Code Style](#code-style)
4. [Adding New Tools](#adding-new-tools)
5. [Writing Tests](#writing-tests)
6. [Building & Testing](#building--testing)
7. [Submitting Changes](#submitting-changes)

## Development Setup

### Prerequisites

- **Rust 1.70+** (install from https://rustup.rs/)
- **Ollama** (v0.1.0+) running locally
- **Git**
- **ripgrep** (optional, for tool examples)

### Clone & Build

```bash
git clone https://github.com/cutecycle/yggdra.git
cd yggdra

# Build development version
cargo build

# Run tests
cargo test

# Run the binary
./target/debug/yggdra
```

### Development Workflow

```bash
# 1. Create feature branch
git checkout -b feature/my-feature

# 2. Make changes and test
cargo test
cargo build

# 3. Check code quality
cargo clippy -- -D warnings
cargo fmt -- --check

# 4. Commit with conventional message
git commit -m "feat: add new feature

Description of changes.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"

# 5. Push and open PR
git push origin feature/my-feature
```

## Code Organization

### Module Structure

```
src/
├── main.rs           # Entry point, initialization
├── lib.rs            # Library exports
├── ui.rs             # Terminal UI (Ratatui)
├── config.rs         # Configuration loading
├── session.rs        # Session management
├── message.rs        # Message buffer & storage
├── ollama.rs         # Ollama API client
├── steering.rs       # Steering directive system
├── tools.rs          # Tool execution (if present)
├── agent.rs          # Agent orchestration (if present)
└── notifications.rs  # Desktop notifications
```

### Module Responsibilities

| Module | Purpose | Key Types |
|--------|---------|-----------|
| `ui` | Terminal rendering & events | `App`, events |
| `message` | Message storage & compression | `Message`, `MessageBuffer` |
| `ollama` | LLM inference | `OllamaClient`, `ModelInfo` |
| `session` | Session persistence | `Session` |
| `config` | Environment configuration | `Config` |
| `steering` | System prompt injection | `SteeringDirective` |

## Code Style

### Rust Style Guidelines

**Follow the Rust community standards:**

1. **Formatting**
   ```bash
   # Format code
   cargo fmt
   
   # Check formatting
   cargo fmt -- --check
   ```

2. **Linting**
   ```bash
   # Check clippy warnings
   cargo clippy -- -D warnings
   ```

3. **Naming Conventions**
   - Functions/variables: `snake_case`
   - Types/traits: `PascalCase`
   - Constants: `UPPER_CASE`

### Documentation

**Add doc comments to public items:**

```rust
/// Brief description of what this function does.
///
/// More detailed explanation of the behavior, including:
/// - Expected inputs
/// - Return value
/// - Error conditions
///
/// # Examples
/// ```
/// let result = my_function(arg);
/// assert_eq!(result, expected);
/// ```
pub fn my_function(arg: String) -> Result<String> {
    // Implementation
}
```

### Error Handling

**Use `anyhow::Result<T>` for fallible operations:**

```rust
use anyhow::{anyhow, Result};

// Good: Provides context
fn do_something() -> Result<String> {
    let file = std::fs::read_to_string("config.json")
        .map_err(|e| anyhow!("Failed to read config: {}", e))?;
    Ok(file)
}

// Better: More specific error
fn do_something() -> Result<String> {
    let file = std::fs::read_to_string("config.json")
        .map_err(|e| anyhow!("Config file not found at config.json: {}", e))?;
    Ok(file)
}
```

### Comments

**Write comments for "why", not "what":**

```rust
// ❌ Bad: Restates the code
self.counter = self.counter + 1; // increment counter

// ✅ Good: Explains intent
// Increment retry counter; will stop trying after MAX_RETRIES
self.counter = self.counter + 1;

// ✅ Good: Explains non-obvious logic
// Use exponential backoff to avoid overwhelming Ollama
let delay_ms = 100 * 2_u64.pow(self.counter);
```

## Adding New Tools

### Overview

Tools are commands that Yggdra can execute locally (ripgrep, git, bash, etc.).

### Adding a Tool Command

1. **Update the UI handler** in `src/ui.rs`:

```rust
async fn handle_tool_command(&mut self, command: &str) {
    let tool_args = command.strip_prefix("/tool ").unwrap_or("").trim();
    
    // Validate tool syntax
    if tool_args.is_empty() {
        self.status_message = "❌ No tool specified".to_string();
        return;
    }
    
    // Execute tool
    let result = execute_tool(tool_args).await;
    
    // Handle result and persist
    match result {
        Ok(output) => {
            let msg = Message::new("tool", output);
            let _ = self.message_buffer.add_and_persist(msg);
        }
        Err(e) => {
            self.status_message = format!("❌ Tool error: {}", e);
        }
    }
}
```

2. **Implement tool execution** (or use existing `execute_tool`):

```rust
async fn execute_tool(args: &str) -> Result<String> {
    // Parse and validate args
    let cmd_parts: Vec<&str> = args.split_whitespace().collect();
    let tool_name = cmd_parts.first().ok_or_else(|| anyhow!("No tool"))?;
    
    // Match known tools or shell out
    match *tool_name {
        "rg" | "grep" => execute_search_tool(&cmd_parts[1..]).await,
        "git" => execute_git_tool(&cmd_parts[1..]).await,
        _ => Err(anyhow!("Unknown tool: {}", tool_name)),
    }
}
```

3. **Add tests** in `tests/` directory:

```rust
#[tokio::test]
async fn test_rg_tool_execution() {
    let result = execute_tool("rg 'pattern' .").await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(!output.is_empty());
}
```

### Tool Guidelines

- **Safety First**: Validate inputs, prevent shell injection
- **Timeout**: Limit execution to <30 seconds
- **Output**: Capture and truncate large outputs (>10 KB)
- **Error Messages**: User-friendly, not technical stack traces
- **Testing**: Include tests for normal cases and error cases

### Example: Git Status Tool

```rust
async fn execute_git_tool(args: &[&str]) -> Result<String> {
    // Validate command is safe
    if args.is_empty() {
        return Err(anyhow!("git requires a subcommand"));
    }
    
    // Whitelist allowed git commands
    match args[0] {
        "log" | "status" | "diff" | "branch" => {},
        _ => return Err(anyhow!("git command not allowed: {}", args[0])),
    }
    
    // Execute with timeout
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("git")
            .args(args)
            .output()
    )
    .await
    .map_err(|_| anyhow!("git command timeout"))?
    .map_err(|e| anyhow!("Failed to execute git: {}", e))?;
    
    // Return output
    String::from_utf8(output.stdout)
        .map_err(|e| anyhow!("Invalid UTF-8 in output: {}", e))
}
```

## Writing Tests

### Test Organization

**Unit Tests**: In the module file with `#[cfg(test)]`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_message_creation() {
        let msg = Message::new("user", "hello".to_string());
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "hello");
    }
}
```

**Integration Tests**: In `tests/` directory

```rust
// tests/integration_test.rs
#[tokio::test]
async fn test_full_message_flow() {
    let config = Config::default();
    let session = Session::load_or_create().unwrap();
    let mut app = App::new(config, session, None);
    
    // Test behavior
    assert!(app.running);
}
```

### Test Patterns

**Testing async code:**

```rust
#[tokio::test]
async fn test_ollama_client() {
    let client = OllamaClient::new("http://localhost:11434", "qwen:3.5")
        .await
        .expect("Failed to create client");
    
    let models = client.list_models().await;
    assert!(models.is_ok());
}
```

**Testing with mock data:**

```rust
#[test]
fn test_message_buffer_compression() {
    let mut buffer = MessageBuffer::new_in_memory();
    
    // Add many messages
    for i in 0..30 {
        let msg = Message::new("user", format!("msg {}", i));
        buffer.add_and_persist(msg).unwrap();
    }
    
    // Verify compression
    assert!(buffer.is_compressed());
}
```

### Test Requirements

- ✅ All public APIs must have tests
- ✅ Error paths must be tested
- ✅ Async functions should use `#[tokio::test]`
- ✅ Mocking/fixtures for external dependencies
- ✅ Run tests before committing: `cargo test`

## Building & Testing

### Development Build

```bash
# Debug build (fast compile, slow runtime)
cargo build

# Run
./target/debug/yggdra
```

### Release Build

```bash
# Release build (slow compile, optimized runtime)
cargo build --release

# Binary location
./target/release/yggdra

# Size
ls -lh target/release/yggdra
```

### Testing

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_message_creation

# Run with output
cargo test -- --nocapture

# Run only integration tests
cargo test --test integration_test
```

### Quality Checks

```bash
# Format check
cargo fmt -- --check

# Linting (strict)
cargo clippy -- -D warnings

# Documentation check
cargo doc --no-deps --document-private-items

# Run all checks before commit
cargo fmt && cargo clippy -- -D warnings && cargo test
```

## Submitting Changes

### Commit Message Format

Follow Conventional Commits:

```
feat: add /tool command for local tool execution

Add support for executing ripgrep, git, and bash commands
directly from Yggdra. Includes timeout protection (30s) and
proper error handling.

Fixes #123
Tests: Added test_tool_execution_timeout

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>
```

### Types

- `feat:` - New feature
- `fix:` - Bug fix
- `refactor:` - Code restructuring
- `docs:` - Documentation
- `test:` - Tests
- `perf:` - Performance improvement
- `chore:` - Maintenance

### PR Checklist

Before submitting a pull request, ensure:

- [ ] Code builds: `cargo build --release`
- [ ] All tests pass: `cargo test`
- [ ] No clippy warnings: `cargo clippy -- -D warnings`
- [ ] Code formatted: `cargo fmt`
- [ ] Commits have descriptive messages
- [ ] PR description explains changes and testing
- [ ] Related issues are referenced

### Review Process

1. **Automated Checks**: Tests, clippy, formatting
2. **Code Review**: Maintainers review for:
   - Correctness and design
   - Performance implications
   - Security considerations
   - Documentation completeness
3. **Merge**: Upon approval and all checks passing

## Debugging

### Enable Debug Logging

```bash
# Run with stderr output visible
RUST_LOG=debug ./target/debug/yggdra 2>&1 | tee debug.log

# Specific module
RUST_LOG=yggdra::ollama=debug ./target/debug/yggdra
```

### Debugging Tools

**GDB/LLDB:**

```bash
# Compile with debug symbols
cargo build

# Debug with lldb (macOS)
lldb target/debug/yggdra

# Debug with gdb (Linux)
gdb target/debug/yggdra
```

**Profiling:**

```bash
# CPU profiling with perf
cargo build --release
perf record -g target/release/yggdra
perf report

# Memory profiling
valgrind --leak-check=full target/release/yggdra
```

## Questions or Issues?

- **Bug Reports**: GitHub Issues with reproduction steps
- **Feature Requests**: GitHub Discussions or Issues
- **General Questions**: GitHub Discussions or email

---

**Thank you for contributing to Yggdra!** 🚀
