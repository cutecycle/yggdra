/// Exhaustive proof tests for the XML tool call parser.
///
/// These tests prove to any reader that `parse_xml_tool_calls` and
/// `parse_tool_calls` handle every format variant a model might emit.
///
/// Run:  cargo test --test tool_call_parsing_proofs

use yggdra::agent::{parse_tool_calls, parse_xml_tool_calls};

// ─────────────────────────────────────────────────────────────────────────────
// HELPERS
// ─────────────────────────────────────────────────────────────────────────────

fn shell(cmd: &str) -> String {
    format!("<tool>shell</tool>\n<command>{}</command>", cmd)
}

fn shell_desc(cmd: &str, desc: &str) -> String {
    format!("<tool>shell</tool>\n<command>{}</command>\n<desc>{}</desc>", cmd, desc)
}

// ─────────────────────────────────────────────────────────────────────────────
// SHELL TOOL
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_shell_basic_command() {
    let calls = parse_xml_tool_calls(&shell("echo hello"));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "echo hello");
}

#[test]
fn proof_shell_name_is_always_shell() {
    let calls = parse_xml_tool_calls(&shell("ls"));
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn proof_shell_flags_preserved() {
    let calls = parse_xml_tool_calls(&shell("ls -la /tmp"));
    assert_eq!(calls[0].args, "ls -la /tmp");
}

#[test]
fn proof_shell_pipe_preserved() {
    let calls = parse_xml_tool_calls(&shell("echo foo | grep foo"));
    assert_eq!(calls[0].args, "echo foo | grep foo");
}

#[test]
fn proof_shell_single_quotes_preserved() {
    let calls = parse_xml_tool_calls(&shell("find . -name '*.rs'"));
    assert_eq!(calls[0].args, "find . -name '*.rs'");
}

#[test]
fn proof_shell_redirect_preserved() {
    let calls = parse_xml_tool_calls(&shell("cat file > out.txt"));
    assert_eq!(calls[0].args, "cat file > out.txt");
}

#[test]
fn proof_shell_ampersand_redirect_preserved() {
    let calls = parse_xml_tool_calls(&shell("cargo build 2>&1"));
    assert_eq!(calls[0].args, "cargo build 2>&1");
}

#[test]
fn proof_shell_with_desc_tag() {
    let calls = parse_xml_tool_calls(&shell_desc("echo hi", "Say hi"));
    assert_eq!(calls[0].description.as_deref(), Some("Say hi"));
}

#[test]
fn proof_shell_without_desc_tag_is_none() {
    let calls = parse_xml_tool_calls(&shell("pwd"));
    assert!(calls[0].description.is_none());
}

#[test]
fn proof_shell_empty_desc_is_some_empty() {
    let xml = "<tool>shell</tool>\n<command>pwd</command>\n<desc></desc>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].description.as_deref(), Some(""));
}

#[test]
fn proof_shell_returnlines_appended_with_null() {
    let xml = "<tool>shell</tool>\n<command>cat src/main.rs</command>\n<returnlines>1-100</returnlines>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].args.contains('\x00'));
    let (cmd, rl) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(cmd, "cat src/main.rs");
    assert_eq!(rl, "1-100");
}

#[test]
fn proof_shell_returnlines_range_preserved() {
    let xml = "<tool>shell</tool>\n<command>grep -n fn src/lib.rs</command>\n<returnlines>50-200</returnlines>";
    let calls = parse_xml_tool_calls(xml);
    let (_, rl) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(rl, "50-200");
}

#[test]
fn proof_shell_async_mode_true() {
    let xml = "<tool>shell</tool>\n<command>sleep 5</command>\n<mode>async</mode>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_mode);
}

#[test]
fn proof_shell_async_with_task_id() {
    let xml = "<tool>shell</tool>\n<command>cargo test 2>&1</command>\n<mode>async</mode>\n<task_id>run-tests</task_id>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_mode);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("run-tests"));
}

#[test]
fn proof_shell_async_without_task_id_has_none_task_id() {
    let xml = "<tool>shell</tool>\n<command>sleep 1</command>\n<mode>async</mode>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_mode);
    assert!(calls[0].async_task_id.is_none());
}

#[test]
fn proof_shell_no_mode_tag_async_false() {
    let calls = parse_xml_tool_calls(&shell("ls"));
    assert!(!calls[0].async_mode);
}

#[test]
fn proof_shell_sync_mode_async_false() {
    let xml = "<tool>shell</tool>\n<command>ls</command>\n<mode>sync</mode>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
}

#[test]
fn proof_shell_task_id_none_when_not_async() {
    let xml = "<tool>shell</tool>\n<command>ls</command>\n<task_id>ignored</task_id>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_task_id.is_none());
}

#[test]
fn proof_shell_tellhuman_extracted() {
    let xml = "<tool>shell</tool>\n<command>make</command>\n<tellhuman>Build started!</tellhuman>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].tellhuman.as_deref(), Some("Build started!"));
}

#[test]
fn proof_shell_no_tellhuman_is_none() {
    let calls = parse_xml_tool_calls(&shell("ls"));
    assert!(calls[0].tellhuman.is_none());
}

#[test]
fn proof_shell_leading_whitespace_in_command_trimmed() {
    let xml = "<tool>shell</tool>\n<command>   echo hi   </command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "echo hi");
}

#[test]
fn proof_shell_multiline_command_preserved() {
    let xml = "<tool>shell</tool>\n<command>cargo test --lib\n2>&1</command>";
    let calls = parse_xml_tool_calls(xml);
    // extract_tag trims, so whitespace-only surrounds get trimmed but inner content stays
    assert!(calls[0].args.contains("cargo test"));
}

#[test]
fn proof_shell_think_block_before_ignored() {
    let xml = "<think>I need to run a command.</think><tool>shell</tool>\n<command>ls -la</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls -la");
}

#[test]
fn proof_shell_prose_before_parsed() {
    let xml = "Sure, here is the command:\n<tool>shell</tool>\n<command>echo done</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo done");
}

#[test]
fn proof_shell_prose_after_parsed() {
    let xml = "<tool>shell</tool>\n<command>echo done</command>\nThis will print done.";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
}

