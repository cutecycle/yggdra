//! Snapshot tests: golden strings from real model families, verifying our parser handles every one.
//!
//! Key parser behaviour discovered during authoring:
//!   - `parse_xml_tool_calls` does NOT skip `<think>` content — tool calls found inside
//!     `<think>` blocks ARE parsed. To strip thinking before parsing, call
//!     `sanitize_model_output` first (which removes `<think>…</think>` blocks).
//!   - `sanitize_model_output` also strips stop tokens: `<|endoftext|>`, `<|im_end|>`, etc.
//!   - Valid tool names: shell, setfile, patchfile, commit.
//!   - Unix command names (cat, ls, echo, python3 …) auto-remap to the `shell` tool.
//!   - Args for setfile: `"path\x00content"`, for patchfile: `"path\x00start\x00end\x00text"`.
//!   - commit accepts both `<message>` and `<commit_message>` tags.

use yggdra::agent::{parse_xml_tool_calls, parse_tool_calls, sanitize_model_output};

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 1 — qwen3.5 / qwen2.5 style (clean, follows instructions well)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn qwen_clean_shell() {
    let text = "<tool>shell</tool>\n<command>ls -la</command>\n<desc>List current directory files.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "ls -la");
    assert_eq!(calls[0].description.as_deref(), Some("List current directory files."));
}

#[test]
fn qwen_think_then_shell() {
    // qwen3.5 with thinking enabled: <think> before call
    // Note: parse_xml_tool_calls finds tool calls *inside* think blocks too.
    // We use sanitize first so only the real call after </think> is returned.
    let text = "<think>\nI should list the files first.\n</think>\n<tool>shell</tool>\n<command>find . -name '*.rs'</command>\n<desc>Find Rust files.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "find . -name '*.rs'");
}

#[test]
fn qwen_two_calls_back_to_back() {
    let text = "<tool>shell</tool>\n<command>cargo build</command>\n<desc>Build the project.</desc>\n<tool>shell</tool>\n<command>cargo test --lib</command>\n<desc>Run tests.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "cargo build");
    assert_eq!(calls[1].args, "cargo test --lib");
}

#[test]
fn qwen_setfile_rust_code() {
    let content = "fn main() {\n    println!(\"Hello, world!\");\n}";
    let text = format!("<tool>setfile</tool>\n<path>src/main.rs</path>\n<content>\n{}</content>\n<desc>Create main.rs</desc>", content);
    let calls = parse_xml_tool_calls(&text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "setfile");
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "src/main.rs");
    assert_eq!(parts[1], content);
}

#[test]
fn qwen_commit_call() {
    let text = "<tool>commit</tool>\n<message>feat: add authentication module</message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "feat: add authentication module");
}

#[test]
fn qwen_patchfile_call() {
    let text = "<tool>patchfile</tool>\n<path>src/lib.rs</path>\n<start_line>10</start_line>\n<end_line>12</end_line>\n<new_text>// replaced\n</new_text>\n<desc>Patch lib.rs</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "patchfile");
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[0], "src/lib.rs");
    assert_eq!(parts[1], "10");
    assert_eq!(parts[2], "12");
    assert_eq!(parts[3], "// replaced\n");
}

#[test]
fn qwen_shell_with_returnlines() {
    let text = "<tool>shell</tool>\n<command>cargo test 2>&1</command>\n<returnlines>50</returnlines>\n<desc>Run tests and capture output.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    // args = "command\x00returnlines"
    assert!(calls[0].args.contains('\x00'));
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "cargo test 2>&1");
    assert_eq!(parts[1], "50");
}

#[test]
fn qwen_shell_async_mode() {
    let text = "<tool>shell</tool>\n<command>make build</command>\n<mode>async</mode>\n<task_id>build-01</task_id>\n<desc>Run build in background.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].async_mode);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("build-01"));
}

