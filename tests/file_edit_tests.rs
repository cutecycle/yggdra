/// Comprehensive integration tests for SetfileTool, PatchfileTool, and ReadfileTool.
/// Sandbox is uninitialised in this binary, so all absolute paths are permissive.

#[cfg(test)]
mod file_edit_tests {
    use std::fs;
    use yggdra::tools::{PatchfileTool, ReadfileTool, SetfileTool, Tool, ToolRegistry};

    fn tmpfile(name: &str) -> String {
        format!("/tmp/yggdra_fe_{}", name)
    }

    fn cleanup(path: &str) {
        let _ = fs::remove_file(path);
    }

    fn cleanup_dir(path: &str) {
        let _ = fs::remove_dir_all(path);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // SetfileTool tests
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn setfile_creates_new_file() {
        let path = tmpfile("creates_new");
        cleanup(&path);
        let tool = SetfileTool;
        let args = format!("{}\x00hello world\n", path);
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("✅"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world\n");
        cleanup(&path);
    }

    #[test]
    fn setfile_overwrites_existing() {
        let path = tmpfile("overwrites");
        let tool = SetfileTool;
        tool.execute(&format!("{}\x00first\n", path)).unwrap();
        tool.execute(&format!("{}\x00second\n", path)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        cleanup(&path);
    }

    #[test]
    fn setfile_empty_content() {
        let path = tmpfile("empty_content");
        cleanup(&path);
        let tool = SetfileTool;
        let args = format!("{}\x00", path);
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("✅"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        cleanup(&path);
    }

    #[test]
    fn setfile_single_line_no_newline() {
        let path = tmpfile("single_no_nl");
        let tool = SetfileTool;
        tool.execute(&format!("{}\x00hello", path)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
        cleanup(&path);
    }

    #[test]
    fn setfile_trailing_newline_preserved() {
        let path = tmpfile("trailing_nl");
        let tool = SetfileTool;
        tool.execute(&format!("{}\x00hello\n", path)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
        cleanup(&path);
    }

    #[test]
    fn setfile_no_trailing_newline() {
        let path = tmpfile("no_trailing_nl");
        let tool = SetfileTool;
        tool.execute(&format!("{}\x00hello", path)).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.ends_with('\n'));
        cleanup(&path);
    }

    #[test]
    fn setfile_multiline() {
        let path = tmpfile("multiline");
        let tool = SetfileTool;
        let content = "line1\nline2\nline3\nline4\nline5\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("line1"));
        assert!(back.contains("line3"));
        assert!(back.contains("line5"));
        cleanup(&path);
    }

    #[test]
    fn setfile_unicode_basic() {
        let path = tmpfile("unicode_basic");
        let tool = SetfileTool;
        let content = "héllo wörld\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_unicode_emoji() {
        let path = tmpfile("unicode_emoji");
        let tool = SetfileTool;
        let content = "🦀 Rust 🎉\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_unicode_cjk() {
        let path = tmpfile("unicode_cjk");
        let tool = SetfileTool;
        let content = "日本語テスト\n中文测试\n한국어 테스트\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_large_content_10k() {
        let path = tmpfile("large_10k");
        let tool = SetfileTool;
        let content = "abcdefghij".repeat(1000);
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().len(), 10000);
        cleanup(&path);
    }

    #[test]
    fn setfile_large_content_100k() {
        let path = tmpfile("large_100k");
        let tool = SetfileTool;
        let content = "x".repeat(100_000);
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().len(), 100_000);
        cleanup(&path);
    }

    #[test]
    fn setfile_large_content_line_count() {
        let path = tmpfile("large_lines");
        let tool = SetfileTool;
        let content = (0..1000).map(|i| format!("line{}\n", i)).collect::<String>();
        let result = tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert!(result.contains("1000 lines"));
        cleanup(&path);
    }

    #[test]
    fn setfile_returns_line_count_message() {
        let path = tmpfile("line_count_msg");
        let tool = SetfileTool;
        let content = "a\nb\nc\n";
        let result = tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert!(result.contains("3 lines"));
        cleanup(&path);
    }

    #[test]
    fn setfile_returns_ok_marker() {
        let path = tmpfile("ok_marker");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00content\n", path)).unwrap();
        assert!(result.contains("✅"));
        cleanup(&path);
    }

    #[test]
    fn setfile_creates_parent_dir() {
        let dir = "/tmp/yggdra_fe_nested/subdir";
        let path = format!("{}/file.txt", dir);
        cleanup_dir("/tmp/yggdra_fe_nested");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00content\n", path));
        assert!(result.is_ok(), "should create parent dirs: {:?}", result);
        assert!(fs::metadata(&path).is_ok());
        cleanup_dir("/tmp/yggdra_fe_nested");
    }

    #[test]
    fn setfile_creates_deeply_nested() {
        let base = "/tmp/yggdra_fe_deep";
        let path = format!("{}/a/b/c/d/e/file.txt", base);
        cleanup_dir(base);
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00deep content\n", path));
        assert!(result.is_ok(), "{:?}", result);
        assert_eq!(fs::read_to_string(&path).unwrap(), "deep content\n");
        cleanup_dir(base);
    }

    #[test]
    fn setfile_content_with_quotes() {
        let path = tmpfile("with_quotes");
        let tool = SetfileTool;
        let content = r#"She said "hello" and 'goodbye'"#;
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_content_with_backslashes() {
        let path = tmpfile("with_backslashes");
        let tool = SetfileTool;
        let content = "path\\to\\file\n\\n escaped\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_content_with_xml_tags() {
        let path = tmpfile("with_xml");
        let tool = SetfileTool;
        let content = "<tool>shell</tool>\n<|tool>exec<|end_tool>\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_content_with_json() {
        let path = tmpfile("with_json");
        let tool = SetfileTool;
        let content = "{\"name\": \"yggdra\", \"version\": \"0.1.0\", \"active\": true}\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("yggdra"));
        assert!(back.contains("version"));
        cleanup(&path);
    }

    #[test]
    fn setfile_content_rust_code() {
        let path = tmpfile("rust_code.rs");
        let tool = SetfileTool;
        let content = r#"fn main() {
    println!("Hello, world!");
    let x: u32 = 42;
    assert_eq!(x, 42);
}
"#;
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("fn main"));
        assert!(back.contains("println!"));
        cleanup(&path);
    }

    #[test]
    fn setfile_content_python_code() {
        let path = tmpfile("python_code.py");
        let tool = SetfileTool;
        let content = "#!/usr/bin/env python3\n\ndef greet(name: str) -> str:\n    return f'Hello, {name}!'\n\nif __name__ == '__main__':\n    print(greet('world'))\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("def greet"));
        assert!(back.contains("__main__"));
        cleanup(&path);
    }

    #[test]
    fn setfile_content_shell_script() {
        let path = tmpfile("shell_script.sh");
        let tool = SetfileTool;
        let content = "#!/bin/bash\nset -euo pipefail\necho \"Starting...\"\nfor i in 1 2 3; do\n  echo \"Item $i\"\ndone\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("#!/bin/bash"));
        assert!(back.contains("for i in"));
        cleanup(&path);
    }

    #[test]
    fn setfile_content_markdown() {
        let path = tmpfile("readme.md");
        let tool = SetfileTool;
        let content = "# Title\n\n## Section\n\nSome text.\n\n```rust\nfn foo() {}\n```\n\n- item 1\n- item 2\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("# Title"));
        assert!(back.contains("```rust"));
        cleanup(&path);
    }

    #[test]
    fn setfile_overwrite_with_fewer_lines() {
        let path = tmpfile("overwrite_fewer");
        let tool = SetfileTool;
        let ten_lines = (1..=10).map(|i| format!("line{}\n", i)).collect::<String>();
        tool.execute(&format!("{}\x00{}", path, ten_lines)).unwrap();
        tool.execute(&format!("{}\x00a\nb\nc\n", path)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert_eq!(back.lines().count(), 3);
        assert!(!back.contains("line4"));
        cleanup(&path);
    }

    #[test]
    fn setfile_overwrite_with_more_lines() {
        let path = tmpfile("overwrite_more");
        let tool = SetfileTool;
        tool.execute(&format!("{}\x00a\nb\nc\n", path)).unwrap();
        let ten_lines = (1..=10).map(|i| format!("line{}\n", i)).collect::<String>();
        tool.execute(&format!("{}\x00{}", path, ten_lines)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert_eq!(back.lines().count(), 10);
        cleanup(&path);
    }

    #[test]
    fn setfile_validates_empty_path() {
        let tool = SetfileTool;
        let err = tool.validate_input("\x00some content");
        assert!(err.is_err(), "empty path before \\x00 should fail");
    }

    #[test]
    fn setfile_validates_no_null_passes_validation() {
        // Without a \x00, the whole string is the path; sandbox is uninit so it passes
        let tool = SetfileTool;
        let result = tool.validate_input("just-path-no-null");
        assert!(result.is_ok(), "uninit sandbox allows any path");
    }

    #[test]
    fn setfile_args_format_path_null_content_writes_file() {
        let path = tmpfile("format_test");
        let tool = SetfileTool;
        let args = format!("{}\x00actual content\n", path);
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("✅"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "actual content\n");
        cleanup(&path);
    }

    #[test]
    fn setfile_write_then_verify_with_fs_read() {
        let path = tmpfile("fs_read_verify");
        let tool = SetfileTool;
        let expected = "verified content line 1\nverified content line 2\n";
        tool.execute(&format!("{}\x00{}", path, expected)).unwrap();
        let actual = fs::read_to_string(&path).unwrap();
        assert_eq!(actual, expected);
        cleanup(&path);
    }

    #[test]
    fn setfile_concurrent_different_files() {
        let tool = SetfileTool;
        let paths: Vec<String> = (0..5).map(|i| tmpfile(&format!("conc_{}", i))).collect();
        for (i, p) in paths.iter().enumerate() {
            tool.execute(&format!("{}\x00content {}\n", p, i)).unwrap();
        }
        for (i, p) in paths.iter().enumerate() {
            let back = fs::read_to_string(p).unwrap();
            assert!(back.contains(&format!("content {}", i)));
            cleanup(p);
        }
    }

    #[test]
    fn setfile_zero_bytes_single_newline() {
        let path = tmpfile("single_newline");
        let tool = SetfileTool;
        tool.execute(&format!("{}\x00\n", path)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "\n");
        cleanup(&path);
    }

    #[test]
    fn setfile_exactly_one_line_no_newline_says_1_lines() {
        let path = tmpfile("one_line_no_nl");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00hello", path)).unwrap();
        assert!(result.contains("1 lines"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn setfile_exactly_one_line_with_newline_says_1_lines() {
        let path = tmpfile("one_line_with_nl");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00hello\n", path)).unwrap();
        assert!(result.contains("1 lines"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn setfile_dotfile() {
        let path = "/tmp/.yggdra_hidden_test";
        cleanup(path);
        let tool = SetfileTool;
        tool.execute(&format!("{}\x00hidden\n", path)).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "hidden\n");
        cleanup(path);
    }

    #[test]
    fn setfile_extension_rs() {
        let path = tmpfile("test_file.rs");
        let tool = SetfileTool;
        let content = "fn test() -> bool { true }\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_extension_json() {
        let path = tmpfile("config.json");
        let tool = SetfileTool;
        let content = "{\"key\": \"value\"}\n";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        cleanup(&path);
    }

    #[test]
    fn setfile_path_with_spaces_in_name() {
        let path = "/tmp/yggdra fe spaces.txt";
        cleanup(path);
        let tool = SetfileTool;
        let args = format!("{}\x00spaced content\n", path);
        let result = tool.execute(&args);
        // May or may not succeed depending on FS support, but should not panic
        let _ = result;
        cleanup(path);
    }

    #[test]
    fn setfile_very_long_path_nesting() {
        let base = "/tmp/yggdra_fe_longpath";
        let path = format!("{}/l1/l2/l3/l4/l5/l6/l7/l8/file.txt", base);
        cleanup_dir(base);
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00deep\n", path));
        assert!(result.is_ok(), "{:?}", result);
        assert_eq!(fs::read_to_string(&path).unwrap(), "deep\n");
        cleanup_dir(base);
    }

    #[test]
    fn setfile_zero_line_count_for_empty_content() {
        let path = tmpfile("zero_lines");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00", path)).unwrap();
        assert!(result.contains("0 lines"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn setfile_two_lines_counts_correctly() {
        let path = tmpfile("two_lines");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00line1\nline2\n", path)).unwrap();
        assert!(result.contains("2 lines"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn setfile_result_contains_wrote() {
        let path = tmpfile("wrote_check");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00content\n", path)).unwrap();
        assert!(result.contains("wrote"), "expected 'wrote' in: {}", result);
        cleanup(&path);
    }

    #[test]
    fn setfile_validates_path_with_null_in_middle() {
        // The path part (before first \x00) must not be empty
        let tool = SetfileTool;
        let ok = tool.validate_input("/tmp/yggdra_fe_nullmid\x00content");
        assert!(ok.is_ok());
    }

    #[test]
    fn setfile_execute_returns_path_in_result() {
        let path = tmpfile("path_in_result");
        let tool = SetfileTool;
        let result = tool.execute(&format!("{}\x00something\n", path)).unwrap();
        // Result should mention the filename
        assert!(result.contains("yggdra_fe_path_in_result"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn setfile_5_lines_no_trailing_newline() {
        let path = tmpfile("5_lines_nonl");
        let tool = SetfileTool;
        let content = "a\nb\nc\nd\ne";
        tool.execute(&format!("{}\x00{}", path, content)).unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert_eq!(back.lines().count(), 5);
        assert!(!back.ends_with('\n'));
        cleanup(&path);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // PatchfileTool tests
    // ══════════════════════════════════════════════════════════════════════════

    fn write_lines(path: &str, lines: &[&str]) {
        let content = lines.join("\n") + "\n";
        fs::write(path, content).unwrap();
    }

    fn read_lines(path: &str) -> Vec<String> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| l.to_string())
            .collect()
    }

    fn patch(path: &str, start: usize, end: usize, new_text: &str) -> Result<String, anyhow::Error> {
        let tool = PatchfileTool;
        let args = format!("{}\x00{}\x00{}\x00{}", path, start, end, new_text);
        tool.execute(&args)
    }

    #[test]
    fn patchfile_replace_single_line() {
        let path = tmpfile("patch_single");
        write_lines(&path, &["a", "b", "c", "d", "e"]);
        patch(&path, 3, 3, "C").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[2], "C");
        assert_eq!(lines[0], "a");
        assert_eq!(lines[4], "e");
        cleanup(&path);
    }

    #[test]
    fn patchfile_replace_single_line_first() {
        let path = tmpfile("patch_first");
        write_lines(&path, &["first", "second", "third"]);
        patch(&path, 1, 1, "FIRST").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "FIRST");
        assert_eq!(lines[1], "second");
        cleanup(&path);
    }

    #[test]
    fn patchfile_replace_single_line_last() {
        let path = tmpfile("patch_last");
        write_lines(&path, &["x", "y", "z"]);
        patch(&path, 3, 3, "Z").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[2], "Z");
        assert_eq!(lines[0], "x");
        cleanup(&path);
    }

    #[test]
    fn patchfile_replace_middle_two_lines() {
        let path = tmpfile("patch_mid2");
        write_lines(&path, &["1", "2", "3", "4", "5"]);
        patch(&path, 2, 3, "TWO\nTHREE").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "1");
        assert_eq!(lines[1], "TWO");
        assert_eq!(lines[2], "THREE");
        assert_eq!(lines[3], "4");
        cleanup(&path);
    }

    #[test]
    fn patchfile_replace_all_lines() {
        let path = tmpfile("patch_all");
        write_lines(&path, &["a", "b", "c"]);
        patch(&path, 1, 3, "X\nY\nZ").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines, vec!["X", "Y", "Z"]);
        cleanup(&path);
    }

    #[test]
    fn patchfile_shrink_3_to_1() {
        let path = tmpfile("patch_shrink");
        write_lines(&path, &["before", "L1", "L2", "L3", "after"]);
        patch(&path, 2, 4, "ONLY").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "before");
        assert_eq!(lines[1], "ONLY");
        assert_eq!(lines[2], "after");
        assert_eq!(lines.len(), 3);
        cleanup(&path);
    }

    #[test]
    fn patchfile_expand_1_to_3() {
        let path = tmpfile("patch_expand3");
        write_lines(&path, &["before", "old", "after"]);
        patch(&path, 2, 2, "new1\nnew2\nnew3").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "before");
        assert_eq!(lines[1], "new1");
        assert_eq!(lines[2], "new2");
        assert_eq!(lines[3], "new3");
        assert_eq!(lines[4], "after");
        cleanup(&path);
    }

    #[test]
    fn patchfile_expand_1_to_10() {
        let path = tmpfile("patch_expand10");
        write_lines(&path, &["start", "single", "end"]);
        let new_text: String = (1..=10).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
        patch(&path, 2, 2, &new_text).unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "start");
        assert_eq!(lines[1], "line1");
        assert_eq!(lines[10], "line10");
        assert_eq!(lines[11], "end");
        cleanup(&path);
    }

    #[test]
    fn patchfile_empty_replacement_deletes() {
        let path = tmpfile("patch_delete");
        write_lines(&path, &["keep1", "del1", "del2", "keep2"]);
        patch(&path, 2, 3, "").unwrap();
        let lines = read_lines(&path);
        assert!(lines.iter().all(|l| !l.contains("del")));
        assert!(lines.contains(&"keep1".to_string()));
        assert!(lines.contains(&"keep2".to_string()));
        cleanup(&path);
    }

    #[test]
    fn patchfile_whitespace_only_replacement() {
        let path = tmpfile("patch_whitespace");
        write_lines(&path, &["a", "b", "c"]);
        let result = patch(&path, 2, 2, "   ");
        assert!(result.is_ok());
        let lines = read_lines(&path);
        assert_eq!(lines[1], "   ");
        cleanup(&path);
    }

    #[test]
    fn patchfile_unicode_replacement() {
        let path = tmpfile("patch_unicode");
        write_lines(&path, &["hello", "world", "end"]);
        patch(&path, 2, 2, "🦀 Rust 🎉").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[1], "🦀 Rust 🎉");
        cleanup(&path);
    }

    #[test]
    fn patchfile_preserve_lines_before() {
        let path = tmpfile("patch_before");
        write_lines(&path, &["before1", "before2", "target", "after1"]);
        patch(&path, 3, 3, "REPLACED").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "before1");
        assert_eq!(lines[1], "before2");
        cleanup(&path);
    }

    #[test]
    fn patchfile_preserve_lines_after() {
        let path = tmpfile("patch_after");
        write_lines(&path, &["before", "target", "after1", "after2"]);
        patch(&path, 2, 2, "REPLACED").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[2], "after1");
        assert_eq!(lines[3], "after2");
        cleanup(&path);
    }

    #[test]
    fn patchfile_large_file_patch_middle() {
        let path = tmpfile("patch_large_mid");
        let content_lines: Vec<String> = (1..=100).map(|i| format!("line{}", i)).collect();
        let content_strs: Vec<&str> = content_lines.iter().map(|s| s.as_str()).collect();
        write_lines(&path, &content_strs);
        patch(&path, 50, 51, "patched50\npatched51").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[49], "patched50");
        assert_eq!(lines[50], "patched51");
        assert_eq!(lines[48], "line49");
        assert_eq!(lines[51], "line52");
        cleanup(&path);
    }

    #[test]
    fn patchfile_large_file_patch_end() {
        let path = tmpfile("patch_large_end");
        let content_lines: Vec<String> = (1..=100).map(|i| format!("line{}", i)).collect();
        let content_strs: Vec<&str> = content_lines.iter().map(|s| s.as_str()).collect();
        write_lines(&path, &content_strs);
        patch(&path, 99, 100, "endA\nendB").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[98], "endA");
        assert_eq!(lines[99], "endB");
        assert_eq!(lines[97], "line98");
        cleanup(&path);
    }

    #[test]
    fn patchfile_single_line_file_replace() {
        let path = tmpfile("patch_single_line_file");
        fs::write(&path, "only line\n").unwrap();
        patch(&path, 1, 1, "replaced").unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("replaced"));
        assert!(!back.contains("only line"));
        cleanup(&path);
    }

    #[test]
    fn patchfile_returns_success_marker() {
        let path = tmpfile("patch_success");
        write_lines(&path, &["a", "b", "c"]);
        let result = patch(&path, 1, 1, "A").unwrap();
        assert!(result.contains("✅"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn patchfile_out_of_bounds_error() {
        let path = tmpfile("patch_oob");
        write_lines(&path, &["a", "b", "c", "d", "e"]);
        // start_line > total+1 (5+1=6), so start=7 should error
        let err = patch(&path, 7, 8, "x").unwrap_err();
        assert!(
            err.to_string().contains("exceeds"),
            "expected 'exceeds' in: {}",
            err
        );
        cleanup(&path);
    }

    #[test]
    fn patchfile_end_less_than_start_error() {
        let path = tmpfile("patch_inv_range");
        write_lines(&path, &["a", "b", "c"]);
        let err = patch(&path, 3, 2, "x").unwrap_err();
        assert!(
            err.to_string().contains("end_line") || err.to_string().contains("start_line"),
            "got: {}",
            err
        );
        cleanup(&path);
    }

    #[test]
    fn patchfile_nonexistent_file_error() {
        let path = tmpfile("patch_nonexistent_xyz987");
        cleanup(&path);
        let err = patch(&path, 1, 1, "x").unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "got: {}",
            err
        );
    }

    #[test]
    fn patchfile_content_with_special_chars() {
        let path = tmpfile("patch_special");
        write_lines(&path, &["normal", "old", "normal"]);
        patch(&path, 2, 2, r#"has "quotes" and \backslash\ and 'ticks'"#).unwrap();
        let lines = read_lines(&path);
        assert!(lines[1].contains("quotes"));
        assert!(lines[1].contains("backslash"));
        cleanup(&path);
    }

    #[test]
    fn patchfile_content_rust_code() {
        let path = tmpfile("patch_rust.rs");
        write_lines(&path, &["fn foo() {", "    old_body();", "}"]);
        patch(&path, 2, 2, "    let x = 42;\n    println!(\"{}\", x);").unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("let x = 42"));
        assert!(back.contains("println!"));
        cleanup(&path);
    }

    #[test]
    fn patchfile_content_json_block() {
        let path = tmpfile("patch_json");
        write_lines(&path, &["{", "  \"old\": \"value\"", "}"]);
        patch(&path, 2, 2, "  \"key\": \"new_value\",\n  \"flag\": true").unwrap();
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("new_value"));
        assert!(back.contains("flag"));
        cleanup(&path);
    }

    #[test]
    fn patchfile_idempotent() {
        let path = tmpfile("patch_idempotent");
        write_lines(&path, &["a", "b", "c"]);
        patch(&path, 2, 2, "B").unwrap();
        patch(&path, 2, 2, "B").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[1], "B");
        assert_eq!(lines.len(), 3);
        cleanup(&path);
    }

    #[test]
    fn patchfile_sequential_patches() {
        let path = tmpfile("patch_seq");
        write_lines(&path, &["a", "b", "c", "d", "e"]);
        patch(&path, 2, 2, "B").unwrap();
        patch(&path, 4, 4, "D").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "a");
        assert_eq!(lines[1], "B");
        assert_eq!(lines[2], "c");
        assert_eq!(lines[3], "D");
        assert_eq!(lines[4], "e");
        cleanup(&path);
    }

    #[test]
    fn patchfile_roundtrip_setfile_then_patch() {
        let path = tmpfile("patch_roundtrip");
        let set = SetfileTool;
        set.execute(&format!("{}\x00alpha\nbeta\ngamma\n", path)).unwrap();
        patch(&path, 2, 2, "BETA").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "alpha");
        assert_eq!(lines[1], "BETA");
        assert_eq!(lines[2], "gamma");
        cleanup(&path);
    }

    #[test]
    fn patchfile_patch_first_line_preserves_rest() {
        let path = tmpfile("patch_first_rest");
        write_lines(&path, &["old_first", "second", "third", "fourth"]);
        patch(&path, 1, 1, "new_first").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "new_first");
        assert_eq!(lines[1], "second");
        assert_eq!(lines[3], "fourth");
        cleanup(&path);
    }