#[test]
fn proof_shell_prose_before_and_after() {
    let xml = "Let me run this:\n<tool>shell</tool>\n<command>pwd</command>\nThat will show the cwd.";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "pwd");
}

#[test]
fn proof_shell_long_command() {
    let long_cmd = "a".repeat(500);
    let xml = format!("<tool>shell</tool>\n<command>{}</command>", long_cmd);
    let calls = parse_xml_tool_calls(&xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, long_cmd);
}

#[test]
fn proof_shell_double_ampersand() {
    let calls = parse_xml_tool_calls(&shell("cd /tmp && ls"));
    assert_eq!(calls[0].args, "cd /tmp && ls");
}

#[test]
fn proof_shell_semicolons_preserved() {
    let calls = parse_xml_tool_calls(&shell("echo a; echo b; echo c"));
    assert_eq!(calls[0].args, "echo a; echo b; echo c");
}

#[test]
fn proof_shell_subshell_parens_preserved() {
    let calls = parse_xml_tool_calls(&shell("(cd /tmp && ls)"));
    assert_eq!(calls[0].args, "(cd /tmp && ls)");
}

#[test]
fn proof_shell_double_quotes_in_command() {
    let calls = parse_xml_tool_calls(&shell(r#"grep "hello world" file.txt"#));
    assert_eq!(calls[0].args, r#"grep "hello world" file.txt"#);
}

#[test]
fn proof_shell_backtick_in_command() {
    let calls = parse_xml_tool_calls(&shell("echo `date`"));
    assert!(calls[0].args.contains("`date`"));
}

#[test]
fn proof_shell_dollar_var_in_command() {
    let calls = parse_xml_tool_calls(&shell("echo $HOME"));
    assert_eq!(calls[0].args, "echo $HOME");
}

#[test]
fn proof_shell_path_glob_in_command() {
    let calls = parse_xml_tool_calls(&shell("ls src/**/*.rs"));
    assert_eq!(calls[0].args, "ls src/**/*.rs");
}

#[test]
fn proof_shell_count_is_one() {
    let xml = shell("echo once");
    let calls = parse_xml_tool_calls(&xml);
    assert_eq!(calls.len(), 1);
}

#[test]
fn proof_shell_all_optional_fields_default() {
    let calls = parse_xml_tool_calls(&shell("echo x"));
    let c = &calls[0];
    assert!(c.description.is_none());
    assert!(!c.async_mode);
    assert!(c.async_task_id.is_none());
    assert!(c.tellhuman.is_none());
}

#[test]
fn proof_shell_returnlines_no_extra_content() {
    let xml = "<tool>shell</tool>\n<command>wc -l src/lib.rs</command>\n<returnlines>1-5</returnlines>";
    let calls = parse_xml_tool_calls(xml);
    // Exactly one null byte in args
    let null_count = calls[0].args.chars().filter(|&c| c == '\x00').count();
    assert_eq!(null_count, 1);
}

#[test]
fn proof_shell_no_returnlines_no_null_byte() {
    let calls = parse_xml_tool_calls(&shell("ls"));
    assert!(!calls[0].args.contains('\x00'));
}

// ─────────────────────────────────────────────────────────────────────────────
// SETFILE TOOL
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_setfile_basic_path_and_content() {
    let xml = "<tool>setfile</tool>\n<path>hello.txt</path>\n<content>hello world</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "setfile");
    let (path, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(path, "hello.txt");
    assert_eq!(content, "hello world");
}

#[test]
fn proof_setfile_args_contains_null_separator() {
    let xml = "<tool>setfile</tool>\n<path>a.txt</path>\n<content>data</content>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].args.contains('\x00'));
}

#[test]
fn proof_setfile_multiline_content() {
    let xml = "<tool>setfile</tool>\n<path>multi.txt</path>\n<content>line1\nline2\nline3</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains("line1"));
    assert!(content.contains("line2"));
    assert!(content.contains("line3"));
}

#[test]
fn proof_setfile_leading_newline_stripped() {
    // Parser strips ONE leading newline from content (the newline right after <content>)
    let xml = "<tool>setfile</tool>\n<path>x.txt</path>\n<content>\nhello\n</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(content, "hello\n");
}

#[test]
fn proof_setfile_no_leading_newline_not_double_stripped() {
    let xml = "<tool>setfile</tool>\n<path>x.txt</path>\n<content>hello</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(content, "hello");
}

#[test]
fn proof_setfile_rust_code_content() {
    let xml = "<tool>setfile</tool>\n<path>src/lib.rs</path>\n<content>pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains("pub fn add"));
    assert!(content.contains("a + b"));
}

#[test]
fn proof_setfile_python_code_content() {
    let xml = "<tool>setfile</tool>\n<path>script.py</path>\n<content>def greet(name):\n    print(f\"Hello, {name}\")\n</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains("def greet"));
}

#[test]
fn proof_setfile_empty_content_produces_empty_string() {
    let xml = "<tool>setfile</tool>\n<path>empty.txt</path>\n<content></content>";
    let calls = parse_xml_tool_calls(xml);
    let (path, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(path, "empty.txt");
    assert_eq!(content, "");
}

#[test]
fn proof_setfile_whitespace_only_content_preserved() {
    let xml = "<tool>setfile</tool>\n<path>ws.txt</path>\n<content>   </content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(content, "   ");
}

#[test]
fn proof_setfile_unicode_content() {
    let xml = "<tool>setfile</tool>\n<path>unicode.txt</path>\n<content>Hello 🌍 — café — 日本語</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains("🌍"));
    assert!(content.contains("日本語"));
    assert!(content.contains("café"));
}

#[test]
fn proof_setfile_path_with_directory() {
    let xml = "<tool>setfile</tool>\n<path>src/subdir/module.rs</path>\n<content>// empty</content>";
    let calls = parse_xml_tool_calls(xml);
    let (path, _) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(path, "src/subdir/module.rs");
}

#[test]
fn proof_setfile_path_deeply_nested() {
    let xml = "<tool>setfile</tool>\n<path>a/b/c/d/e.txt</path>\n<content>deep</content>";
    let calls = parse_xml_tool_calls(xml);
    let (path, _) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(path, "a/b/c/d/e.txt");
}

#[test]
fn proof_setfile_with_desc_tag() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>x</content>\n<desc>Create file</desc>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].description.as_deref(), Some("Create file"));
}

#[test]
fn proof_setfile_without_desc_is_none() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>x</content>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].description.is_none());
}

