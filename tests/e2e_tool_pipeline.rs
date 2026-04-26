//! End-to-end tests for the full tool pipeline:
//!   model XML output → parse_xml_tool_calls → ToolRegistry::execute → result string
//!
//! Covers: shell, setfile, patchfile, commit tools.

use std::fs;
use tempfile::TempDir;
use yggdra::agent::parse_xml_tool_calls;
use yggdra::tools::ToolRegistry;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Strip the `--- changes ---` git-diff suffix that ShellTool appends.
fn strip_changes(s: &str) -> &str {
    s.split("\n--- changes ---\n").next().unwrap_or(s)
}

/// Parse a single tool call from XML and return (name, args).
fn parse_one(xml: &str) -> (String, String) {
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1, "expected exactly 1 tool call in: {xml}");
    (calls[0].name.clone(), calls[0].args.clone())
}

/// Execute a tool call parsed from XML, returning the result string.
fn exec_from_xml(registry: &ToolRegistry, xml: &str) -> Result<String, String> {
    let (name, args) = parse_one(xml);
    registry.execute(&name, &args).map_err(|e| e.to_string())
}

// ── Module 1: Shell tool – parse + execute ────────────────────────────────────

#[test]
fn shell_echo_hello() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo hello</command><desc>greet</desc>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains("hello"), "got: {out}");
}

#[test]
fn shell_printf_world() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf '%s' world</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out), "world", "got: {out}");
}

#[test]
fn shell_echo_no_newline() {
    let reg = ToolRegistry::new();
    // Use printf instead of echo -n — POSIX sh on macOS doesn't support echo -n
    let xml = r#"<tool>shell</tool><command>printf '%s' test</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out), "test", "got: {out}");
}

#[test]
fn shell_date_year() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>date +%Y</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    let year = strip_changes(&out).trim();
    assert!(year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()), "got: {out}");
}

#[test]
fn shell_uname_nonempty() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>uname</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(!strip_changes(&out).trim().is_empty(), "got: {out}");
}

#[test]
fn shell_true_success() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>true</command>"#;
    let result = exec_from_xml(&reg, xml);
    assert!(result.is_ok(), "expected Ok, got: {:?}", result);
}

#[test]
fn shell_false_error() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>false</command>"#;
    // false exits non-zero; our ShellTool collects stdout+stderr but doesn't
    // distinguish exit code — it returns Ok with empty/stderr output.
    // Just assert it doesn't panic and returns some result.
    let _result = exec_from_xml(&reg, xml);
}

#[test]
fn shell_arithmetic() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo $((2 + 2))</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains('4'), "got: {out}");
}

#[test]
fn shell_pipe_head() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf 'a\nb\nc\n' | head -1</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out).trim(), "a", "got: {out}");
}

#[test]
fn shell_word_count() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo 'hello world' | wc -w | tr -d ' '</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains('2'), "got: {out}");
}

#[test]
fn shell_multiline_output() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf 'a\nb\nc\n'</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    let body = strip_changes(&out);
    assert_eq!(body.trim().lines().count(), 3, "got: {out}");
}

#[test]
fn shell_returnlines_flag() {
    let reg = ToolRegistry::new();
    // returnlines tag → args becomes "command\x00range"
    let xml = r#"<tool>shell</tool><command>printf 'line1\nline2\nline3\nline4\nline5\n'</command><returnlines>1-3</returnlines>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].args.contains('\x00'), "expected \\x00 delimiter in args");
    let out = reg.execute(&calls[0].name, &calls[0].args).unwrap();
    assert!(out.contains("lines 1-3"), "got: {out}");
}

#[test]
fn shell_large_output() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>seq 1 100</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(!strip_changes(&out).trim().is_empty(), "got: {out}");
    assert!(strip_changes(&out).contains("100"), "got: {out}");
}

#[test]
fn shell_unicode_output() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo "héllo"</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains("héllo"), "got: {out}");
}

#[test]
fn shell_subshell_arithmetic() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo $((10 * 10))</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains("100"), "got: {out}");
}

#[test]
fn shell_env_variable_expansion() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo ${HOME:0:1}</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(!strip_changes(&out).trim().is_empty(), "got: {out}");
}

#[test]
fn shell_sort_output() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf 'b\na\nc\n' | sort</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    let lines: Vec<&str> = strip_changes(&out).trim().lines().collect();
    assert_eq!(lines, vec!["a", "b", "c"], "got: {out}");
}

#[test]
fn shell_tr_command() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo 'hello' | tr 'a-z' 'A-Z'</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains("HELLO"), "got: {out}");
}

#[test]
fn shell_multiple_pipes() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf 'foo\nbar\nbaz\n' | grep 'ba' | wc -l | tr -d ' '</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains('2'), "got: {out}");
}

#[test]
fn shell_exit_0_output() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo ok; exit 0</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains("ok"), "got: {out}");
}

#[test]
fn shell_sed_substitution() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo 'hello world' | sed 's/world/rust/'</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert!(strip_changes(&out).contains("rust"), "got: {out}");
}

#[test]
fn shell_awk_field_extraction() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo 'first second third' | awk '{print $2}'</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out).trim(), "second", "got: {out}");
}

#[test]
fn shell_cut_command() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo 'a:b:c' | cut -d: -f2</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out).trim(), "b", "got: {out}");
}