#[test]
fn qwen_long_think_then_call() {
    let thinking = "a".repeat(500);
    let text = format!(
        "<think>\n{}\n</think>\n<tool>shell</tool>\n<command>echo done</command>\n<desc>Signal completion.</desc>",
        thinking
    );
    let sanitized = sanitize_model_output(&text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo done");
}

#[test]
fn qwen_multi_step_think_two_calls() {
    let text = "<think>\nStep 1: check files.\nStep 2: run tests.\n</think>\n<tool>shell</tool>\n<command>ls src/</command>\n<desc>List source files.</desc>\n<tool>shell</tool>\n<command>cargo test --lib</command>\n<desc>Run lib tests.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "ls src/");
    assert_eq!(calls[1].args, "cargo test --lib");
}

#[test]
fn qwen_tellhuman_tag() {
    let text = "<tool>shell</tool>\n<command>cargo build --release</command>\n<desc>Build release binary.</desc>\n<tellhuman>Starting release build, this may take a moment.</tellhuman>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tellhuman.as_deref(), Some("Starting release build, this may take a moment."));
}

#[test]
fn qwen_setfile_json() {
    let text = "<tool>setfile</tool>\n<path>config.json</path>\n<content>\n{\"key\": \"value\"}\n</content>\n<desc>Write config file.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "config.json");
    assert_eq!(parts[1], "{\"key\": \"value\"}\n");
}

#[test]
fn qwen_commit_multiline_message() {
    let text = "<tool>commit</tool>\n<message>fix: resolve race condition\n\nThe previous implementation had a TOCTOU issue.</message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].args.contains("TOCTOU"));
}

#[test]
fn qwen_shell_no_trailing_newline() {
    let text = "<tool>shell</tool>\n<command>pwd</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "pwd");
    assert!(calls[0].description.is_none());
}

#[test]
fn qwen_setfile_empty_content() {
    let text = "<tool>setfile</tool>\n<path>empty.txt</path>\n<content></content>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "empty.txt");
    assert_eq!(parts[1], "");
}

#[test]
fn qwen_patchfile_single_line() {
    let text = "<tool>patchfile</tool>\n<path>README.md</path>\n<start_line>1</start_line>\n<end_line>1</end_line>\n<new_text># My Project\n</new_text>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[0], "README.md");
    assert_eq!(parts[1], "1");
    assert_eq!(parts[2], "1");
}

#[test]
fn qwen_three_calls_sequence() {
    let text = concat!(
        "<tool>shell</tool>\n<command>git status</command>\n<desc>Check status.</desc>\n",
        "<tool>shell</tool>\n<command>git add -A</command>\n<desc>Stage all.</desc>\n",
        "<tool>commit</tool>\n<message>chore: update dependencies</message>"
    );
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2].name, "commit");
    assert_eq!(calls[2].args, "chore: update dependencies");
}

#[test]
fn qwen_shell_complex_pipe() {
    let text = "<tool>shell</tool>\n<command>rg 'fn parse' src/ --type rust | head -20</command>\n<desc>Find parse functions.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].args.contains("rg 'fn parse'"));
}

#[test]
fn qwen_think_block_stripped_no_phantom_calls() {
    // Thinking contains XML that looks like a tool call — sanitize must strip it
    let text = "<think>\n<tool>shell</tool>\n<command>bad command</command>\n</think>\n<tool>shell</tool>\n<command>good command</command>\n<desc>The real call.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "good command");
}

#[test]
fn qwen_inline_parse_tool_calls_dispatch() {
    // parse_tool_calls should also find this XML call
    let text = "<tool>shell</tool>\n<command>date</command>\n<desc>Get current date.</desc>";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn qwen_setfile_path_with_subdir() {
    let text = "<tool>setfile</tool>\n<path>src/utils/helpers.rs</path>\n<content>\npub fn noop() {}\n</content>\n<desc>Create helper module.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "src/utils/helpers.rs");
    assert_eq!(parts[1], "pub fn noop() {}\n");
}

#[test]
fn qwen_commit_conventional_breaking_change() {
    let text = "<tool>commit</tool>\n<message>feat!: redesign public API\n\nBREAKING CHANGE: removed deprecated methods</message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].args.starts_with("feat!:"));
}

#[test]
fn qwen_desc_with_special_chars() {
    let text = "<tool>shell</tool>\n<command>echo 'hello world'</command>\n<desc>Print greeting: hello & world.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].description.as_deref(), Some("Print greeting: hello & world."));
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 2 — deepseek-r1 style (always emits <think> first)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn r1_short_think_simple_shell() {
    let text = "<think>\nLet me run a quick check.\n</think>\n<tool>shell</tool>\n<command>echo hello</command>\n<desc>greet</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo hello");
}

#[test]
fn r1_long_think_then_setfile() {
    let think = "First I need to understand the structure.\nLet me look at what files exist.\nOkay, I'll create the file now.";
    let content = "pub struct Foo;\n";
    let text = format!(
        "<think>\n{}\n</think>\n<tool>setfile</tool>\n<path>src/foo.rs</path>\n<content>\n{}</content>\n<desc>Create foo module.</desc>",
        think, content
    );
    let sanitized = sanitize_model_output(&text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "setfile");
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "src/foo.rs");
}