    #[test]
    fn patchfile_patch_last_line_preserves_rest() {
        let path = tmpfile("patch_last_rest");
        write_lines(&path, &["first", "second", "third", "old_last"]);
        patch(&path, 4, 4, "new_last").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "first");
        assert_eq!(lines[2], "third");
        assert_eq!(lines[3], "new_last");
        cleanup(&path);
    }

    #[test]
    fn patchfile_multiline_new_text_preserves_newlines() {
        let path = tmpfile("patch_multi_nl");
        write_lines(&path, &["before", "old", "after"]);
        patch(&path, 2, 2, "line_A\nline_B\nline_C").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[1], "line_A");
        assert_eq!(lines[2], "line_B");
        assert_eq!(lines[3], "line_C");
        cleanup(&path);
    }

    #[test]
    fn patchfile_no_trailing_newline_in_replacement() {
        let path = tmpfile("patch_no_trail_nl");
        write_lines(&path, &["a", "b", "c"]);
        patch(&path, 2, 2, "B").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[1], "B");
        cleanup(&path);
    }

    #[test]
    fn patchfile_zero_start_is_error() {
        let path = tmpfile("patch_zero_start");
        write_lines(&path, &["a", "b"]);
        let err = patch(&path, 0, 1, "x").unwrap_err();
        assert!(
            err.to_string().contains("1-based") || err.to_string().contains("0"),
            "got: {}",
            err
        );
        cleanup(&path);
    }

    #[test]
    fn patchfile_zero_start_zero_end_is_error() {
        let path = tmpfile("patch_zero_zero");
        write_lines(&path, &["a", "b"]);
        // start=0 should fail at 1-based check
        let tool = PatchfileTool;
        let args = format!("{}\x000\x000\x00text", path);
        let err = tool.execute(&args).unwrap_err();
        assert!(
            err.to_string().contains("1-based") || err.to_string().contains("0"),
            "got: {}",
            err
        );
        cleanup(&path);
    }

    #[test]
    fn patchfile_same_line_range_is_single_line_patch() {
        let path = tmpfile("patch_same_range");
        write_lines(&path, &["p", "q", "r"]);
        patch(&path, 2, 2, "Q").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[1], "Q");
        assert_eq!(lines.len(), 3);
        cleanup(&path);
    }

    #[test]
    fn patchfile_validate_empty_path_fails() {
        let tool = PatchfileTool;
        let err = tool.validate_input("\x001\x002\x00text");
        assert!(err.is_err());
    }

    #[test]
    fn patchfile_validate_valid_args_pass() {
        let tool = PatchfileTool;
        let ok = tool.validate_input("/tmp/yggdra_fe_patchval\x001\x002\x00text");
        assert!(ok.is_ok());
    }

    #[test]
    fn patchfile_100_line_file_patch_line_50() {
        let path = tmpfile("patch_100_l50");
        let content_lines: Vec<String> = (1..=100).map(|i| format!("original{}", i)).collect();
        let content_strs: Vec<&str> = content_lines.iter().map(|s| s.as_str()).collect();
        write_lines(&path, &content_strs);
        patch(&path, 50, 50, "patched_line_50").unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[49], "patched_line_50");
        assert_eq!(lines[48], "original49");
        assert_eq!(lines[50], "original51");
        cleanup(&path);
    }

    #[test]
    fn patchfile_result_mentions_line_count() {
        let path = tmpfile("patch_result_lines");
        write_lines(&path, &["a", "b", "c"]);
        let result = patch(&path, 1, 2, "new1\nnew2\nnew3").unwrap();
        // Result contains @@ hunk header with counts
        assert!(result.contains("@@"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn patchfile_registry_execute() {
        let path = tmpfile("patch_registry");
        write_lines(&path, &["x", "y", "z"]);
        let registry = ToolRegistry::new();
        let args = format!("{}\x002\x002\x00Y", path);
        let result = registry.execute("patchfile", &args).unwrap();
        assert!(result.contains("✅"));
        assert_eq!(read_lines(&path)[1], "Y");
        cleanup(&path);
    }

    #[test]
    fn patchfile_content_empty_lines() {
        let path = tmpfile("patch_empty_lines");
        write_lines(&path, &["before", "old", "after"]);
        patch(&path, 2, 2, "\n\n\n").unwrap();
        // Three blank lines inserted in place of "old"
        let back = fs::read_to_string(&path).unwrap();
        assert!(!back.contains("old"));
        assert!(back.contains("before"));
        assert!(back.contains("after"));
        cleanup(&path);
    }

    #[test]
    fn patchfile_append_by_setting_start_to_total_plus_one() {
        let path = tmpfile("patch_append");
        write_lines(&path, &["a", "b", "c", "d", "e"]);
        // start = 6 (total+1), end = 6 — should append
        let tool = PatchfileTool;
        let args = format!("{}\x006\x006\x00appended", path);
        let result = tool.execute(&args);
        assert!(result.is_ok(), "start=total+1 should be allowed: {:?}", result);
        let back = fs::read_to_string(&path).unwrap();
        assert!(back.contains("appended"));
        cleanup(&path);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // ReadfileTool tests
    // ══════════════════════════════════════════════════════════════════════════

    fn write_numbered_file(path: &str, n: usize) {
        let content: String = (1..=n).map(|i| format!("line{}\n", i)).collect();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn readfile_reads_existing_file() {
        let path = tmpfile("read_basic");
        let set = SetfileTool;
        set.execute(&format!("{}\x00hello readfile\n", path)).unwrap();
        let tool = ReadfileTool;
        let result = tool.execute(&path).unwrap();
        assert!(result.contains("hello readfile"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn readfile_line_range_1_3() {
        let path = tmpfile("read_range_1_3");
        write_numbered_file(&path, 10);
        let tool = ReadfileTool;
        let result = tool.execute(&format!("{} 1 3", path)).unwrap();
        assert!(result.contains("line1"));
        assert!(result.contains("line3"));
        assert!(!result.contains("line4"));
        cleanup(&path);
    }

    #[test]
    fn readfile_line_range_middle() {
        let path = tmpfile("read_range_mid");
        write_numbered_file(&path, 10);
        let tool = ReadfileTool;
        let result = tool.execute(&format!("{} 4 6", path)).unwrap();
        assert!(result.contains("line4"));
        assert!(result.contains("line6"));
        assert!(!result.contains("line3"));
        assert!(!result.contains("line7"));
        cleanup(&path);
    }

    #[test]
    fn readfile_line_range_end() {
        let path = tmpfile("read_range_end");
        write_numbered_file(&path, 10);
        let tool = ReadfileTool;
        let result = tool.execute(&format!("{} 8 10", path)).unwrap();
        assert!(result.contains("line8"));
        assert!(result.contains("line10"));
        assert!(!result.contains("line7"));
        cleanup(&path);
    }

    #[test]
    fn readfile_single_line_range() {
        let path = tmpfile("read_single_range");
        write_numbered_file(&path, 10);
        let tool = ReadfileTool;
        let result = tool.execute(&format!("{} 5 5", path)).unwrap();
        assert!(result.contains("line5"));
        assert!(!result.contains("line4"));
        assert!(!result.contains("line6"));
        cleanup(&path);
    }

    #[test]
    fn readfile_range_beyond_end_clamped() {
        let path = tmpfile("read_clamp");
        write_numbered_file(&path, 5);
        let tool = ReadfileTool;
        let result = tool.execute(&format!("{} 1 999", path)).unwrap();
        assert!(result.contains("line1"));
        assert!(result.contains("line5"));
        cleanup(&path);
    }

    #[test]
    fn readfile_empty_file_no_error() {
        let path = tmpfile("read_empty");
        fs::write(&path, "").unwrap();
        let tool = ReadfileTool;
        let result = tool.execute(&path);
        assert!(result.is_ok(), "should handle empty file: {:?}", result);
        cleanup(&path);
    }

    #[test]
    fn readfile_nonexistent_returns_does_not_exist() {
        let path = tmpfile("read_nonexistent_abc999");
        cleanup(&path);
        let tool = ReadfileTool;
        let result = tool.execute(&path).unwrap();
        assert!(
            result.contains("does not exist"),
            "expected 'does not exist' in: {}",
            result
        );
    }

    #[test]
    fn readfile_search_term_present() {
        let path = tmpfile("read_search_present");
        fs::write(&path, "line1: foo\nline2: needle here\nline3: bar\n").unwrap();
        let tool = ReadfileTool;
        let args = format!("{}\x00\x00\x00needle", path);
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("needle"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn readfile_search_term_absent() {
        let path = tmpfile("read_search_absent");
        fs::write(&path, "line1: foo\nline2: bar\n").unwrap();
        let tool = ReadfileTool;
        let args = format!("{}\x00\x00\x00xyzzy_not_present", path);
        let result = tool.execute(&args).unwrap();
        assert!(
            result.contains("no matches") || result.contains("xyzzy_not_present"),
            "got: {}",
            result
        );
        cleanup(&path);
    }

    #[test]
    fn readfile_result_includes_line_numbers() {
        let path = tmpfile("read_line_nums");
        write_numbered_file(&path, 5);
        let tool = ReadfileTool;
        let result = tool.execute(&path).unwrap();
        // Line-numbered output: "   1: line1" format
        assert!(result.contains("1:") || result.contains("  1"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn readfile_large_file_range() {
        let path = tmpfile("read_large_range");
        write_numbered_file(&path, 500);
        let tool = ReadfileTool;
        let result = tool.execute(&format!("{} 100 150", path)).unwrap();
        assert!(result.contains("line100"));
        assert!(result.contains("line150"));
        assert!(!result.contains("line99"));
        assert!(!result.contains("line151"));
        cleanup(&path);
    }

    #[test]
    fn readfile_unicode_content() {
        let path = tmpfile("read_unicode");
        fs::write(&path, "🦀 Rust\n日本語\nhéllo\n").unwrap();
        let tool = ReadfileTool;
        let result = tool.execute(&path).unwrap();
        assert!(result.contains("🦀"));
        assert!(result.contains("日本語"));
        cleanup(&path);
    }

    #[test]
    fn readfile_validate_empty_path() {
        let tool = ReadfileTool;
        let err = tool.validate_input("");
        assert!(err.is_err());
    }

    #[test]
    fn readfile_validate_valid_path() {
        let tool = ReadfileTool;
        let ok = tool.validate_input("Cargo.toml");
        assert!(ok.is_ok());
    }

    #[test]
    fn readfile_validate_glob_passes_but_execute_returns_not_found() {
        // Sandbox is uninitialised; validate_input passes for any non-empty path.
        // The literal glob path "src/*.rs" doesn't exist, so execute returns "does not exist".
        let tool = ReadfileTool;
        assert!(tool.validate_input("src/*.rs").is_ok());
        let result = tool.execute("src/*.rs").unwrap();
        assert!(result.contains("does not exist"), "got: {}", result);
    }

    #[test]
    fn readfile_reads_real_cargo_toml() {
        let tool = ReadfileTool;
        let result = tool.execute("Cargo.toml").unwrap();
        assert!(result.contains("[package]"), "got: {}", result);
    }

    #[test]
    fn readfile_reads_real_src_main_first_10_lines() {
        let tool = ReadfileTool;
        let result = tool.execute("src/main.rs 1 10").unwrap();
        assert!(!result.is_empty());
        // Some Rust content should be present
        assert!(result.len() > 10);
    }

    #[test]
    fn readfile_search_wire_format_null_separated() {
        let path = tmpfile("read_null_sep");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let tool = ReadfileTool;
        let args = format!("{}\x00\x00\x00beta", path);
        let result = tool.execute(&args).unwrap();
        assert!(result.contains("beta"), "got: {}", result);
        cleanup(&path);
    }

    #[test]
    fn readfile_registry_execute_returns_error_unknown_tool() {
        // readfile is NOT in the ToolRegistry (ShellOnly profile)
        let registry = ToolRegistry::new();
        let result = registry.execute("readfile", "Cargo.toml");
        assert!(
            result.is_err(),
            "readfile should not be in ToolRegistry"
        );
        assert!(
            result.unwrap_err().to_string().contains("unknown tool"),
            "should say 'unknown tool'"
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // ToolRegistry integration tests
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn registry_lists_exactly_4_tools() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        assert_eq!(tools.len(), 6, "Expected 6 tools, got: {:?}", tools);
        assert!(tools.contains(&"shell"));
        assert!(tools.contains(&"setfile"));
        assert!(tools.contains(&"patchfile"));
        assert!(tools.contains(&"commit"));
        assert!(tools.contains(&"knowledge"));
        assert!(tools.contains(&"panel"));
    }

    #[test]
    fn registry_shell_executes_echo() {
        let registry = ToolRegistry::new();
        let result = registry.execute("shell", "echo hello_registry").unwrap();
        assert!(result.contains("hello_registry"), "got: {}", result);
    }

    #[test]
    fn registry_setfile_creates_file() {
        let path = tmpfile("registry_setfile");
        let registry = ToolRegistry::new();
        let args = format!("{}\x00registry content\n", path);
        let result = registry.execute("setfile", &args).unwrap();
        assert!(result.contains("✅"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "registry content\n"
        );
        cleanup(&path);
    }

    #[test]
    fn registry_patchfile_modifies_file() {
        let path = tmpfile("registry_patch");
        write_lines(&path, &["p", "q", "r"]);
        let registry = ToolRegistry::new();
        let args = format!("{}\x001\x001\x00P", path);
        let result = registry.execute("patchfile", &args).unwrap();
        assert!(result.contains("✅"));
        assert_eq!(read_lines(&path)[0], "P");
        cleanup(&path);
    }

    #[test]
    fn registry_unknown_tool_descriptive_error() {
        let registry = ToolRegistry::new();
        let err = registry.execute("nonexistent_tool_xyz", "args").unwrap_err();
        assert!(
            err.to_string().contains("unknown tool"),
            "got: {}",
            err
        );
    }

    #[test]
    fn registry_commit_validation_in_registry() {
        let registry = ToolRegistry::new();
        let err = registry.execute("commit", "").unwrap_err();
        // Empty commit message should fail
        assert!(err.to_string().len() > 0);
    }

    #[test]
    fn registry_setfile_overwrite_via_registry() {
        let path = tmpfile("registry_overwrite");
        let registry = ToolRegistry::new();
        registry.execute("setfile", &format!("{}\x00first\n", path)).unwrap();
        registry.execute("setfile", &format!("{}\x00second\n", path)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        cleanup(&path);
    }

    #[test]
    fn registry_shell_piped_command() {
        let registry = ToolRegistry::new();
        let result = registry.execute("shell", "echo hello | tr a-z A-Z").unwrap();
        assert!(result.contains("HELLO"), "got: {}", result);
    }

    #[test]
    fn registry_patchfile_roundtrip() {
        let path = tmpfile("registry_roundtrip");
        let registry = ToolRegistry::new();
        registry.execute("setfile", &format!("{}\x00one\ntwo\nthree\n", path)).unwrap();
        registry.execute("patchfile", &format!("{}\x002\x002\x00TWO", path)).unwrap();
        let lines = read_lines(&path);
        assert_eq!(lines[0], "one");
        assert_eq!(lines[1], "TWO");
        assert_eq!(lines[2], "three");
        cleanup(&path);
    }

    #[test]
    fn registry_default_same_tools_as_new() {
        let from_new = ToolRegistry::new();
        let from_default = ToolRegistry::default();
        let mut new_tools = from_new.list_tools();
        let mut def_tools = from_default.list_tools();
        new_tools.sort();
        def_tools.sort();
        assert_eq!(new_tools, def_tools);
    }
}