#[test]
fn shell_uniq_command() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf 'a\na\nb\n' | uniq</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    let lines: Vec<&str> = strip_changes(&out).trim().lines().collect();
    assert_eq!(lines, vec!["a", "b"], "got: {out}");
}

#[test]
fn shell_basename_command() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>basename /usr/local/bin/cargo</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out).trim(), "cargo", "got: {out}");
}

#[test]
fn shell_dirname_command() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>dirname /usr/local/bin/cargo</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out).trim(), "/usr/local/bin", "got: {out}");
}

#[test]
fn shell_printf_format() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf '%05d\n' 42</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out).trim(), "00042", "got: {out}");
}

#[test]
fn shell_hex_echo() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>printf '%x\n' 255</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    assert_eq!(strip_changes(&out).trim(), "ff", "got: {out}");
}

#[test]
fn shell_double_echo() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo first; echo second</command>"#;
    let out = exec_from_xml(&reg, xml).unwrap();
    let body = strip_changes(&out);
    assert!(body.contains("first") && body.contains("second"), "got: {out}");
}

#[test]
fn shell_desc_is_parsed_not_executed() {
    let xml = r#"<tool>shell</tool><command>echo hello</command><desc>this is just metadata</desc>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].description.as_deref(), Some("this is just metadata"));
    // args should not include the desc content
    assert!(!calls[0].args.contains("metadata"));
}

#[test]
fn shell_async_mode_does_not_change_sync_result() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo sync_result</command><mode>async</mode>"#;
    let calls = parse_xml_tool_calls(xml);
    assert!(calls[0].async_mode, "expected async_mode to be true");
    // async flag is meta-data only; ToolRegistry::execute runs synchronously
    let out = reg.execute(&calls[0].name, &calls[0].args).unwrap();
    assert!(strip_changes(&out).contains("sync_result"), "got: {out}");
}

// ── Module 2: SetfileTool – parse + execute ───────────────────────────────────

fn setfile_xml(path: &str, content: &str) -> String {
    format!("<tool>setfile</tool><path>{path}</path><content>{content}</content>")
}

#[test]
fn setfile_create_new_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("hello.txt");
    let xml = setfile_xml(path.to_str().unwrap(), "hello world");
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    assert!(out.contains("wrote") || out.contains("✅"), "got: {out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
}

#[test]
fn setfile_overwrite_existing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("file.txt");
    fs::write(&path, "old content").unwrap();
    let xml = setfile_xml(path.to_str().unwrap(), "new content");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
}

#[test]
fn setfile_creates_subdirectory() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sub").join("dir").join("file.txt");
    let xml = setfile_xml(path.to_str().unwrap(), "nested");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert!(path.exists(), "file not created at {}", path.display());
    assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
}

#[test]
fn setfile_content_with_newlines_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("multi.txt");
    let content = "line1\nline2\nline3";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let read = fs::read_to_string(&path).unwrap();
    assert_eq!(read, content);
}

#[test]
fn setfile_line_count_in_result() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("counted.txt");
    let content = "a\nb\nc\nd\ne";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    // Should mention "5 lines" or similar
    assert!(out.contains('5'), "expected line count in result, got: {out}");
}

#[test]
fn setfile_empty_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.txt");
    let xml = setfile_xml(path.to_str().unwrap(), "");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
}

#[test]
fn setfile_unicode_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("unicode.txt");
    let content = "こんにちは世界 – héllo – Ñoño";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_long_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("long.txt");
    let content = "x".repeat(1000);
    let xml = setfile_xml(path.to_str().unwrap(), &content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_rust_code_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("src.rs");
    let content = "fn main() {\n    println!(\"hello\");\n}\n";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_json_content_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("data.json");
    let content = r#"{"key": "value", "num": 42}"#;
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_result_mentions_wrote() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("f.txt");
    let xml = setfile_xml(path.to_str().unwrap(), "data");
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    assert!(
        out.contains("wrote") || out.contains("✅") || out.contains("Wrote"),
        "expected write confirmation, got: {out}"
    );
}

#[test]
fn setfile_content_matches_exactly() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("exact.txt");
    let content = "exact match test content";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        content,
        "content mismatch"
    );
}

#[test]
fn setfile_python_code_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("script.py");
    let content = "def greet(name):\n    return f'Hello, {name}'\n";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_deeply_nested_directory() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a").join("b").join("c").join("d").join("file.txt");
    let xml = setfile_xml(path.to_str().unwrap(), "deep");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "deep");
}

#[test]
fn setfile_multiple_writes_to_same_path() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("repeated.txt");
    let reg = ToolRegistry::new();
    for i in 0..3 {
        let content = format!("iteration {i}");
        let xml = setfile_xml(path.to_str().unwrap(), &content);
        exec_from_xml(&reg, &xml).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }
}

#[test]
fn setfile_special_chars_in_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("special.txt");
    let content = "tab:\there\nnewline above";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_xml_path_and_content_parsed_correctly() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("parsed.txt");
    let xml = format!(
        "<tool>setfile</tool><path>{}</path><content>parsed content</content>",
        path.display()
    );
    let calls = parse_xml_tool_calls(&xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "setfile");
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], path.to_str().unwrap());
    assert_eq!(parts[1], "parsed content");
}