#[test]
fn r1_think_with_attempted_wrong_format_then_correct() {
    // Thinking shows the model trying another format first, but actual output is correct XML
    let text = "<think>\nMaybe I should use JSON? No, I'll use the XML format.\n{\"tool\": \"shell\", \"args\": \"ls\"}\nActually, let me use XML.\n</think>\n<tool>shell</tool>\n<command>ls -la</command>\n<desc>List files.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls -la");
}

#[test]
fn r1_empty_think_block() {
    let text = "<think></think>\n<tool>shell</tool>\n<command>cat README.md</command>\n<desc>Read README.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "cat README.md");
}

#[test]
fn r1_raw_parse_finds_call_inside_think() {
    // Without sanitize, parse_xml_tool_calls WILL find calls inside <think> — documented limitation
    let text = "<think>\n<tool>shell</tool>\n<command>phantom</command>\n</think>\n<tool>shell</tool>\n<command>real</command>\n<desc>Real call.</desc>";
    let calls = parse_xml_tool_calls(text); // no sanitize
    // Parser finds both
    assert!(calls.len() >= 1);
    // The first call found is the phantom inside <think>
    assert_eq!(calls[0].args, "phantom");
}

#[test]
fn r1_think_before_each_of_two_calls() {
    let text = concat!(
        "<think>\nNeed to build first.\n</think>\n",
        "<tool>shell</tool>\n<command>cargo build</command>\n<desc>Build.</desc>\n",
        "<think>\nNow test.\n</think>\n",
        "<tool>shell</tool>\n<command>cargo test</command>\n<desc>Test.</desc>"
    );
    // sanitize strips both <think> blocks
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "cargo build");
    assert_eq!(calls[1].args, "cargo test");
}

#[test]
fn r1_thinking_with_unicode() {
    let text = "<think>\nThinking… 🤔 déjà vu — let's go!\n</think>\n<tool>shell</tool>\n<command>echo unicode</command>\n<desc>Unicode test.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo unicode");
}

#[test]
fn r1_commit_with_think() {
    let text = "<think>\nI'll commit the changes now.\n</think>\n<tool>commit</tool>\n<message>refactor: extract helper functions</message>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "refactor: extract helper functions");
}

#[test]
fn r1_thinking_style_begin_end_of_thought() {
    // Some r1 variants use <|begin_of_thought|>...</|end_of_thought|>
    let text = "<|begin_of_thought|>\nMy internal reasoning here.\n<|end_of_thought|>\n<tool>shell</tool>\n<command>ls</command>\n<desc>List files.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls");
}

#[test]
fn r1_patchfile_after_think() {
    let text = "<think>\nI need to fix line 5 of the config.\n</think>\n<tool>patchfile</tool>\n<path>config.toml</path>\n<start_line>5</start_line>\n<end_line>5</end_line>\n<new_text>timeout = 30\n</new_text>\n<desc>Update timeout config.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "patchfile");
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[0], "config.toml");
    assert_eq!(parts[1], "5");
    assert_eq!(parts[2], "5");
    assert!(parts[3].contains("timeout = 30"));
}

#[test]
fn r1_very_long_thinking_paragraphs() {
    let para = "Let me think about the implications of this carefully. ";
    let thinking = para.repeat(20); // ~1100 chars of thinking
    let text = format!(
        "<think>\n{}\n</think>\n<tool>shell</tool>\n<command>make check</command>\n<desc>Run checks.</desc>",
        thinking
    );
    let sanitized = sanitize_model_output(&text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "make check");
}