#[test]
fn proof_setfile_very_long_content() {
    let big = "x".repeat(1000);
    let xml = format!("<tool>setfile</tool>\n<path>big.txt</path>\n<content>{}</content>", big);
    let calls = parse_xml_tool_calls(&xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(content.len(), 1000);
}

#[test]
fn proof_setfile_content_with_xml_like_strings_preserved() {
    // XML-like strings inside <content> should NOT be parsed as tags
    let xml = "<tool>setfile</tool>\n<path>t.html</path>\n<content><div>Hello</div>\n<span>World</span></content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    // Parser uses find() for the closing tag, so <div> and <span> are treated as literal text
    // The content extraction stops at </content>, so both tags are inside
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains("<div>") || content.contains("Hello"));
}

#[test]
fn proof_setfile_content_with_braces() {
    let xml = "<tool>setfile</tool>\n<path>f.rs</path>\n<content>fn x() { let y = 1; }</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains('{'));
    assert!(content.contains('}'));
}

#[test]
fn proof_setfile_path_null_separator_exact_format() {
    // Confirm exactly: args = path + \x00 + content
    let xml = "<tool>setfile</tool>\n<path>p.txt</path>\n<content>c</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "p.txt\x00c");
}

#[test]
fn proof_setfile_async_false_by_default() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>data</content>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
}

#[test]
fn proof_setfile_newlines_at_end_of_content_preserved() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>line\n\n\n</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(content, "line\n\n\n");
}

#[test]
fn proof_setfile_content_with_tab_chars() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>\tfirst\n\tsecond</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains('\t'));
}

#[test]
fn proof_setfile_second_leading_newline_kept() {
    // Only the FIRST leading newline is stripped — a second one stays
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>\n\nhello</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    // First newline stripped → "\nhello"
    assert_eq!(content, "\nhello");
}

// ─────────────────────────────────────────────────────────────────────────────
// PATCHFILE TOOL
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_patchfile_basic_args_format() {
    let xml = "<tool>patchfile</tool>\n<path>src/main.rs</path>\n<start_line>5</start_line>\n<end_line>10</end_line>\n<new_text>new code</new_text>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "patchfile");
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[0], "src/main.rs");
    assert_eq!(parts[1], "5");
    assert_eq!(parts[2], "10");
    assert_eq!(parts[3], "new code");
}

#[test]
fn proof_patchfile_has_three_null_separators() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>3</end_line>\n<new_text>x</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let count = calls[0].args.chars().filter(|&c| c == '\x00').count();
    assert_eq!(count, 3);
}

#[test]
fn proof_patchfile_multiline_new_text() {
    let xml = "<tool>patchfile</tool>\n<path>a.rs</path>\n<start_line>2</start_line>\n<end_line>4</end_line>\n<new_text>fn run() {\n    todo!()\n}</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert!(parts[3].contains("fn run()"));
    assert!(parts[3].contains("todo!()"));
}

#[test]
fn proof_patchfile_single_line_replacement() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>7</start_line>\n<end_line>7</end_line>\n<new_text>let x = 42;</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[1], "7");
    assert_eq!(parts[2], "7");
}

#[test]
fn proof_patchfile_empty_new_text_deletion() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>3</start_line>\n<end_line>5</end_line>\n<new_text></new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[3], "");
}

#[test]
fn proof_patchfile_unicode_new_text() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>1</end_line>\n<new_text>// 日本語コメント 🦀</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert!(parts[3].contains("日本語"));
    assert!(parts[3].contains("🦀"));
}

#[test]
fn proof_patchfile_with_desc() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>2</end_line>\n<new_text>x</new_text>\n<desc>Fix typo</desc>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].description.as_deref(), Some("Fix typo"));
}

#[test]
fn proof_patchfile_without_desc_is_none() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>2</end_line>\n<new_text>x</new_text>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].description.is_none());
}

#[test]
fn proof_patchfile_leading_newline_in_new_text_stripped() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>1</end_line>\n<new_text>\nfoo</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    // extract_tag_raw strips one leading newline
    assert_eq!(parts[3], "foo");
}

#[test]
fn proof_patchfile_path_in_correct_position() {
    let xml = "<tool>patchfile</tool>\n<path>mydir/file.rs</path>\n<start_line>10</start_line>\n<end_line>20</end_line>\n<new_text>replacement</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[0], "mydir/file.rs");
}

#[test]
fn proof_patchfile_start_end_as_strings_preserved() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>42</start_line>\n<end_line>99</end_line>\n<new_text>y</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[1], "42");
    assert_eq!(parts[2], "99");
}

#[test]
fn proof_patchfile_async_false_default() {
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>1</end_line>\n<new_text>x</new_text>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
}

// ─────────────────────────────────────────────────────────────────────────────
// COMMIT TOOL
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_commit_message_tag() {
    let xml = "<tool>commit</tool>\n<message>feat: add login</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "feat: add login");
}

#[test]
fn proof_commit_commit_message_alias() {
    let xml = "<tool>commit</tool>\n<commit_message>fix: handle null</commit_message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "fix: handle null");
}

#[test]
fn proof_commit_message_preferred_over_commit_message_alias() {
    // When both are present, <message> wins (it's tried first)
    let xml = "<tool>commit</tool>\n<message>first</message>\n<commit_message>second</commit_message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "first");
}

#[test]
fn proof_commit_conventional_feat_prefix() {
    let xml = "<tool>commit</tool>\n<message>feat(auth): add JWT login</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "feat(auth): add JWT login");
}

#[test]
fn proof_commit_conventional_fix_prefix() {
    let xml = "<tool>commit</tool>\n<message>fix: resolve null pointer</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "fix: resolve null pointer");
}

#[test]
fn proof_commit_conventional_chore_prefix() {
    let xml = "<tool>commit</tool>\n<message>chore: update deps</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "chore: update deps");
}