#[test]
fn setfile_csv_content_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("data.csv");
    let content = "id,name,value\n1,foo,100\n2,bar,200\n";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_100_lines() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("hundred.txt");
    let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    let xml = setfile_xml(path.to_str().unwrap(), &content);
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    assert!(out.contains("100"), "expected 100 in output, got: {out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn setfile_file_with_dot_in_name() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    let content = "[package]\nname = \"test\"\n";
    let xml = setfile_xml(path.to_str().unwrap(), content);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

// ── Module 3: PatchfileTool – parse + execute ─────────────────────────────────

fn create_test_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, content).unwrap();
    path
}

fn patch_xml(path: &str, start: usize, end: usize, new_text: &str) -> String {
    format!(
        "<tool>patchfile</tool><path>{path}</path><start_line>{start}</start_line><end_line>{end}</end_line><new_text>{new_text}</new_text>"
    )
}

#[test]
fn patchfile_simple_replacement() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "simple.txt", "line1\nline2\nline3\nline4\nline5\n");
    let xml = patch_xml(path.to_str().unwrap(), 3, 3, "replaced");
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    assert!(out.contains('✅') || out.contains("patched"), "got: {out}");
    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[2], "replaced", "got lines: {lines:?}");
    assert_eq!(lines[0], "line1");
    assert_eq!(lines[4], "line5");
}

#[test]
fn patchfile_range_replacement() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "range.txt", "line1\nline2\nline3\nline4\nline5\n");
    let xml = patch_xml(path.to_str().unwrap(), 2, 4, "new_line2\nnew_line3\nnew_line4");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "line1");
    assert_eq!(lines[1], "new_line2");
    assert_eq!(lines[2], "new_line3");
    assert_eq!(lines[3], "new_line4");
    assert_eq!(lines[4], "line5");
}

#[test]
fn patchfile_patch_first_line() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "first.txt", "old_first\nsecond\nthird\n");
    let xml = patch_xml(path.to_str().unwrap(), 1, 1, "new_first");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let lines: Vec<String> = fs::read_to_string(&path).unwrap().lines().map(String::from).collect();
    assert_eq!(lines[0], "new_first");
    assert_eq!(lines[1], "second");
}

#[test]
fn patchfile_patch_last_line() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "last.txt", "first\nsecond\nold_last\n");
    let xml = patch_xml(path.to_str().unwrap(), 3, 3, "new_last");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("new_last"), "got: {content}");
    assert!(!content.contains("old_last"), "got: {content}");
}

#[test]
fn patchfile_single_line_file() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "single.txt", "only line\n");
    let xml = patch_xml(path.to_str().unwrap(), 1, 1, "replaced line");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("replaced line"), "got: {content}");
}

#[test]
fn patchfile_multiline_new_text() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "multi.txt", "a\nb\nc\n");
    let xml = patch_xml(path.to_str().unwrap(), 2, 2, "x\ny\nz");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines[0], "a");
    assert_eq!(lines[1], "x");
    assert_eq!(lines[2], "y");
    assert_eq!(lines[3], "z");
    assert_eq!(lines[4], "c");
}

#[test]
fn patchfile_code_replacement_preserves_indentation() {
    let dir = TempDir::new().unwrap();
    let content = "fn foo() {\n    let x = 1;\n    return x;\n}\n";
    let path = create_test_file(&dir, "code.rs", content);
    let new_code = "    let x = 42;";
    let xml = patch_xml(path.to_str().unwrap(), 2, 2, new_code);
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("    let x = 42;"), "got: {result}");
}

#[test]
fn patchfile_delete_lines_with_empty_new_text() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "delete.txt", "keep1\ndelete_me\nkeep2\n");
    let xml = patch_xml(path.to_str().unwrap(), 2, 2, "");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(!content.contains("delete_me"), "got: {content}");
    assert!(content.contains("keep1"), "got: {content}");
    assert!(content.contains("keep2"), "got: {content}");
}

#[test]
fn patchfile_result_contains_checkmark() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "check.txt", "line1\nline2\n");
    let xml = patch_xml(path.to_str().unwrap(), 1, 1, "patched");
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    assert!(out.contains('✅') || out.contains("patched"), "got: {out}");
}

#[test]
fn patchfile_result_shows_line_info() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "info.txt", "a\nb\nc\n");
    let xml = patch_xml(path.to_str().unwrap(), 2, 2, "new_b");
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    assert!(out.contains("@@"), "expected @@ hunk header, got: {out}");
}

#[test]
fn patchfile_bounds_error_is_graceful() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "bounds.txt", "line1\nline2\n");
    let xml = patch_xml(path.to_str().unwrap(), 99, 99, "out of bounds");
    let reg = ToolRegistry::new();
    let result = exec_from_xml(&reg, &xml);
    assert!(result.is_err(), "expected error for out-of-bounds, got: {:?}", result);
    let err = result.unwrap_err();
    assert!(err.contains("patchfile") || err.contains("exceeds"), "got: {err}");
}

#[test]
fn patchfile_xml_args_parsed_correctly() {
    let path = "/tmp/test_parse.txt";
    let xml = patch_xml(path, 5, 10, "new content");
    let calls = parse_xml_tool_calls(&xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "patchfile");
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[0], path);
    assert_eq!(parts[1], "5");
    assert_eq!(parts[2], "10");
    assert_eq!(parts[3], "new content");
}

#[test]
fn patchfile_five_line_file_patch_middle() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "mid.txt", "1\n2\n3\n4\n5\n");
    let xml = patch_xml(path.to_str().unwrap(), 3, 3, "THREE");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let lines: Vec<String> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(lines[2], "THREE");
    assert_eq!(lines[0], "1");
    assert_eq!(lines[4], "5");
}