#[test]
fn r1_thinking_tag_variant() {
    // Some variants use <thinking>...</thinking>
    let text = "<thinking>\nAnalyzing the problem...\n</thinking>\n<tool>shell</tool>\n<command>cargo clippy</command>\n<desc>Run clippy.</desc>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "cargo clippy");
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 3 — gemma3 style (brief preamble, commit_message tag)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn gemma_with_preamble_ill_check() {
    let text = "I'll check the files:\n<tool>shell</tool>\n<command>ls -la</command>\n<desc>List directory contents.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls -la");
}

#[test]
fn gemma_let_me_run_that_preamble() {
    let text = "Let me run that:\n<tool>shell</tool>\n<command>cargo fmt --check</command>\n<desc>Check formatting.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "cargo fmt --check");
}

#[test]
fn gemma_commit_message_tag() {
    // Gemma sometimes uses <commit_message> instead of <message>
    let text = "<tool>commit</tool>\n<commit_message>docs: update README with setup instructions</commit_message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "docs: update README with setup instructions");
}

#[test]
fn gemma_clean_no_preamble() {
    let text = "<tool>shell</tool>\n<command>git log --oneline -10</command>\n<desc>Show recent commits.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "git log --oneline -10");
}

#[test]
fn gemma_setfile_python_content() {
    let content = "#!/usr/bin/env python3\nprint('hello')\n";
    let text = format!(
        "<tool>setfile</tool>\n<path>hello.py</path>\n<content>\n{}</content>\n<desc>Create Python script.</desc>",
        content
    );
    let calls = parse_xml_tool_calls(&text);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "hello.py");
    assert_eq!(parts[1], content);
}

#[test]
fn gemma_python3_tool_remapped_to_shell() {
    // Gemma emits <tool>python3</tool> — auto-remap to shell
    let text = "<tool>python3</tool>\n<command>script.py</command>\n<desc>Run the Python script.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    // prefix "python3" is prepended to "script.py"
    assert_eq!(calls[0].args, "python3 script.py");
}

#[test]
fn gemma_multiple_shell_calls() {
    let text = concat!(
        "Sure, I'll run both commands:\n",
        "<tool>shell</tool>\n<command>git fetch origin</command>\n<desc>Fetch latest.</desc>\n",
        "<tool>shell</tool>\n<command>git merge origin/main</command>\n<desc>Merge main.</desc>"
    );
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "git fetch origin");
    assert_eq!(calls[1].args, "git merge origin/main");
}

#[test]
fn gemma_desc_after_command_normal_order() {
    let text = "<tool>shell</tool>\n<command>cargo test --lib 2>&1 | tail -5</command>\n<desc>Run tests and show last 5 lines.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].description.as_deref(), Some("Run tests and show last 5 lines."));
}

#[test]
fn gemma_prose_after_call_ignored() {
    let text = "<tool>shell</tool>\n<command>ls src/</command>\n<desc>List source.</desc>\n\nThis will show us the source directory structure.";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls src/");
}

#[test]
fn gemma_bash_tool_remapped() {
    // <tool>bash</tool> should remap to shell
    let text = "<tool>bash</tool>\n<command>echo 'test'</command>\n<desc>Echo test.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "bash echo 'test'");
}

#[test]
fn gemma_git_tool_remapped() {
    let text = "<tool>git</tool>\n<command>status</command>\n<desc>Check git status.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "git status");
}

#[test]
fn gemma_preamble_then_setfile() {
    let text = "I'll create that file for you:\n<tool>setfile</tool>\n<path>notes.txt</path>\n<content>\nMemo: check the build\n</content>\n<desc>Write notes file.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "setfile");
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "notes.txt");
    assert_eq!(parts[1], "Memo: check the build\n");
}

#[test]
fn gemma_commit_message_with_scope() {
    let text = "<tool>commit</tool>\n<commit_message>feat(parser): support gemma commit_message tag</commit_message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "feat(parser): support gemma commit_message tag");
}

#[test]
fn gemma_shell_then_commit() {
    let text = "I'll stage and commit:\n<tool>shell</tool>\n<command>git add -A</command>\n<desc>Stage all changes.</desc>\n<tool>commit</tool>\n<commit_message>chore: stage and commit all changes</commit_message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[1].name, "commit");
    assert_eq!(calls[1].args, "chore: stage and commit all changes");
}

#[test]
fn gemma_node_tool_remapped() {
    let text = "<tool>node</tool>\n<command>index.js</command>\n<desc>Run Node.js script.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "node index.js");
}