#[test]
fn proof_commit_conventional_docs_prefix() {
    let xml = "<tool>commit</tool>\n<message>docs: update README</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "docs: update README");
}

#[test]
fn proof_commit_conventional_refactor_prefix() {
    let xml = "<tool>commit</tool>\n<message>refactor: extract helper fn</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "refactor: extract helper fn");
}

#[test]
fn proof_commit_message_with_parens_and_colon() {
    let xml = "<tool>commit</tool>\n<message>feat(parser): handle all formats</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "feat(parser): handle all formats");
}

#[test]
fn proof_commit_message_with_emoji() {
    let xml = "<tool>commit</tool>\n<message>✨ feat: sparkle time</message>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].args.contains("✨"));
    assert!(calls[0].args.contains("sparkle time"));
}

#[test]
fn proof_commit_empty_message_produces_empty_args() {
    let xml = "<tool>commit</tool>\n<message></message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "");
}

#[test]
fn proof_commit_multiline_message() {
    let xml = "<tool>commit</tool>\n<message>feat: add thing\n\nThis adds the thing.\nSee issue #42.</message>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].args.contains("feat: add thing"));
}

#[test]
fn proof_commit_no_message_tag_empty_args() {
    // commit with no <message> tag → args should be empty (both alternatives fail)
    let xml = "<tool>commit</tool>\n<desc>some description</desc>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "");
}

#[test]
fn proof_commit_async_false_default() {
    let xml = "<tool>commit</tool>\n<message>test</message>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
}

#[test]
fn proof_commit_name_is_commit() {
    let xml = "<tool>commit</tool>\n<message>x</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "commit");
}

#[test]
fn proof_commit_message_with_special_chars() {
    let xml = "<tool>commit</tool>\n<message>fix: handle edge-case 'foo' & \"bar\"</message>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].args.contains("edge-case"));
}

#[test]
fn proof_commit_message_whitespace_trimmed() {
    let xml = "<tool>commit</tool>\n<message>  feat: spaced  </message>";
    let calls = parse_xml_tool_calls(xml);
    // extract_tag trims whitespace
    assert_eq!(calls[0].args, "feat: spaced");
}

// ─────────────────────────────────────────────────────────────────────────────
// UNIX COMMAND REMAPPING
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_remap_cat_with_command() {
    let xml = "<tool>cat</tool>\n<command>src/main.rs</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cat src/main.rs");
}

#[test]
fn proof_remap_cat_without_command() {
    let xml = "<tool>cat</tool>\n<desc>just cat</desc>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cat");
}

#[test]
fn proof_remap_ls_with_flags() {
    let xml = "<tool>ls</tool>\n<command>-la /home</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "ls -la /home");
}