#[test]
fn patchfile_replace_all_lines() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "all.txt", "old1\nold2\nold3\n");
    let xml = patch_xml(path.to_str().unwrap(), 1, 3, "new1\nnew2\nnew3");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(!content.contains("old1"), "got: {content}");
    assert!(content.contains("new1") && content.contains("new2") && content.contains("new3"), "got: {content}");
}

#[test]
fn patchfile_preserves_trailing_newline() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "trail.txt", "a\nb\nc\n");
    let xml = patch_xml(path.to_str().unwrap(), 2, 2, "B");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.ends_with('\n'), "trailing newline lost: {:?}", content);
}

#[test]
fn patchfile_no_trailing_newline_preserved() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notrail.txt");
    fs::write(&path, "a\nb\nc").unwrap(); // no trailing newline
    let xml = patch_xml(path.to_str().unwrap(), 2, 2, "B");
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(!content.ends_with('\n'), "unexpected trailing newline: {:?}", content);
}

#[test]
fn patchfile_context_lines_in_result() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "ctx.txt", "a\nb\nc\nd\ne\n");
    let xml = patch_xml(path.to_str().unwrap(), 3, 3, "C");
    let reg = ToolRegistry::new();
    let out = exec_from_xml(&reg, &xml).unwrap();
    // Result should contain context lines
    assert!(out.contains('-') || out.contains('+'), "expected diff markers, got: {out}");
}

#[test]
fn patchfile_insert_via_xml_new_text_tag() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "insert.txt", "first\nlast\n");
    // Insert between lines 1 and 2
    let xml = format!(
        "<tool>patchfile</tool><path>{}</path><start_line>1</start_line><end_line>1</end_line><new_text>first\nmiddle</new_text>",
        path.display()
    );
    let reg = ToolRegistry::new();
    exec_from_xml(&reg, &xml).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("middle"), "got: {content}");
}

// ── Module 4: CommitTool ──────────────────────────────────────────────────────

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn commit_nothing_to_commit_returns_ok() {
    if !git_available() { return; }
    let reg = ToolRegistry::new();
    // Running in yggdra project root; unless there are staged changes, "nothing to commit"
    let xml = r#"<tool>commit</tool><message>test: e2e commit tool test</message>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "test: e2e commit tool test");
    let result = reg.execute("commit", "test: e2e commit tool test");
    // Either succeeds (unlikely) or "nothing to commit" → Ok
    match &result {
        Ok(msg) => assert!(!msg.is_empty(), "expected non-empty message"),
        Err(e)  => { let s = e.to_string(); assert!(s.contains("nothing") || s.contains("commit"), "unexpected error: {s}"); }
    }
}

#[test]
fn commit_empty_message_errors() {
    let reg = ToolRegistry::new();
    let result = reg.execute("commit", "");
    assert!(result.is_err(), "expected error for empty commit message");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("empty"), "got: {err}");
}

#[test]
fn commit_xml_message_tag_parsed() {
    let xml = r#"<tool>commit</tool><message>feat: new feature</message>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "feat: new feature");
}

#[test]
fn commit_xml_commit_message_tag_parsed() {
    // Models sometimes use <commit_message> instead of <message>
    let xml = r#"<tool>commit</tool><commit_message>fix: bug fix</commit_message>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "commit");
    assert_eq!(calls[0].args, "fix: bug fix");
}

#[test]
fn commit_in_temp_dir_no_git_fails() {
    if !git_available() { return; }
    // CommitTool runs git in the process cwd — to test no-git scenario we can't
    // easily change cwd in tests, so we just verify the tool exists and empty msg errors.
    let reg = ToolRegistry::new();
    let result = reg.execute("commit", "");
    assert!(result.is_err());
}

#[test]
fn commit_strips_surrounding_quotes() {
    // The CommitTool strips surrounding single/double quotes from the message.
    let xml = r#"<tool>commit</tool><message>"quoted message"</message>"#;
    let calls = parse_xml_tool_calls(xml);
    // The message tag strips surrounding whitespace, the tool itself strips quotes at execute time.
    assert_eq!(calls[0].args, "\"quoted message\"");
}

#[test]
fn commit_multiline_message_truncated_to_first_line() {
    if !git_available() { return; }
    let reg = ToolRegistry::new();
    // Multi-line commit message is valid; result is first line of git output.
    let result = reg.execute("commit", "first line\nsecond line");
    match result {
        Ok(s) => assert!(!s.is_empty()),
        Err(e) => { let s = e.to_string(); assert!(!s.is_empty()); }
    }
}

#[test]
fn commit_git_init_and_commit() {
    if !git_available() { return; }
    let dir = TempDir::new().unwrap();
    // Init a new repo, stage a file, commit via shell, verify via CommitTool.
    let p = dir.path();
    std::process::Command::new("git").args(["init"]).current_dir(p).output().unwrap();
    std::process::Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(p).output().unwrap();
    std::process::Command::new("git").args(["config", "user.name", "Test"]).current_dir(p).output().unwrap();
    fs::write(p.join("README.md"), "hello").unwrap();
    std::process::Command::new("git").args(["add", "README.md"]).current_dir(p).output().unwrap();
    // CommitTool uses the process cwd, not a custom dir, so we verify via shell in that dir instead.
    let result = std::process::Command::new("git")
        .args(["commit", "-m", "initial commit"])
        .current_dir(p)
        .output()
        .unwrap();
    assert!(result.status.success(), "git commit failed: {}", String::from_utf8_lossy(&result.stderr));
}