#[test]
fn gemma_cargo_tool_remapped() {
    let text = "<tool>cargo</tool>\n<command>build --release</command>\n<desc>Build in release mode.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cargo build --release");
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 4 — llama3.2 style (compact, sometimes no <desc>, brief prefix)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn llama_shell_no_desc() {
    let text = "<tool>shell</tool>\n<command>ls -la</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "ls -la");
    assert!(calls[0].description.is_none());
}

#[test]
fn llama_very_brief_just_tool_and_command() {
    let text = "<tool>shell</tool>\n<command>echo hello</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo hello");
}

#[test]
fn llama_running_prefix_before_xml() {
    let text = "Running:\n<tool>shell</tool>\n<command>cargo check</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "cargo check");
}

#[test]
fn llama_compact_setfile() {
    let text = "<tool>setfile</tool>\n<path>Makefile</path>\n<content>\nall:\n\tcargo build\n</content>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "Makefile");
    assert!(parts[1].contains("cargo build"));
}

#[test]
fn llama_commit_no_think() {
    let text = "<tool>commit</tool>\n<message>test: add integration tests</message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "test: add integration tests");
}

#[test]
fn llama_prose_after_tool_call() {
    let text = "<tool>shell</tool>\n<command>git diff HEAD</command>\n<desc>Show changes.</desc>\n\nI'll analyze the diff once I see the output.";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "git diff HEAD");
}

#[test]
fn llama_two_calls() {
    let text = "<tool>shell</tool>\n<command>cargo fmt</command>\n<tool>shell</tool>\n<command>cargo clippy</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "cargo fmt");
    assert_eq!(calls[1].args, "cargo clippy");
}

#[test]
fn llama_running_prefix_with_desc() {
    let text = "Running: \n<tool>shell</tool>\n<command>make test</command>\n<desc>Run the test suite.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "make test");
    assert_eq!(calls[0].description.as_deref(), Some("Run the test suite."));
}

#[test]
fn llama_cat_tool_remapped() {
    let text = "<tool>cat</tool>\n<command>src/main.rs</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cat src/main.rs");
}

#[test]
fn llama_find_tool_remapped() {
    let text = "<tool>find</tool>\n<command>. -name '*.toml'</command>\n<desc>Find TOML files.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "find . -name '*.toml'");
}

#[test]
fn llama_grep_tool_remapped() {
    let text = "<tool>grep</tool>\n<command>-r 'TODO' src/</command>\n<desc>Find TODOs.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "grep -r 'TODO' src/");
}

#[test]
fn llama_shell_with_semicolons() {
    let text = "<tool>shell</tool>\n<command>cd /tmp && ls -la && pwd</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].args.contains("&&"));
}

#[test]
fn llama_patchfile_no_desc() {
    let text = "<tool>patchfile</tool>\n<path>src/lib.rs</path>\n<start_line>3</start_line>\n<end_line>3</end_line>\n<new_text>use std::io;\n</new_text>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "patchfile");
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 5 — smollm2 style (tiny 1.7B model, unreliable but catchable)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smollm2_sure_prefix() {
    let text = "Sure! <tool>shell</tool>\n<command>ls</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls");
}

#[test]
fn smollm2_here_is_prefix() {
    let text = "Here is the command:\n<tool>shell</tool>\n<command>echo 'ready'</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo 'ready'");
}

#[test]
fn smollm2_shell_no_desc() {
    let text = "<tool>shell</tool>\n<command>pwd</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].description.is_none());
}

#[test]
fn smollm2_echo_tool_remapped() {
    // Small models emit <tool>echo</tool> — auto-remap rescues it
    let text = "<tool>echo</tool>\n<command>hello world</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "echo hello world");
}

#[test]
fn smollm2_minimal_tool_command_only() {
    let text = "<tool>shell</tool>\n<command>date</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "date");
}

#[test]
fn smollm2_ls_tool_remapped() {
    // <tool>ls</tool> → shell with "ls" prefix
    let text = "<tool>ls</tool>\n<command>-la /home</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "ls -la /home");
}

#[test]
fn smollm2_sure_with_desc() {
    let text = "Sure!\n<tool>shell</tool>\n<command>cat Cargo.toml</command>\n<desc>Read Cargo.toml.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "cat Cargo.toml");
}

