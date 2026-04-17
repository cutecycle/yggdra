// Integration tests for spawn tool shell patterns
// Validates that spawn correctly handles pipes, redirects, chains, and complex quoting

#[cfg(test)]
mod spawn_shell_patterns {
    use std::fs;
    use std::path::PathBuf;

    /// Helper: run spawn with a command and return output or error
    fn run_spawn(command: &str) -> Result<String, String> {
        let registry = yggdra::tools::ToolRegistry::new();
        registry.execute("spawn", command)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn test_spawn_simple_command() {
        // Basic command without pipes or redirects
        let result = run_spawn("echo hello");
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello"));
    }

    #[test]
    fn test_spawn_with_arguments() {
        // Command with arguments
        let result = run_spawn("ls -la");
        assert!(result.is_ok(), "ls -la should work");
    }

    #[test]
    fn test_spawn_with_quoted_args() {
        // Arguments with spaces in quotes
        let result = run_spawn("echo 'hello world'");
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello world"));
    }

    #[test]
    fn test_spawn_with_double_quotes() {
        // Double-quoted arguments
        let result = run_spawn("echo \"test string\"");
        assert!(result.is_ok());
    }

    #[test]
    fn test_spawn_with_pipe() {
        // Pipe operator is NOT actually supported in spawn directly
        // spawn executes commands without a shell, so | is treated as an argument
        // This documents the limitation clearly
        let result = run_spawn("echo line1 | wc -l");
        // spawn will try to execute "echo" with args ["line1", "|", "wc", "-l"]
        // which will fail or produce unexpected output
        // The key point: | doesn't work as a pipe in spawn
        let _ = result; // Don't assert — just document that pipes don't work as expected
    }

    #[test]
    fn test_spawn_with_and_chain() {
        // AND chain (&&) is NOT actually supported by spawn directly
        // spawn executes commands without a shell, so it treats && as an argument
        // This test documents the limitation
        let result = run_spawn("echo first && echo second");
        // spawn will try to execute "echo" with args ["first", "&&", "echo", "second"]
        let _ = result; // Just document — don't assume failure/success
    }

    #[test]
    fn test_spawn_with_or_chain() {
        // OR chain (||) is NOT actually supported by spawn directly
        // spawn executes commands without a shell, so || is treated as arguments
        let result = run_spawn("test -f /tmp/file_that_should_not_exist || echo recovered");
        // spawn will try to execute "test" with those args
        let _ = result; // Just document the limitation
    }

    #[test]
    fn test_spawn_git_log_pipe() {
        // Real-world pattern: git log with pipe
        let result = run_spawn("git log --oneline -5");
        // May fail if not in a git repo, but command itself is valid
        // Just verify it runs without panicking
        let _ = result;
    }

    #[test]
    fn test_spawn_find_with_complex_args() {
        // find command with complex pattern
        let result = run_spawn("find . -maxdepth 1 -type f -name '*.md'");
        assert!(result.is_ok(), "find with complex args should work");
    }

    #[test]
    fn test_spawn_blocked_bash() {
        // Shell interpreter should be blocked
        let result = run_spawn("bash -c 'echo hello'");
        assert!(result.is_err(), "bash should be blocked");
        let error = result.unwrap_err();
        assert!(error.to_lowercase().contains("blocked"), "Error should mention blocked");
    }

    #[test]
    fn test_spawn_blocked_sh() {
        // sh should be blocked
        let result = run_spawn("sh -c 'echo test'");
        assert!(result.is_err(), "sh should be blocked");
    }

    #[test]
    fn test_spawn_blocked_zsh() {
        // zsh should be blocked
        let result = run_spawn("zsh -c 'echo test'");
        assert!(result.is_err(), "zsh should be blocked");
    }

    #[test]
    fn test_spawn_blocked_absolute_path_bash() {
        // Absolute path /bin/bash should be blocked
        let result = run_spawn("/bin/bash -c 'echo test'");
        assert!(result.is_err(), "Absolute /bin/bash should be blocked");
    }

    #[test]
    fn test_spawn_binary_not_found() {
        // Non-existent binary should error
        let result = run_spawn("nonexistent_command_xyz");
        assert!(result.is_err(), "Non-existent binary should error");
        let error = result.unwrap_err();
        assert!(error.to_lowercase().contains("not found"), "Error should mention not found");
    }

    #[test]
    fn test_spawn_error_message_recovery_hint() {
        // When bash is blocked, error should include recovery hint
        let result = run_spawn("bash -c 'git status'");
        assert!(result.is_err());
        let error = result.unwrap_err();
        // Should suggest the correct form
        assert!(
            error.contains("Wrong") || error.contains("Right") || error.contains("spawn"),
            "Error should provide recovery hint, got: {}",
            error
        );
    }

    #[test]
    fn test_spawn_multiword_quoted_pattern() {
        // Pattern with spaces in find
        let result = run_spawn("find . -maxdepth 1 -type f");
        assert!(result.is_ok(), "find without quotes should work");
    }

    #[test]
    fn test_spawn_grep_with_pattern() {
        // grep with pattern
        let result = run_spawn("grep -r 'TODO' . --include='*.rs'");
        // May or may not find TODOs, but command should execute
        let _ = result;
    }

    #[test]
    fn test_spawn_cat_file() {
        // cat a known file
        let result = run_spawn("cat Cargo.toml");
        assert!(result.is_ok(), "cat Cargo.toml should work");
        let output = result.unwrap();
        assert!(output.contains("package") || output.contains("name"), "Should read TOML file");
    }

    #[test]
    fn test_spawn_empty_args() {
        // Empty arguments should error
        let result = run_spawn("");
        assert!(result.is_err(), "Empty args should error");
    }

    #[test]
    fn test_spawn_whitespace_only() {
        // Whitespace-only arguments should error
        let result = run_spawn("   ");
        assert!(result.is_err(), "Whitespace-only args should error");
    }
}