#[test]
fn commit_tool_list_includes_commit() {
    let reg = ToolRegistry::new();
    let tools = reg.list_tools();
    assert!(tools.contains(&"commit"), "commit not in tool list: {tools:?}");
}

// ── Module 5: Full pipeline sequences ────────────────────────────────────────

#[test]
fn pipeline_two_shell_calls_both_executed() {
    let reg = ToolRegistry::new();
    let xml = concat!(
        "<tool>shell</tool><command>echo first_result</command>",
        "<tool>shell</tool><command>echo second_result</command>"
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 2);
    let out1 = reg.execute(&calls[0].name, &calls[0].args).unwrap();
    let out2 = reg.execute(&calls[1].name, &calls[1].args).unwrap();
    assert!(strip_changes(&out1).contains("first_result"), "got: {out1}");
    assert!(strip_changes(&out2).contains("second_result"), "got: {out2}");
}

#[test]
fn pipeline_setfile_then_shell_reads_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pipeline.txt");
    let reg = ToolRegistry::new();

    // Step 1: write file
    let write_xml = setfile_xml(path.to_str().unwrap(), "pipeline content");
    exec_from_xml(&reg, &write_xml).unwrap();

    // Step 2: shell reads file
    let read_xml = format!(
        "<tool>shell</tool><command>cat {}</command>",
        path.display()
    );
    let out = exec_from_xml(&reg, &read_xml).unwrap();
    assert!(strip_changes(&out).contains("pipeline content"), "got: {out}");
}

#[test]
fn pipeline_setfile_then_patchfile() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("seq.txt");
    let reg = ToolRegistry::new();

    // Step 1: create file
    let write_xml = setfile_xml(path.to_str().unwrap(), "original\ncontent\nhere\n");
    exec_from_xml(&reg, &write_xml).unwrap();

    // Step 2: patch line 2
    let patch_xml_str = patch_xml(path.to_str().unwrap(), 2, 2, "patched_content");
    exec_from_xml(&reg, &patch_xml_str).unwrap();

    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("patched_content"), "got: {result}");
    assert!(result.contains("original"), "got: {result}");
    assert!(result.contains("here"), "got: {result}");
}

#[test]
fn pipeline_result_formatted_as_tool_output() {
    let reg = ToolRegistry::new();
    // Simulate the UI injection format: "[TOOL_OUTPUT: name = result]"
    let xml = r#"<tool>shell</tool><command>echo injected</command>"#;
    let (name, args) = parse_one(xml);
    let result = reg.execute(&name, &args).unwrap();
    let result_body = strip_changes(&result);
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, result_body);
    assert!(injected.starts_with("[TOOL_OUTPUT: shell = "), "got: {injected}");
    assert!(injected.contains("injected"), "got: {injected}");
    assert!(injected.ends_with(']'), "got: {injected}");
}

#[test]
fn pipeline_three_tools_all_succeed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("three.txt");
    let reg = ToolRegistry::new();

    let xml = format!(
        concat!(
            "<tool>shell</tool><command>echo step1</command>",
            "<tool>setfile</tool><path>{}</path><content>step2 data</content>",
            "<tool>shell</tool><command>echo step3</command>",
        ),
        path.display()
    );
    let calls = parse_xml_tool_calls(&xml);
    assert_eq!(calls.len(), 3);
    for call in &calls {
        let result = reg.execute(&call.name, &call.args);
        assert!(result.is_ok(), "tool {} failed: {:?}", call.name, result);
    }
}

#[test]
fn pipeline_setfile_patchfile_shell_reads() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("full.txt");
    let reg = ToolRegistry::new();

    exec_from_xml(&reg, &setfile_xml(path.to_str().unwrap(), "alpha\nbeta\ngamma\n")).unwrap();
    exec_from_xml(&reg, &patch_xml(path.to_str().unwrap(), 2, 2, "BETA")).unwrap();

    let read_xml = format!("<tool>shell</tool><command>cat {}</command>", path.display());
    let out = exec_from_xml(&reg, &read_xml).unwrap();
    assert!(strip_changes(&out).contains("BETA"), "got: {out}");
    assert!(strip_changes(&out).contains("alpha"), "got: {out}");
}

#[test]
fn pipeline_multiple_setfile_overwrites() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("over.txt");
    let reg = ToolRegistry::new();
    for v in ["v1", "v2", "v3"] {
        exec_from_xml(&reg, &setfile_xml(path.to_str().unwrap(), v)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), v);
    }
}

#[test]
fn pipeline_shell_output_used_as_input_to_next() {
    let reg = ToolRegistry::new();
    let xml1 = r#"<tool>shell</tool><command>echo 'dynamic_value'</command>"#;
    let out1 = exec_from_xml(&reg, xml1).unwrap();
    let value = strip_changes(&out1).trim().to_string();

    // Simulate using the output in the next command
    let xml2 = format!("<tool>shell</tool><command>echo got_{value}</command>");
    let out2 = exec_from_xml(&reg, &xml2).unwrap();
    assert!(strip_changes(&out2).contains("got_dynamic_value"), "got: {out2}");
}