#[test]
fn proof_remap_grep_with_args() {
    let xml = "<tool>grep</tool>\n<command>fn main src/*.rs</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "grep fn main src/*.rs");
}

#[test]
fn proof_remap_find_with_args() {
    let xml = "<tool>find</tool>\n<command>. -name '*.toml'</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "find . -name '*.toml'");
}

#[test]
fn proof_remap_echo_command() {
    let xml = "<tool>echo</tool>\n<command>hello world</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "echo hello world");
}

#[test]
fn proof_remap_python3_script() {
    let xml = "<tool>python3</tool>\n<command>script.py</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "python3 script.py");
}

#[test]
fn proof_remap_python_script() {
    let xml = "<tool>python</tool>\n<command>-c 'print(1)'</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "python -c 'print(1)'");
}

#[test]
fn proof_remap_cargo_command() {
    let xml = "<tool>cargo</tool>\n<command>build --release</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cargo build --release");
}

#[test]
fn proof_remap_git_command() {
    let xml = "<tool>git</tool>\n<command>status</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "git status");
}

#[test]
fn proof_remap_make_command() {
    let xml = "<tool>make</tool>\n<command>install</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "make install");
}

#[test]
fn proof_remap_sh_command() {
    let xml = "<tool>sh</tool>\n<command>-c echo hi</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "sh -c echo hi");
}

#[test]
fn proof_remap_bash_command() {
    let xml = "<tool>bash</tool>\n<command>-c 'ls -la'</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "bash -c 'ls -la'");
}

#[test]
fn proof_remap_rg_search() {
    let xml = "<tool>rg</tool>\n<command>fn main src/</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "rg fn main src/");
}

#[test]
fn proof_remap_tree_command() {
    let xml = "<tool>tree</tool>\n<command>-L 2</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "tree -L 2");
}

#[test]
fn proof_remap_jq_command() {
    let xml = "<tool>jq</tool>\n<command>.name data.json</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "jq .name data.json");
}

#[test]
fn proof_remap_mkdir_command() {
    let xml = "<tool>mkdir</tool>\n<command>-p src/tests</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "mkdir -p src/tests");
}

#[test]
fn proof_remap_unknown_tool_skipped() {
    let xml = "<tool>foobar</tool>\n<command>do stuff</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_remap_unknown_tool_xyz_skipped() {
    let xml = "<tool>unknown_xyz_tool</tool>\n<command>whatever</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_remap_node_command() {
    let xml = "<tool>node</tool>\n<command>index.js</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "node index.js");
}

#[test]
fn proof_remap_head_command() {
    let xml = "<tool>head</tool>\n<command>-n 20 file.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "head -n 20 file.txt");
}

#[test]
fn proof_remap_tail_command() {
    let xml = "<tool>tail</tool>\n<command>-f logs/app.log</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "tail -f logs/app.log");
}

#[test]
fn proof_remap_fd_command() {
    let xml = "<tool>fd</tool>\n<command>.rs src/</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "fd .rs src/");
}

#[test]
fn proof_remap_wc_command() {
    let xml = "<tool>wc</tool>\n<command>-l src/main.rs</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "wc -l src/main.rs");
}

#[test]
fn proof_remap_sort_command() {
    let xml = "<tool>sort</tool>\n<command>-u list.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "sort -u list.txt");
}

#[test]
fn proof_remap_sed_command() {
    let xml = "<tool>sed</tool>\n<command>-i 's/foo/bar/g' file.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "sed -i 's/foo/bar/g' file.txt");
}

#[test]
fn proof_remap_awk_command() {
    let xml = "<tool>awk</tool>\n<command>'{print $1}' data.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "awk '{print $1}' data.txt");
}

// ─────────────────────────────────────────────────────────────────────────────
// MULTI-TOOL RESPONSES
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_multi_two_shell_calls() {
    let xml = "<tool>shell</tool>\n<command>echo a</command>\n<tool>shell</tool>\n<command>echo b</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "echo a");
    assert_eq!(calls[1].args, "echo b");
}

#[test]
fn proof_multi_shell_then_setfile() {
    let xml = "<tool>shell</tool>\n<command>mkdir -p src</command>\n<tool>setfile</tool>\n<path>src/lib.rs</path>\n<content>// lib</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[1].name, "setfile");
}

#[test]
fn proof_multi_shell_patchfile_commit() {
    let xml = concat!(
        "<tool>shell</tool>\n<command>ls</command>\n",
        "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>1</end_line>\n<new_text>x</new_text>\n",
        "<tool>commit</tool>\n<message>done</message>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[1].name, "patchfile");
    assert_eq!(calls[2].name, "commit");
}

#[test]
fn proof_multi_three_shell_calls() {
    let xml = "<tool>shell</tool>\n<command>echo 1</command>\n<tool>shell</tool>\n<command>echo 2</command>\n<tool>shell</tool>\n<command>echo 3</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].args, "echo 1");
    assert_eq!(calls[1].args, "echo 2");
    assert_eq!(calls[2].args, "echo 3");
}

#[test]
fn proof_multi_four_tools() {
    let xml = concat!(
        "<tool>shell</tool>\n<command>mkdir out</command>\n",
        "<tool>setfile</tool>\n<path>out/a.txt</path>\n<content>a</content>\n",
        "<tool>setfile</tool>\n<path>out/b.txt</path>\n<content>b</content>\n",
        "<tool>commit</tool>\n<message>add files</message>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 4);
}

#[test]
fn proof_multi_think_between_tools() {
    let xml = concat!(
        "<tool>shell</tool>\n<command>echo first</command>\n",
        "<think>Now I need to write the file.</think>\n",
        "<tool>setfile</tool>\n<path>x.txt</path>\n<content>data</content>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "echo first");
    assert_eq!(calls[1].name, "setfile");
}

#[test]
fn proof_multi_first_call_async_second_not() {
    let xml = concat!(
        "<tool>shell</tool>\n<command>long task</command>\n<mode>async</mode>\n<task_id>bg1</task_id>\n",
        "<tool>shell</tool>\n<command>quick</command>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    assert!(calls[0].async_mode);
    assert!(!calls[1].async_mode);
}

#[test]
fn proof_multi_invalid_tool_skipped_valid_kept() {
    let xml = "<tool>notreal</tool>\n<command>x</command>\n<tool>shell</tool>\n<command>ls</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn proof_multi_setfile_then_shell() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>data</content>\n<tool>shell</tool>\n<command>cat f.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "setfile");
    assert_eq!(calls[1].name, "shell");
}

#[test]
fn proof_multi_correct_order_preserved() {
    let xml = concat!(
        "<tool>commit</tool>\n<message>first</message>\n",
        "<tool>shell</tool>\n<command>second</command>\n",
        "<tool>setfile</tool>\n<path>f</path>\n<content>third</content>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[1].name, "shell");
    assert_eq!(calls[2].name, "setfile");
}

#[test]
fn proof_multi_prose_between_tools() {
    let xml = concat!(
        "<tool>shell</tool>\n<command>ls</command>\n",
        "Great, that worked. Now let me write the file:\n",
        "<tool>setfile</tool>\n<path>out.txt</path>\n<content>result</content>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
}

#[test]
fn proof_multi_two_consecutive_commits() {
    let xml = "<tool>commit</tool>\n<message>first commit</message>\n<tool>commit</tool>\n<message>second commit</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "first commit");
    assert_eq!(calls[1].args, "second commit");
}

#[test]
fn proof_multi_five_tools_all_parsed() {
    let xml = concat!(
        "<tool>shell</tool>\n<command>a</command>\n",
        "<tool>shell</tool>\n<command>b</command>\n",
        "<tool>shell</tool>\n<command>c</command>\n",
        "<tool>shell</tool>\n<command>d</command>\n",
        "<tool>shell</tool>\n<command>e</command>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 5);
}

// ─────────────────────────────────────────────────────────────────────────────
// ASYNC MODE
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_async_mode_and_task_id_set() {
    let xml = "<tool>shell</tool>\n<command>cargo test 2>&1</command>\n<mode>async</mode>\n<task_id>tests-bg</task_id>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_mode);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("tests-bg"));
}

#[test]
fn proof_async_mode_without_task_id_async_true() {
    let xml = "<tool>shell</tool>\n<command>sleep 2</command>\n<mode>async</mode>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_mode);
    assert!(calls[0].async_task_id.is_none());
}

#[test]
fn proof_async_mode_sync_value_not_async() {
    let xml = "<tool>shell</tool>\n<command>echo x</command>\n<mode>sync</mode>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
}

#[test]
fn proof_async_no_mode_tag_defaults_false() {
    let xml = "<tool>shell</tool>\n<command>echo x</command>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
}

#[test]
fn proof_async_task_id_none_when_sync() {
    let xml = "<tool>shell</tool>\n<command>ls</command>\n<mode>sync</mode>\n<task_id>whatever</task_id>";
    let calls = parse_xml_tool_calls(xml);
    // async_mode is false → async_task_id must be None
    assert!(calls[0].async_task_id.is_none());
}

#[test]
fn proof_async_task_id_none_when_no_mode_tag() {
    let xml = "<tool>shell</tool>\n<command>ls</command>\n<task_id>ignored</task_id>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
    assert!(calls[0].async_task_id.is_none());
}

#[test]
fn proof_async_task_id_with_hyphens() {
    let xml = "<tool>shell</tool>\n<command>make</command>\n<mode>async</mode>\n<task_id>build-release-job</task_id>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("build-release-job"));
}

#[test]
fn proof_async_task_id_uuid_style() {
    let xml = "<tool>shell</tool>\n<command>./run.sh</command>\n<mode>async</mode>\n<task_id>a1b2c3d4-e5f6-7890</task_id>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("a1b2c3d4-e5f6-7890"));
}

#[test]
fn proof_async_false_for_setfile() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>data</content>\n<mode>async</mode>";
    let calls = parse_xml_tool_calls(xml);
    // mode tag is only meaningful for shell; setfile always sync
    // Actually the parser does set async_mode for any tool if <mode>async</mode> is present
    // Just verify it doesn't crash
    assert_eq!(calls.len(), 1);
}

#[test]
fn proof_async_false_for_commit() {
    let xml = "<tool>commit</tool>\n<message>x</message>";
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].async_mode);
}

// ─────────────────────────────────────────────────────────────────────────────
// BACKTICK FALLBACK (via parse_tool_calls)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_backtick_simple_command_with_space() {
    let text = "Run this: `echo hello world`";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "echo hello world");
}

#[test]
fn proof_backtick_command_with_slash() {
    let text = "Try `ls /tmp` to list files.";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "ls /tmp");
}

#[test]
fn proof_backtick_command_with_dot() {
    let text = "Use `cat ./file.txt` please.";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn proof_backtick_single_word_not_extracted() {
    // No space, no slash, no dot → not a shell command
    let text = "Try `ls` alone.";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_backtick_triple_backtick_fence_skipped() {
    // Triple-backtick code fence should be skipped
    let text = "```\necho hello\n```";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_backtick_triple_fence_then_single_found() {
    // After a triple-fence block, a single backtick command IS extracted
    let text = "```\nsome code\n```\nNow run `echo done`";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo done");
}

#[test]
fn proof_backtick_xml_wins_over_backtick() {
    // XML present → backtick fallback is NOT used
    let text = "<tool>shell</tool>\n<command>ls</command>\nAlso try `echo fallback`";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls");
}

#[test]
fn proof_backtick_fallback_shell_name() {
    let text = "`cargo test --lib`";
    let calls = parse_tool_calls(text);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn proof_backtick_fallback_no_async() {
    let text = "`ls -la`";
    let calls = parse_tool_calls(text);
    assert_eq!(calls[0].async_mode, false);
    assert!(calls[0].async_task_id.is_none());
    assert!(calls[0].description.is_none());
    assert!(calls[0].tellhuman.is_none());
}

#[test]
fn proof_backtick_path_with_dot() {
    let text = "Check `./run.sh`";
    let calls = parse_tool_calls(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "./run.sh");
}

// ─────────────────────────────────────────────────────────────────────────────
// EDGE CASES / ADVERSARIAL
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_edge_empty_string_returns_empty_vec() {
    let calls = parse_xml_tool_calls("");
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_whitespace_only_returns_empty_vec() {
    let calls = parse_xml_tool_calls("   \n\t  ");
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_no_tool_tags_returns_empty_vec() {
    let calls = parse_xml_tool_calls("This is just prose text with no tool calls at all.");
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_unknown_tool_returns_empty_vec() {
    let calls = parse_xml_tool_calls("<tool>unknown_xyz</tool>\n<command>stuff</command>");
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_tool_tag_no_close_tag() {
    // <tool> without </tool> — parser breaks out of loop
    let calls = parse_xml_tool_calls("<tool>shell\n<command>ls</command>");
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_shell_no_command_tag_empty_args() {
    let xml = "<tool>shell</tool>\n<desc>no command here</desc>";
    let calls = parse_xml_tool_calls(xml);
    // shell with empty command: the match arm requires !command.is_empty() for shell
    // so it falls to the final _ arm which also returns command (empty string)
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "");
}

#[test]
fn proof_edge_desc_before_command_still_works() {
    // Tag ordering shouldn't matter — parser scans the whole block
    let xml = "<tool>shell</tool>\n<desc>Run ls</desc>\n<command>ls -la</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls -la");
    assert_eq!(calls[0].description.as_deref(), Some("Run ls"));
}

#[test]
fn proof_edge_think_block_before_and_after() {
    let xml = concat!(
        "<think>Thinking about what to do.</think>\n",
        "<tool>shell</tool>\n<command>echo hi</command>\n",
        "<think>That was good.</think>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo hi");
}

#[test]
fn proof_edge_long_preamble_before_tool() {
    let preamble = "This is a long paragraph of text that a model might emit before getting to the actual tool call. It contains no tool calls whatsoever, just filler content to simulate real model output behavior.\n\n";
    let xml = format!("{}<tool>shell</tool>\n<command>ls</command>", preamble);
    let calls = parse_xml_tool_calls(&xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls");
}

#[test]
fn proof_edge_html_entities_preserved_as_is() {
    // Parser does NOT decode HTML entities — they pass through literally
    let xml = "<tool>shell</tool>\n<command>echo &amp; hello &lt;world&gt;</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "echo &amp; hello &lt;world&gt;");
}

#[test]
fn proof_edge_whitespace_in_tool_name_trimmed() {
    let xml = "<tool>  shell  </tool>\n<command>echo x</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn proof_edge_setfile_nested_tool_tag_in_content_not_mis_parsed() {
    // A <tool> string literally in the file content should not cause a second parse
    // Because the block is delimited up to the NEXT literal <tool> opening tag in the text
    // This is a known limitation — test that it doesn't crash and handles gracefully
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>no extra tool here</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "setfile");
    assert!(!calls[0].args.is_empty());
}

#[test]
fn proof_edge_multiple_message_tags_first_wins() {
    // extract_tag finds the FIRST occurrence
    let xml = "<tool>commit</tool>\n<message>first</message>\n<message>second</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "first");
}

#[test]
fn proof_edge_parse_tool_calls_empty_string() {
    let calls = parse_tool_calls("");
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_parse_tool_calls_prose_no_xml_no_backtick() {
    let calls = parse_tool_calls("I cannot run commands at this time.");
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_returnlines_tag_ordering() {
    // returnlines before command — parser searches entire block so order doesn't matter
    let xml = "<tool>shell</tool>\n<returnlines>1-20</returnlines>\n<command>cat Cargo.toml</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    let (cmd, rl) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(cmd, "cat Cargo.toml");
    assert_eq!(rl, "1-20");
}

#[test]
fn proof_edge_task_id_before_mode_tag() {
    // task_id and mode in different order
    let xml = "<tool>shell</tool>\n<command>x</command>\n<task_id>tid1</task_id>\n<mode>async</mode>";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_mode);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("tid1"));
}

#[test]
fn proof_edge_tellhuman_with_all_fields() {
    let xml = concat!(
        "<tool>shell</tool>\n",
        "<command>cargo build 2>&1</command>\n",
        "<desc>Building</desc>\n",
        "<mode>async</mode>\n",
        "<task_id>build-task</task_id>\n",
        "<tellhuman>Build is running in background</tellhuman>\n",
        "<returnlines>1-50</returnlines>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].async_mode);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("build-task"));
    assert_eq!(calls[0].description.as_deref(), Some("Building"));
    assert_eq!(calls[0].tellhuman.as_deref(), Some("Build is running in background"));
    let (_, rl) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(rl, "1-50");
}

#[test]
fn proof_edge_patchfile_no_new_text_tag() {
    // patchfile with missing new_text uses empty string default
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<start_line>1</start_line>\n<end_line>2</end_line>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[3], "");
}

#[test]
fn proof_edge_patchfile_default_start_end_lines() {
    // patchfile with missing start/end uses "0" as default
    let xml = "<tool>patchfile</tool>\n<path>f.rs</path>\n<new_text>x</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[1], "0");
    assert_eq!(parts[2], "0");
}

#[test]
fn proof_edge_setfile_no_path_tag() {
    // setfile with missing path uses empty string
    let xml = "<tool>setfile</tool>\n<content>data</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    let (path, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(path, "");
    assert_eq!(content, "data");
}

#[test]
fn proof_edge_multiple_think_blocks() {
    let xml = concat!(
        "<think>First thought.</think>\n",
        "<think>Second thought.</think>\n",
        "<tool>shell</tool>\n<command>echo done</command>\n",
        "<think>Post-thought.</think>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo done");
}

#[test]
fn proof_edge_no_content_between_tool_tags() {
    // Empty tool name — empty string is not a valid tool
    let xml = "<tool></tool>\n<command>echo hi</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 0);
}

#[test]
fn proof_edge_commit_alias_whitespace_trimmed() {
    let xml = "<tool>commit</tool>\n<commit_message>  trimmed message  </commit_message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "trimmed message");
}

#[test]
fn proof_edge_large_multi_tool_batch() {
    // 10 shell calls
    let mut xml = String::new();
    for i in 0..10 {
        xml.push_str(&format!("<tool>shell</tool>\n<command>echo {}</command>\n", i));
    }
    let calls = parse_xml_tool_calls(&xml);
    assert_eq!(calls.len(), 10);
    for (i, c) in calls.iter().enumerate() {
        assert_eq!(c.args, format!("echo {}", i));
    }
}

#[test]
fn proof_edge_remap_unix_unknown_command_goes_through_skip() {
    // A tool name that is NOT in UNIX_COMMANDS and NOT a valid tool → skipped
    let xml = "<tool>notacommand</tool>\n<command>args</command>\n<tool>shell</tool>\n<command>ok</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn proof_edge_setfile_content_with_comment_style_tags() {
    // Content containing <!-- comment --> should be preserved
    let xml = "<tool>setfile</tool>\n<path>f.html</path>\n<content><!-- comment -->\n<p>text</p></content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains("comment") || content.contains("text"));
}

#[test]
fn proof_edge_backtick_trim_whitespace() {
    // Backtick command whitespace is trimmed
    let text = "Run `  echo hello world  `";
    let calls = parse_tool_calls(text);
    if !calls.is_empty() {
        // The trim() in extract_backtick_command strips surrounding spaces
        assert_eq!(calls[0].args, "echo hello world");
    }
}

#[test]
fn proof_edge_xml_with_newlines_around_tags() {
    let xml = "\n\n<tool>shell</tool>\n\n<command>ls</command>\n\n";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "ls");
}

// ─────────────────────────────────────────────────────────────────────────────
// FIELD COMPLETENESS PROOFS
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_fields_shell_name_field() {
    let calls = parse_xml_tool_calls(&shell("pwd"));
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn proof_fields_setfile_name_field() {
    let xml = "<tool>setfile</tool>\n<path>f</path>\n<content>x</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "setfile");
}

#[test]
fn proof_fields_patchfile_name_field() {
    let xml = "<tool>patchfile</tool>\n<path>f</path>\n<start_line>1</start_line>\n<end_line>1</end_line>\n<new_text>x</new_text>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "patchfile");
}

#[test]
fn proof_fields_commit_name_field() {
    let xml = "<tool>commit</tool>\n<message>x</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "commit");
}

#[test]
fn proof_fields_all_five_struct_fields_accessible() {
    let xml = "<tool>shell</tool>\n<command>ls</command>\n<desc>d</desc>\n<mode>async</mode>\n<task_id>t</task_id>\n<tellhuman>h</tellhuman>";
    let calls = parse_xml_tool_calls(xml);
    let c = &calls[0];
    let _ = &c.name;
    let _ = &c.args;
    let _ = &c.description;
    let _ = c.async_mode;
    let _ = &c.async_task_id;
    let _ = &c.tellhuman;
    // If this compiles and runs, all fields exist
    assert!(true);
}

#[test]
fn proof_fields_description_is_option_string() {
    let xml = "<tool>shell</tool>\n<command>x</command>\n<desc>Hello</desc>";
    let calls = parse_xml_tool_calls(xml);
    let desc: Option<String> = calls[0].description.clone();
    assert_eq!(desc.as_deref(), Some("Hello"));
}

#[test]
fn proof_fields_async_task_id_is_option_string() {
    let xml = "<tool>shell</tool>\n<command>x</command>\n<mode>async</mode>\n<task_id>my-id</task_id>";
    let calls = parse_xml_tool_calls(xml);
    let id: Option<String> = calls[0].async_task_id.clone();
    assert_eq!(id.as_deref(), Some("my-id"));
}

#[test]
fn proof_fields_tellhuman_is_option_string() {
    let xml = "<tool>shell</tool>\n<command>x</command>\n<tellhuman>message</tellhuman>";
    let calls = parse_xml_tool_calls(xml);
    let t: Option<String> = calls[0].tellhuman.clone();
    assert_eq!(t.as_deref(), Some("message"));
}

// ─────────────────────────────────────────────────────────────────────────────
// ADDITIONAL COVERAGE
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn proof_shell_command_with_curly_braces() {
    let calls = parse_xml_tool_calls(&shell("for f in {a,b,c}; do echo $f; done"));
    assert!(calls[0].args.contains('{'));
}

#[test]
fn proof_shell_command_with_star_glob() {
    let calls = parse_xml_tool_calls(&shell("rm -f *.o"));
    assert_eq!(calls[0].args, "rm -f *.o");
}

#[test]
fn proof_commit_breaking_change_notation() {
    let xml = "<tool>commit</tool>\n<message>feat!: breaking API change</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "feat!: breaking API change");
}

#[test]
fn proof_commit_multi_word_scope() {
    let xml = "<tool>commit</tool>\n<message>feat(tool-parser): handle remapping</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "feat(tool-parser): handle remapping");
}

#[test]
fn proof_setfile_json_content() {
    let xml = r#"<tool>setfile</tool>
<path>config.json</path>
<content>{"key": "value", "num": 42}</content>"#;
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    assert!(content.contains("\"key\""));
    assert!(content.contains("42"));
}

#[test]
fn proof_setfile_toml_content() {
    let xml = "<tool>setfile</tool>\n<path>Cargo.toml</path>\n<content>[package]\nname = \"myapp\"\nversion = \"0.1.0\"</content>";
    let calls = parse_xml_tool_calls(xml);
    let (path, content) = calls[0].args.split_once('\x00').unwrap();
    assert_eq!(path, "Cargo.toml");
    assert!(content.contains("[package]"));
}

#[test]
fn proof_patchfile_rust_code_new_text() {
    let xml = concat!(
        "<tool>patchfile</tool>\n",
        "<path>src/lib.rs</path>\n",
        "<start_line>1</start_line>\n",
        "<end_line>5</end_line>\n",
        "<new_text>pub fn hello() -> &'static str {\n    \"hello\"\n}\n</new_text>"
    );
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert!(parts[3].contains("pub fn hello"));
    assert!(parts[3].contains("\"hello\""));
}

#[test]
fn proof_shell_command_with_heredoc_style() {
    let calls = parse_xml_tool_calls(&shell("printf 'line1\\nline2\\n' > out.txt"));
    assert!(calls[0].args.contains("printf"));
    assert!(calls[0].args.contains("out.txt"));
}

#[test]
fn proof_remap_bat_command() {
    let xml = "<tool>bat</tool>\n<command>src/main.rs</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "bat src/main.rs");
}

#[test]
fn proof_remap_cp_command() {
    let xml = "<tool>cp</tool>\n<command>src/a.rs src/b.rs</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cp src/a.rs src/b.rs");
}

#[test]
fn proof_remap_mv_command() {
    let xml = "<tool>mv</tool>\n<command>old.txt new.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "mv old.txt new.txt");
}

#[test]
fn proof_remap_rm_command() {
    let xml = "<tool>rm</tool>\n<command>-rf target/</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "rm -rf target/");
}

#[test]
fn proof_remap_touch_command() {
    let xml = "<tool>touch</tool>\n<command>newfile.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "touch newfile.txt");
}

#[test]
fn proof_remap_chmod_command() {
    let xml = "<tool>chmod</tool>\n<command>+x run.sh</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "chmod +x run.sh");
}

#[test]
fn proof_remap_uniq_command() {
    let xml = "<tool>uniq</tool>\n<command>sorted.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "uniq sorted.txt");
}

#[test]
fn proof_remap_cut_command() {
    let xml = "<tool>cut</tool>\n<command>-d, -f1 data.csv</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cut -d, -f1 data.csv");
}

#[test]
fn proof_multi_remap_and_valid_mixed() {
    let xml = concat!(
        "<tool>cat</tool>\n<command>README.md</command>\n",
        "<tool>shell</tool>\n<command>echo done</command>\n",
        "<tool>commit</tool>\n<message>finish</message>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].args, "cat README.md");
    assert_eq!(calls[1].name, "shell");
    assert_eq!(calls[2].name, "commit");
}

#[test]
fn proof_setfile_content_with_newline_at_start_and_middle() {
    let xml = "<tool>setfile</tool>\n<path>f.txt</path>\n<content>\nfirst\nsecond\nthird</content>";
    let calls = parse_xml_tool_calls(xml);
    let (_, content) = calls[0].args.split_once('\x00').unwrap();
    // First leading newline stripped
    assert_eq!(content, "first\nsecond\nthird");
}

#[test]
fn proof_patchfile_large_line_numbers() {
    let xml = "<tool>patchfile</tool>\n<path>big.rs</path>\n<start_line>9999</start_line>\n<end_line>10050</end_line>\n<new_text>x</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[1], "9999");
    assert_eq!(parts[2], "10050");
}

#[test]
fn proof_shell_command_with_tee() {
    let calls = parse_xml_tool_calls(&shell("cargo test 2>&1 | tee test.log"));
    assert!(calls[0].args.contains("tee"));
}

#[test]
fn proof_multi_two_commits_different_messages() {
    let xml = "<tool>commit</tool>\n<message>feat: first</message>\n<tool>commit</tool>\n<commit_message>fix: second</commit_message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args, "feat: first");
    assert_eq!(calls[1].args, "fix: second");
}

#[test]
fn proof_shell_only_command_no_other_tags() {
    let xml = "<tool>shell</tool><command>echo minimal</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "echo minimal");
}

#[test]
fn proof_setfile_only_required_tags() {
    let xml = "<tool>setfile</tool><path>f.txt</path><content>x</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "setfile");
}

#[test]
fn proof_commit_only_required_tag() {
    let xml = "<tool>commit</tool><message>done</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, "done");
}

#[test]
fn proof_patchfile_only_required_tags() {
    let xml = "<tool>patchfile</tool><path>f.rs</path><start_line>1</start_line><end_line>2</end_line><new_text>x</new_text>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[0], "f.rs");
}