#[test]
fn smollm2_make_tool_remapped() {
    let text = "<tool>make</tool>\n<command>install</command>\n<desc>Install binary.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "make install");
}

#[test]
fn smollm2_setfile_simple() {
    let text = "Here is the file:\n<tool>setfile</tool>\n<path>hello.txt</path>\n<content>\nhello\n</content>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "setfile");
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "hello.txt");
    assert_eq!(parts[1], "hello\n");
}

#[test]
fn smollm2_sh_tool_remapped() {
    let text = "<tool>sh</tool>\n<command>-c 'echo test'</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "sh -c 'echo test'");
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 6 — Heretic model outputs + sanitize_model_output
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn heretic_endoftext_after_tool_call() {
    let text = "<tool>shell</tool>\n<command>ls</command>\n<desc>List files.</desc><|endoftext|>";
    let sanitized = sanitize_model_output(text);
    assert!(!sanitized.contains("<|endoftext|>"));
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls");
}

#[test]
fn heretic_im_end_suffix_stripped() {
    let text = "<tool>shell</tool>\n<command>whoami</command><|im_end|>";
    let sanitized = sanitize_model_output(text);
    assert!(!sanitized.contains("<|im_end|>"));
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "whoami");
}

#[test]
fn heretic_eot_id_suffix() {
    let text = "<tool>shell</tool>\n<command>id</command>\n<desc>Who am I.</desc><|eot_id|>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "id");
}

#[test]
fn heretic_end_of_turn_suffix() {
    let text = "<tool>commit</tool>\n<message>fix: resolve issue</message><|end_of_turn|>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "fix: resolve issue");
}

#[test]
fn heretic_eot_suffix() {
    let text = "<tool>shell</tool>\n<command>uname -a</command><|EOT|>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "uname -a");
}

#[test]
fn heretic_tool_call_then_artifact_pipeline() {
    // Complete pipeline: valid call + stop artifact → sanitize → parse
    let text = "<tool>shell</tool>\n<command>cargo build</command>\n<desc>Build.</desc>\n<|endoftext|>\n<|im_start|>user";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "cargo build");
}

#[test]
fn heretic_artifact_only_no_tool_call() {
    let text = "<|endoftext|>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 0);
}

#[test]
fn heretic_multiple_artifacts_stripped() {
    let text = "<tool>shell</tool>\n<command>ls</command><|im_end|><|endoftext|>";
    let sanitized = sanitize_model_output(text);
    // Truncates at first stop marker
    assert!(!sanitized.contains("<|im_end|>"));
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
}

#[test]
fn heretic_im_start_marker_truncates() {
    let text = "<tool>shell</tool>\n<command>echo a</command>\n<|im_start|>assistant\nSome extra output";
    let sanitized = sanitize_model_output(text);
    assert!(!sanitized.contains("<|im_start|>"));
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo a");
}

#[test]
fn heretic_end_marker_stripped() {
    let text = "<tool>shell</tool>\n<command>echo b</command>\n<|end|>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo b");
}

#[test]
fn heretic_deepseek_sentence_marker() {
    // DeepSeek uses <｜end▁of▁sentence｜>
    let text = "<tool>shell</tool>\n<command>echo ds</command><｜end▁of▁sentence｜>";
    let sanitized = sanitize_model_output(text);
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo ds");
}

#[test]
fn heretic_sanitize_preserves_content_before_marker() {
    let text = "Some text before. <tool>shell</tool>\n<command>ls</command>\n<|endoftext|>Text after should be gone.";
    let sanitized = sanitize_model_output(text);
    assert!(sanitized.contains("Some text before"));
    assert!(!sanitized.contains("Text after should be gone"));
}

#[test]
fn heretic_sanitize_think_plus_stop_token() {
    let text = "<think>\nHmm...\n</think>\n<tool>shell</tool>\n<command>pwd</command>\n<|endoftext|>";
    let sanitized = sanitize_model_output(text);
    assert!(!sanitized.contains("<think>"));
    assert!(!sanitized.contains("<|endoftext|>"));
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "pwd");
}

#[test]
fn heretic_signal_tags_stripped() {
    // </done> and </understood> are stripped
    let text = "<tool>shell</tool>\n<command>make</command></done>";
    let sanitized = sanitize_model_output(text);
    assert!(!sanitized.contains("</done>"));
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
}