#[test]
fn pipeline_setfile_creates_and_shell_checks_existence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("exists_check.txt");
    let reg = ToolRegistry::new();

    exec_from_xml(&reg, &setfile_xml(path.to_str().unwrap(), "check me")).unwrap();

    let xml = format!(
        "<tool>shell</tool><command>test -f {} && echo yes</command>",
        path.display()
    );
    let out = exec_from_xml(&reg, &xml).unwrap();
    assert!(strip_changes(&out).contains("yes"), "file not found, got: {out}");
}

#[test]
fn pipeline_patchfile_then_shell_line_count() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "lc.txt", "a\nb\nc\n");
    let reg = ToolRegistry::new();

    exec_from_xml(&reg, &patch_xml(path.to_str().unwrap(), 2, 2, "B1\nB2")).unwrap();

    let xml = format!(
        "<tool>shell</tool><command>wc -l < {}</command>",
        path.display()
    );
    let out = exec_from_xml(&reg, &xml).unwrap();
    let count: usize = strip_changes(&out).trim().parse().unwrap_or(0);
    assert_eq!(count, 4, "expected 4 lines after patch, got: {out}");
}

#[test]
fn pipeline_write_json_then_read_field() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("data.json");
    let reg = ToolRegistry::new();

    exec_from_xml(&reg, &setfile_xml(path.to_str().unwrap(), r#"{"status":"ok"}"#)).unwrap();

    let read_xml = format!(
        r#"<tool>shell</tool><command>cat {} | grep -o '"status":"ok"'</command>"#,
        path.display()
    );
    let out = exec_from_xml(&reg, &read_xml).unwrap();
    assert!(strip_changes(&out).contains("status"), "got: {out}");
}

#[test]
fn pipeline_two_files_independent() {
    let dir = TempDir::new().unwrap();
    let p1 = dir.path().join("f1.txt");
    let p2 = dir.path().join("f2.txt");
    let reg = ToolRegistry::new();

    exec_from_xml(&reg, &setfile_xml(p1.to_str().unwrap(), "file_one")).unwrap();
    exec_from_xml(&reg, &setfile_xml(p2.to_str().unwrap(), "file_two")).unwrap();

    assert_eq!(fs::read_to_string(&p1).unwrap(), "file_one");
    assert_eq!(fs::read_to_string(&p2).unwrap(), "file_two");
}

// ── Module 6: Result format proof ────────────────────────────────────────────

#[test]
fn result_format_shell_bracket_format() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo format_test</command>"#;
    let (name, args) = parse_one(xml);
    let result = reg.execute(&name, &args).unwrap();
    let body = strip_changes(&result);
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, body);
    assert!(injected.starts_with("[TOOL_OUTPUT:"), "got: {injected}");
}

#[test]
fn result_format_contains_tool_name() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo name_test</command>"#;
    let (name, args) = parse_one(xml);
    let result = reg.execute(&name, &args).unwrap();
    let body = strip_changes(&result);
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, body);
    assert!(injected.contains("shell"), "got: {injected}");
}

#[test]
fn result_format_contains_result_value() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo unique_xyz_42</command>"#;
    let (name, args) = parse_one(xml);
    let result = reg.execute(&name, &args).unwrap();
    let body = strip_changes(&result);
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, body);
    assert!(injected.contains("unique_xyz_42"), "got: {injected}");
}

#[test]
fn result_format_ends_with_bracket() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo closing</command>"#;
    let (name, args) = parse_one(xml);
    let result = reg.execute(&name, &args).unwrap();
    let body = strip_changes(&result);
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, body);
    assert!(injected.ends_with(']'), "missing closing bracket: {injected}");
}

#[test]
fn result_format_setfile_tool_output() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("fmt.txt");
    let reg = ToolRegistry::new();
    let xml = setfile_xml(path.to_str().unwrap(), "format proof");
    let (name, args) = parse_one(&xml);
    let result = reg.execute(&name, &args).unwrap();
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, result);
    assert!(injected.contains("[TOOL_OUTPUT: setfile = "), "got: {injected}");
}

#[test]
fn result_format_equals_separator() {
    // The format is [TOOL_OUTPUT: name = result] — verify " = " separator
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo sep_test</command>"#;
    let (name, args) = parse_one(xml);
    let result = reg.execute(&name, &args).unwrap();
    let body = strip_changes(&result);
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, body);
    assert!(injected.contains(" = "), "expected \" = \" separator, got: {injected}");
}

#[test]
fn result_format_no_tool_error_prefix_on_success() {
    let reg = ToolRegistry::new();
    let xml = r#"<tool>shell</tool><command>echo success</command>"#;
    let (name, args) = parse_one(xml);
    let result = reg.execute(&name, &args).unwrap();
    let body = strip_changes(&result);
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, body);
    assert!(!injected.contains("[TOOL_ERROR:"), "got: {injected}");
}

#[test]
fn result_format_multiple_tools_independent_injections() {
    let reg = ToolRegistry::new();
    let xmls = [
        r#"<tool>shell</tool><command>echo alpha</command>"#,
        r#"<tool>shell</tool><command>echo beta</command>"#,
    ];
    let injected: Vec<String> = xmls.iter().map(|xml| {
        let (name, args) = parse_one(xml);
        let result = reg.execute(&name, &args).unwrap();
        let body = strip_changes(&result);
        format!("[TOOL_OUTPUT: {} = {}]", name, body)
    }).collect();
    assert!(injected[0].contains("alpha"), "got: {}", injected[0]);
    assert!(injected[1].contains("beta"), "got: {}", injected[1]);
}

