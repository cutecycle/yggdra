# Yggdra Phase 3 Tools & Agents - User Guide

## Quick Start

### Using Tools in the TUI

Press `/` to start a command, then type `/tool` to execute tools:

```
/tool rg "pattern" "/path/to/search"
/tool commit "fix: bug in parser"
/tool python "script.py" "arg1"
/tool spawn "/usr/local/bin/tool" "-v"
/tool editfile "/path/to/file"
/tool ruste "program.rs"
```

### Tool Overview

#### 1. RipgrepTool (rg) - File Search
Search for patterns in files using ripgrep.

**Usage:**
```bash
/tool rg "fn main" ~/project
/tool rg "TODO" src/
/tool rg "const API" .
```

**Features:**
- Fast regex pattern search
- Recursively searches directories
- Returns matching lines or "no matches"

**Safety:**
- Blocks shell injection attempts
- No pipes, redirects, or command substitution
- Dangerous commands (curl, rm, etc.) rejected

---

#### 2. SpawnTool (spawn) - Execute Binaries
Run child processes safely.

**Usage:**
```bash
/tool spawn "/usr/local/bin/mytool" "-v"
/tool spawn "./target/debug/program" "arg1"
/tool spawn "/path/to/binary"
```

**Features:**
- Execute local binaries
- Pass arguments to child process
- Returns stdout from child

**Safety:**
- Blocks system paths (/bin/, /usr/bin/, /sbin/)
- Verifies binary exists before execution
- Validates executable permissions

---

#### 3. EditfileTool (editfile) - File Editing
Edit files with automatic backups.

**Usage:**
```bash
/tool editfile "/path/to/file.txt"
/tool editfile "./config.toml"
/tool editfile "src/main.rs"
```

**Features:**
- Creates automatic backup: `.backup/filename.TIMESTAMP`
- Validates file path
- Prevents accidental overwrites

**Safety:**
- Blocks path traversal (../)
- Protects system files (/etc, /bin, etc.)
- Requires relative or safe absolute paths

---

#### 4. CommitTool (commit) - Git Operations
Create git commits locally.

**Usage:**
```bash
/tool commit "fix: parser bug"
/tool commit "docs: update README"
/tool commit "refactor: cleanup code"
```

**Features:**
- Creates commits with your message
- Returns commit hash on success
- Reports "no changes to commit" if nothing staged

**Safety:**
- Validates git is installed
- Requires non-empty message
- Runs in current directory

---

#### 5. PythonTool (python) - Python Execution
Execute Python 3 scripts safely.

**Usage:**
```bash
/tool python "script.py"
/tool python "analyze.py" "input.csv"
/tool python "test.py" "--verbose"
```

**Features:**
- Runs Python scripts with arguments
- Returns stdout from script
- Reports errors with full traceback

**Safety:**
- Scans for dangerous imports (requests, urllib, socket, http)
- Blocks network library imports
- Requires valid Python 3 installation
- Validates script exists before execution

---

#### 6. RusteTool (ruste) - Rust Compilation
Compile and execute Rust code.

**Usage:**
```bash
/tool ruste "program.rs"
/tool ruste "solution.rs"
```

**Features:**
- Compiles Rust source to binary
- Executes compiled binary
- Returns program output
- Tries native rustc, falls back to Docker

**Safety:**
- Scans for network code (TcpStream, tokio::net)
- Blocks network-enabled Rust code
- Validates file exists
- Cleans up temporary binaries

---

## Agent Framework (Advanced)

The agent framework enables autonomous tool use through Ollama with steering directives.

### Agent Configuration

```rust
use yggdra::agent::AgentConfig;

let config = AgentConfig::new("llama2", "http://localhost:11434")
    .with_max_iterations(10);
```

### Agentic Loop

1. **User sends query** → Agent receives with steering directive
2. **LLM responds** → Agent parses for tool calls `[TOOL: name args]`
3. **Tools execute** → Results collected with error handling
4. **Results injected** → `[TOOL_OUTPUT: result]` sent back to LLM
5. **Loop repeats** → Until max iterations or `[DONE]` signal

### Tool Call Format

LLM outputs tool calls in this format:
```
[TOOL: rg "pattern" "/path"]
[TOOL: commit "message"]
[TOOL: python "script.py"]
```

Agent parses and executes these automatically.

---

## Security Model

### Network Escape Prevention

All tools validate inputs to prevent network escapes:

**RipgrepTool blocks:**
- Shell operators: `|`, `&`, `;`, `>`, `<`
- Command substitution: `` ` ``, `$()`
- Network commands: `curl`, `wget`, `nc`

**PythonTool blocks:**
- `import requests`
- `import urllib`
- `import socket`
- `import http`

**RusteTool blocks:**
- `TcpStream` code
- `tokio::net` usage
- Async networking

**SpawnTool blocks:**
- System paths: `/bin/`, `/usr/bin/`, `/sbin/`
- Unvalidated binaries

**EditfileTool blocks:**
- Path traversal: `../`
- System files: `/etc`, `/bin`, etc.

### Validation Pipeline

Every tool execution follows this validation:

1. **Input sanitization** - Empty args, type checking
2. **Dangerous pattern detection** - Regex matching
3. **File existence verification** - For file operations
4. **Permission validation** - Where applicable
5. **Safe execution** - With proper error handling

---

## Error Handling

All tools return descriptive errors:

```
❌ Tool error: rg: dangerous pattern detected in: test | other
❌ Tool error: spawn: dangerous system path blocked: /bin/bash
❌ Tool error: python: network import blocked: import requests
❌ Tool error: editfile: path traversal attempt blocked: ../etc/passwd
```

---

## Examples

### Search and Display Pattern

```bash
/tool rg "TODO" src/
# Returns all TODO comments in src/ directory
```

### Run Test Script

```bash
/tool python "run_tests.py" "--coverage"
# Executes Python script with coverage reporting
```

### Git Workflow

```bash
/tool rg "FIXME" .
# Find all FIXME comments
/tool editfile "src/bug.rs"
# Edit the file (manually in future phase)
/tool commit "fix: resolve FIXME comments"
# Commit the changes
```

### Rust Experiment

```bash
/tool ruste "experiment.rs"
# Compile and run Rust program
```

---

## Testing Tools

All tools include comprehensive tests:

```bash
# Run all tests
cargo test

# Run tool tests only
cargo test --lib tools::

# Run integration tests
cargo test --test tools_integration
```

Test coverage includes:
- Input validation
- Security blocking
- Error cases
- Happy paths

---

## Limitations & Future

### Current Limitations
- Tools run synchronously (blocking)
- No result caching
- Single tool per command
- Limited to local execution

### Future Enhancements (Phase 4+)
- Parallel tool execution
- Tool result caching
- Tool composition/chaining
- Remote tool execution
- Streaming output
- Tool metrics/profiling

---

## Support

For issues or questions:
1. Check tool help: `/help`
2. Review tool validation error messages
3. Verify tool prerequisites (git, python3, rustc)
4. Check test suite for examples

All tools are production-ready with comprehensive error handling.