#[test]
fn heretic_percent_tag_stripped() {
    let text = "<tool>shell</tool>\n<command>make install</command>\n<percent>75</percent>\n<desc>Install.</desc>";
    let sanitized = sanitize_model_output(text);
    assert!(!sanitized.contains("<percent>"));
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "make install");
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 7 — Real-world edge cases
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn edge_model_follows_system_prompt_example_exactly() {
    // Verbatim example from json_tool_descriptions()
    let text = "<tool>shell</tool>\n<command>your sh -c command here</command>\n<desc>What you are doing and why.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn edge_model_repeats_format_description_before_call() {
    let text = "TOOL FORMAT — XML tags:\n<tool>shell</tool>\n<command>ls</command>\n<desc>List files.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls");
}

#[test]
fn edge_setfile_missing_path_and_content() {
    // Model uses <command> for setfile — graceful empty args
    let text = "<tool>setfile</tool>\n<command>some file</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    // path and content both empty, args = "\x00"
    assert_eq!(calls[0].name, "setfile");
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "");
    assert_eq!(parts[1], "");
}

#[test]
fn edge_backtick_fence_falls_back_via_parse_tool_calls() {
    // Model wraps in ``` fences — parse_xml_tool_calls finds nothing,
    // parse_tool_calls tries backtick fallback for bare commands
    let text = "```\nls -la\n```";
    let xml_calls = parse_xml_tool_calls(text);
    assert_eq!(xml_calls.len(), 0); // no <tool> tags
    // parse_tool_calls will also return empty because the backtick extractor
    // skips triple-backtick fences
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 0);
}

#[test]
fn edge_backtick_inline_command_fallback() {
    // Inline backtick with a recognisable shell command
    let text = "You can run `ls -la /home` to list the directory.";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "ls -la /home");
}

#[test]
fn edge_partial_xml_no_closing_tool_tag() {
    // Token limit truncation: <tool> opened but no </tool> — graceful empty
    let text = "<tool>shell";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 0);
}

#[test]
fn edge_partial_xml_no_command_content() {
    // <tool> and </tool> present but no <command> — parser pushes a call with empty args
    let text = "<tool>shell</tool>\n";
    let calls = parse_xml_tool_calls(text);
    // The call IS returned but with empty args (parser does not filter empty-arg shell calls)
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "");
}

#[test]
fn edge_done_signal_after_two_calls() {
    let text = "<tool>shell</tool>\n<command>cargo build</command>\n<desc>Build.</desc>\n<tool>shell</tool>\n<command>cargo test</command>\n<desc>Test.</desc>\n[DONE]";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "cargo build");
    assert_eq!(calls[1].args, "cargo test");
}

#[test]
fn edge_invalid_tool_name_skipped() {
    // <tool>invalid_name</tool> is skipped, subsequent valid call found
    let text = "<tool>unknown_tool</tool>\n<command>something</command>\n<tool>shell</tool>\n<command>ls</command>\n<desc>List.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn edge_both_message_and_commit_message_prefer_message() {
    // If both tags present, <message> takes precedence (find returns first match)
    let text = "<tool>commit</tool>\n<message>first message</message>\n<commit_message>second message</commit_message>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "first message");
}

#[test]
fn edge_empty_input_returns_empty() {
    let calls = parse_xml_tool_calls("");
    assert_eq!(calls.len(), 0);
}

#[test]
fn edge_whitespace_only_input() {
    let calls = parse_xml_tool_calls("   \n\t\n   ");
    assert_eq!(calls.len(), 0);
}

#[test]
fn edge_tool_call_with_newlines_in_command() {
    // Some models emit multi-line <command> blocks
    let text = "<tool>shell</tool>\n<command>echo line1\necho line2</command>\n<desc>Two echoes.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].args.contains("echo line1"));
}

#[test]
fn edge_unicode_in_setfile_content() {
    let content = "# Héllo Wörld\nfn grüss() -> &str { \"Grüß Gott\" }\n";
    let text = format!(
        "<tool>setfile</tool>\n<path>unicode.rs</path>\n<content>\n{}</content>",
        content
    );
    let calls = parse_xml_tool_calls(&text);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[1], content);
}