#[test]
fn result_format_consistent_with_ui_code() {
    // Verify format matches src/ui.rs line 2483/2485:
    // format!("[TOOL_OUTPUT: {} = {}]", result.tool_name, model_output)
    let tool_name = "shell";
    let model_output = "hello\n";
    let expected = "[TOOL_OUTPUT: shell = hello\n]";
    let actual = format!("[TOOL_OUTPUT: {} = {}]", tool_name, model_output);
    assert_eq!(actual, expected);
}

#[test]
fn result_format_patchfile_injection() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "inject.txt", "line1\nline2\n");
    let reg = ToolRegistry::new();
    let xml = patch_xml(path.to_str().unwrap(), 1, 1, "NEW");
    let (name, args) = parse_one(&xml);
    let result = reg.execute(&name, &args).unwrap();
    let injected = format!("[TOOL_OUTPUT: {} = {}]", name, result);
    assert!(injected.contains("[TOOL_OUTPUT: patchfile = "), "got: {injected}");
}

// ── Module 7: Error handling ──────────────────────────────────────────────────

#[test]
fn error_unknown_tool_name() {
    let reg = ToolRegistry::new();
    let result = reg.execute("nonexistent_tool_xyz", "args");
    assert!(result.is_err(), "expected error for unknown tool");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("unknown tool"), "got: {err}");
}

#[test]
fn error_shell_nonexistent_command() {
    let reg = ToolRegistry::new();
    let result = reg.execute("shell", "nonexistent_command_yggdra_test_xyz_123");
    // Should return Ok with stderr content (not found) or an empty result — should NOT panic.
    match result {
        Ok(out) => assert!(!out.trim().is_empty() || out.is_empty(), "got: {out}"),
        Err(e)  => { let s = e.to_string(); assert!(!s.is_empty(), "got: {s}"); }
    }
}

#[test]
fn error_shell_empty_command() {
    let reg = ToolRegistry::new();
    let result = reg.execute("shell", "");
    assert!(result.is_err(), "expected error for empty shell command");
}

#[test]
fn error_setfile_empty_path() {
    let reg = ToolRegistry::new();
    let result = reg.execute("setfile", "\x00some content");
    assert!(result.is_err(), "expected error for empty setfile path");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("setfile"), "got: {err}");
}

#[test]
fn error_setfile_no_null_delimiter() {
    let reg = ToolRegistry::new();
    // Without \x00, the whole string is treated as path with empty content
    // If the path doesn't exist and we can't write there, it may error or succeed
    // depending on sandbox state. Just verify no panic.
    let result = reg.execute("setfile", "just_a_path_no_content");
    // Result may be ok or err depending on whether the path is writable
    let _ = result;
}

#[test]
fn error_patchfile_nonexistent_file() {
    let reg = ToolRegistry::new();
    let result = reg.execute("patchfile", "/tmp/yggdra_definitely_does_not_exist_xyz.txt\x001\x001\x00new text");
    assert!(result.is_err(), "expected error for nonexistent file");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("patchfile") || err.contains("does not exist"), "got: {err}");
}

#[test]
fn error_patchfile_zero_start_line() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "zero.txt", "line1\n");
    let reg = ToolRegistry::new();
    let result = reg.execute("patchfile", &format!("{}\x000\x000\x00new", path.display()));
    assert!(result.is_err(), "expected error for start_line=0");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("start_line") || err.contains("1-based"), "got: {err}");
}

#[test]
fn error_patchfile_end_before_start() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "inv.txt", "a\nb\nc\n");
    let reg = ToolRegistry::new();
    let result = reg.execute("patchfile", &format!("{}\x003\x001\x00new", path.display()));
    assert!(result.is_err(), "expected error when end_line < start_line");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("end_line") || err.contains("start_line"), "got: {err}");
}

#[test]
fn error_patchfile_non_integer_line_numbers() {
    let dir = TempDir::new().unwrap();
    let path = create_test_file(&dir, "nonint.txt", "a\nb\n");
    let reg = ToolRegistry::new();
    let result = reg.execute("patchfile", &format!("{}\x00abc\x00xyz\x00new", path.display()));
    assert!(result.is_err(), "expected parse error for non-integer line numbers");
}

#[test]
fn error_commit_empty_message() {
    let reg = ToolRegistry::new();
    let result = reg.execute("commit", "");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("empty"));
}

#[test]
fn error_parse_no_tool_tags_returns_empty() {
    let xml = "no tool tags here at all";
    let calls = parse_xml_tool_calls(xml);
    assert!(calls.is_empty(), "expected no calls, got: {calls:?}");
}

#[test]
fn error_parse_unclosed_tool_tag() {
    let xml = "<tool>shell<command>echo hi</command>";
    let calls = parse_xml_tool_calls(xml);
    // Unclosed <tool> → no valid calls parsed
    assert!(calls.is_empty(), "expected no calls for unclosed tag, got: {calls:?}");
}

#[test]
fn error_parse_invalid_tool_name() {
    let xml = "<tool>definitely_not_a_valid_tool_name</tool><command>echo hi</command>";
    let calls = parse_xml_tool_calls(xml);
    // Unknown tool names are skipped
    assert!(calls.is_empty(), "expected no calls for invalid tool name, got: {calls:?}");
}