#[test]
fn edge_async_shell_with_tellhuman() {
    let text = "<tool>shell</tool>\n<command>cargo build --release 2>&1 | tee build.log</command>\n<mode>async</mode>\n<task_id>release-build</task_id>\n<tellhuman>Starting release build in background, will notify when done.</tellhuman>\n<desc>Background release build.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].async_mode);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("release-build"));
    assert!(calls[0].tellhuman.is_some());
}

#[test]
fn edge_rg_tool_remapped_to_shell() {
    // <tool>rg</tool> auto-remaps
    let text = "<tool>rg</tool>\n<command>parse_xml src/</command>\n<desc>Search for function.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "rg parse_xml src/");
}

#[test]
fn edge_awk_tool_remapped() {
    let text = "<tool>awk</tool>\n<command>'{print $1}' data.txt</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "awk '{print $1}' data.txt");
}

#[test]
fn edge_sed_tool_remapped() {
    let text = "<tool>sed</tool>\n<command>-i 's/foo/bar/g' file.txt</command>\n<desc>Replace foo with bar.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "sed -i 's/foo/bar/g' file.txt");
}

#[test]
fn edge_three_mixed_model_calls() {
    // A response containing shell + setfile + commit in sequence
    let text = concat!(
        "<tool>shell</tool>\n<command>cargo fmt</command>\n<desc>Format.</desc>\n",
        "<tool>setfile</tool>\n<path>CHANGELOG.md</path>\n<content>\n## v1.0\n</content>\n<desc>Update changelog.</desc>\n",
        "<tool>commit</tool>\n<message>release: v1.0</message>"
    );
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[1].name, "setfile");
    assert_eq!(calls[2].name, "commit");
    assert_eq!(calls[2].args, "release: v1.0");
}

#[test]
fn edge_sanitize_then_parse_full_pipeline_r1() {
    let text = "<think>\n<tool>shell</tool>\n<command>phantom</command>\n</think>\n<tool>shell</tool>\n<command>real cmd</command>\n<desc>Real.</desc>\n<|endoftext|>";
    let sanitized = sanitize_model_output(text);
    // think block stripped → phantom not parsed; stop token stripped
    let calls = parse_xml_tool_calls(&sanitized);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "real cmd");
}

#[test]
fn edge_model_adds_newline_before_tool_tag() {
    let text = "\n\n\n<tool>shell</tool>\n<command>df -h</command>\n<desc>Disk usage.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "df -h");
}

#[test]
fn edge_jq_tool_remapped() {
    let text = "<tool>jq</tool>\n<command>'.key' data.json</command>\n<desc>Extract key.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "jq '.key' data.json");
}

#[test]
fn edge_touch_tool_remapped() {
    let text = "<tool>touch</tool>\n<command>newfile.txt</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "touch newfile.txt");
}

#[test]
fn edge_mv_tool_remapped() {
    let text = "<tool>mv</tool>\n<command>old.txt new.txt</command>\n<desc>Rename file.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "mv old.txt new.txt");
}

#[test]
fn edge_cp_tool_remapped() {
    let text = "<tool>cp</tool>\n<command>src.txt dst.txt</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cp src.txt dst.txt");
}

#[test]
fn edge_rm_tool_remapped() {
    let text = "<tool>rm</tool>\n<command>-rf build/</command>\n<desc>Remove build directory.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "rm -rf build/");
}

#[test]
fn edge_head_tool_remapped() {
    let text = "<tool>head</tool>\n<command>-20 src/main.rs</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "head -20 src/main.rs");
}

#[test]
fn edge_tail_tool_remapped() {
    let text = "<tool>tail</tool>\n<command>-f app.log</command>\n<desc>Follow log.</desc>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "tail -f app.log");
}

#[test]
fn edge_wc_tool_remapped() {
    let text = "<tool>wc</tool>\n<command>-l src/agent.rs</command>";
    let calls = parse_xml_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "wc -l src/agent.rs");
}

#[test]
fn edge_unix_command_tool_no_command_tag() {
    // Model emits <tool>ls</tool> with no <command> — remap_prefix used alone
    let text = "<tool>ls</tool>";
    let calls = parse_xml_tool_calls(text);
    // rest block is empty, command is empty, so remap_prefix becomes args = "ls"
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "ls");
}