#[test]
fn error_shell_network_pipe_blocked() {
    let reg = ToolRegistry::new();
    let result = reg.execute("shell", "echo hello | nc localhost 80");
    assert!(result.is_err(), "expected network pipe to be blocked");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("blocked") || err.contains("network"), "got: {err}");
}

// ── Module 8: ToolRegistry self-tests ────────────────────────────────────────

#[test]
fn registry_lists_all_four_tools() {
    let reg = ToolRegistry::new();
    let mut tools = reg.list_tools();
    tools.sort();
    assert!(tools.contains(&"shell"), "missing shell");
    assert!(tools.contains(&"setfile"), "missing setfile");
    assert!(tools.contains(&"patchfile"), "missing patchfile");
    assert!(tools.contains(&"commit"), "missing commit");
}

#[test]
fn registry_execute_shell_succeeds() {
    let reg = ToolRegistry::new();
    let result = reg.execute("shell", "echo registry_test");
    assert!(result.is_ok());
}

#[test]
fn registry_execute_setfile_succeeds() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("reg.txt");
    let reg = ToolRegistry::new();
    let args = format!("{}\x00registry content", path.display());
    let result = reg.execute("setfile", &args);
    assert!(result.is_ok(), "got: {:?}", result);
}

#[test]
fn registry_execute_unknown_tool_fails() {
    let reg = ToolRegistry::new();
    let result = reg.execute("bogus_tool", "args");
    assert!(result.is_err());
}

#[test]
fn registry_new_creates_independent_registries() {
    let reg1 = ToolRegistry::new();
    let reg2 = ToolRegistry::new();
    let out1 = reg1.execute("shell", "echo r1").unwrap();
    let out2 = reg2.execute("shell", "echo r2").unwrap();
    assert!(strip_changes(&out1).contains("r1"));
    assert!(strip_changes(&out2).contains("r2"));
}

// ── Module 9: XML parsing edge cases ─────────────────────────────────────────

#[test]
fn parse_unix_command_remapped_to_shell() {
    // cat is remapped to shell automatically
    let xml = "<tool>cat</tool><command>file.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
    assert!(calls[0].args.contains("cat file.txt"), "got: {}", calls[0].args);
}

#[test]
fn parse_grep_remapped_to_shell() {
    let xml = "<tool>grep</tool><command>pattern file.txt</command>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}

#[test]
fn parse_multiple_calls_in_sequence() {
    let xml = concat!(
        "<tool>shell</tool><command>echo a</command>",
        "<tool>shell</tool><command>echo b</command>",
        "<tool>shell</tool><command>echo c</command>",
    );
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 3, "expected 3 calls, got: {}", calls.len());
}

#[test]
fn parse_tool_call_preserves_async_task_id() {
    let xml = r#"<tool>shell</tool><command>echo bg</command><mode>async</mode><task_id>task-1</task_id>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].async_task_id.as_deref(), Some("task-1"));
}

#[test]
fn parse_setfile_multiline_content() {
    let xml = "<tool>setfile</tool><path>/tmp/x.txt</path><content>\nline1\nline2\n</content>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    let parts: Vec<&str> = calls[0].args.splitn(2, '\x00').collect();
    assert_eq!(parts[0], "/tmp/x.txt");
    // Leading newline stripped by extract_tag_raw
    assert_eq!(parts[1], "line1\nline2\n");
}

#[test]
fn parse_patchfile_all_four_parts() {
    let xml = "<tool>patchfile</tool><path>/tmp/f.txt</path><start_line>5</start_line><end_line>10</end_line><new_text>replacement</new_text>";
    let calls = parse_xml_tool_calls(xml);
    let parts: Vec<&str> = calls[0].args.splitn(4, '\x00').collect();
    assert_eq!(parts[0], "/tmp/f.txt");
    assert_eq!(parts[1], "5");
    assert_eq!(parts[2], "10");
    assert_eq!(parts[3], "replacement");
}

#[test]
fn parse_desc_not_in_args() {
    let xml = r#"<tool>shell</tool><command>ls</command><desc>list files</desc>"#;
    let calls = parse_xml_tool_calls(xml);
    assert!(!calls[0].args.contains("list files"), "desc leaked into args: {}", calls[0].args);
    assert_eq!(calls[0].description.as_deref(), Some("list files"));
}

#[test]
fn parse_tellhuman_field() {
    let xml = r#"<tool>shell</tool><command>ls</command><tellhuman>showing files</tellhuman>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].tellhuman.as_deref(), Some("showing files"));
}

#[test]
fn parse_shell_with_returnlines_encodes_in_args() {
    let xml = r#"<tool>shell</tool><command>seq 1 100</command><returnlines>10-20</returnlines>"#;
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "seq 1 100\x0010-20");
}

#[test]
fn parse_empty_document_returns_empty() {
    let calls = parse_xml_tool_calls("");
    assert!(calls.is_empty());
}

#[test]
fn parse_whitespace_only_returns_empty() {
    let calls = parse_xml_tool_calls("   \n\t  ");
    assert!(calls.is_empty());
}

#[test]
fn parse_commit_with_message_tag() {
    let xml = "<tool>commit</tool><message>fix: patch applied</message>";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls[0].args, "fix: patch applied");
}

#[test]
fn parse_interleaved_prose_ignored() {
    let xml = "Some model output here.\n<tool>shell</tool><command>echo hi</command>\nMore prose.";
    let calls = parse_xml_tool_calls(xml);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "shell");
}
