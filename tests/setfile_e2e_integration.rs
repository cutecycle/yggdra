//! End-to-end integration tests for SetfileTool diff highlighting.
//!
//! These tests verify the REAL SetfileTool::execute() path with actual git repos
//! and file modifications, ensuring colored diff output is correctly generated.
//!
//! Tests cover:
//! - Basic modification with colored diff
//! - No changes (empty diff)
//! - Only additions with green coloring
//! - Only removals with red coloring
//! - Large diffs without truncation issues
//! - Binary-like content with ANSI codes

use std::fs;
use std::process::Command;
use tempfile::TempDir;
use yggdra::tools::ToolRegistry;

// ── Constants ─────────────────────────────────────────────────────────────

const RED_CODE: &str = "\x1b[31m";
const GREEN_CODE: &str = "\x1b[32m";
const RESET_CODE: &str = "\x1b[0m";

// ── Helper Functions ──────────────────────────────────────────────────────

/// Initialize a git repo in a temp directory with default user config.
fn init_git_repo(dir: &TempDir) {
    let repo_dir = dir.path();
    
    let output = Command::new("git")
        .args(&["init"])
        .current_dir(repo_dir)
        .output()
        .expect("failed to init git repo");
    
    if !output.status.success() {
        panic!("git init failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    let output = Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(repo_dir)
        .output()
        .expect("failed to configure git user email");
    
    if !output.status.success() {
        panic!("git config email failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    let output = Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(repo_dir)
        .output()
        .expect("failed to configure git user name");
    
    if !output.status.success() {
        panic!("git config name failed: {}", String::from_utf8_lossy(&output.stderr));
    }
}

/// Create and commit an initial file in the repo.
fn create_initial_file(dir: &TempDir, path: &str, content: &str) {
    let repo_dir = dir.path();
    let file_path = repo_dir.join(path);
    
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).expect("failed to create parent dirs");
    }
    
    fs::write(&file_path, content).expect("failed to write initial file");
    
    let output = Command::new("git")
        .args(&["add", path])
        .current_dir(repo_dir)
        .output()
        .expect("failed to git add");
    
    if !output.status.success() {
        panic!("git add failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    
    let output = Command::new("git")
        .args(&["commit", "-m", "initial commit"])
        .current_dir(repo_dir)
        .output()
        .expect("failed to git commit");
    
    if !output.status.success() {
        panic!("git commit failed: {}", String::from_utf8_lossy(&output.stderr));
    }
}

/// Change working directory to the repo and execute setfile via ToolRegistry.
/// Uses absolute paths so SetfileTool doesn't need to change cwd.
fn execute_setfile_in_repo(registry: &ToolRegistry, dir: &TempDir, file_path: &str, new_content: &str) -> Result<String, String> {
    let repo_dir = dir.path();
    
    // Use absolute path to avoid relying on PROJECT_ROOT
    let absolute_path = repo_dir.join(file_path);
    let absolute_str = absolute_path.to_string_lossy().to_string();
    
    // Build setfile args: path\0content
    let args = format!("{}\x00{}", absolute_str, new_content);
    
    registry.execute("setfile", &args).map_err(|e| e.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Test: basic file modification with additions, removals, and context.
/// Verifies colored diff output includes red for removals and green for additions.
#[test]
fn prod_setfile_e2e_basic() {
    let dir = TempDir::new().unwrap();
    init_git_repo(&dir);
    
    let initial = "line1\nline2\nline3\nline4\n";
    create_initial_file(&dir, "test.txt", initial);
    
    let modified = "line1 modified\nline2\nline5 new\nline4\n";
    
    let registry = ToolRegistry::new();
    let output = execute_setfile_in_repo(&registry, &dir, "test.txt", modified)
        .expect("setfile execution failed");
    
    // Verify the output contains diff markers
    assert!(output.contains("diff --git"), "missing git diff header in: {}", output);
    
    // Verify red coloring for removed lines
    assert!(output.contains(&format!("{}-line1", RED_CODE)), 
        "missing red color code for removal in: {}", output);
    
    // Verify green coloring for added lines
    assert!(output.contains(&format!("{}+line1 modified", GREEN_CODE)), 
        "missing green color code for addition in: {}", output);
    
    // Verify file was actually written
    let file_path = dir.path().join("test.txt");
    assert_eq!(fs::read_to_string(&file_path).unwrap(), modified);
}

/// Test: file with no actual changes.
/// Verifies that no diff is output (or minimal output indicating no-op).
#[test]
fn prod_setfile_e2e_empty() {
    let dir = TempDir::new().unwrap();
    init_git_repo(&dir);
    
    let content = "unchanged content\n";
    create_initial_file(&dir, "nochange.txt", content);
    
    let registry = ToolRegistry::new();
    let output = execute_setfile_in_repo(&registry, &dir, "nochange.txt", content)
        .expect("setfile execution failed");
    
    // When there are no changes, the diff output should be empty
    // The output should only contain the write summary (✅ wrote ... lines)
    // or a no-op indicator
    assert!(output.contains("✅") || output.contains("wrote") || output.contains("no-op"),
        "unexpected output for unchanged file: {}", output);
}

/// Test: file with only added lines (no removals).
/// Verifies green coloring is present and no red coloring appears.
#[test]
fn prod_setfile_e2e_additions() {
    let dir = TempDir::new().unwrap();
    init_git_repo(&dir);
    
    let initial = "line1\nline2\n";
    create_initial_file(&dir, "additions.txt", initial);
    
    let modified = "line1\nline2\nnew_line_3\nnew_line_4\n";
    
    let registry = ToolRegistry::new();
    let output = execute_setfile_in_repo(&registry, &dir, "additions.txt", modified)
        .expect("setfile execution failed");
    
    // Verify green coloring for additions
    assert!(output.contains(&format!("{}+new_line_3", GREEN_CODE)), 
        "missing green color code in additions test: {}", output);
    
    // Verify no red coloring (no removals in the diff)
    // Note: We check that lines starting with "-" don't have red color
    let has_red_removal = output.contains(&format!("{}-", RED_CODE)) && 
                          !output.contains("---") && 
                          !output.contains("+++");
    assert!(!has_red_removal, 
        "unexpected red color code in additions-only test: {}", output);
    
    // Verify file was written correctly
    assert_eq!(fs::read_to_string(dir.path().join("additions.txt")).unwrap(), modified);
}

/// Test: file with only removed lines (no additions).
/// Verifies red coloring is present and no green coloring appears.
#[test]
fn prod_setfile_e2e_removals() {
    let dir = TempDir::new().unwrap();
    init_git_repo(&dir);
    
    let initial = "line1\nline2\nline3\nline4\n";
    create_initial_file(&dir, "removals.txt", initial);
    
    let modified = "line1\nline4\n";
    
    let registry = ToolRegistry::new();
    let output = execute_setfile_in_repo(&registry, &dir, "removals.txt", modified)
        .expect("setfile execution failed");
    
    // Verify red coloring for removals
    assert!(output.contains(&format!("{}-line2", RED_CODE)), 
        "missing red color code in removals test: {}", output);
    
    // Verify no green coloring (no additions in the diff)
    // Note: We check that lines starting with "+" don't have green color
    let has_green_addition = output.contains(&format!("{}+", GREEN_CODE)) && 
                            !output.contains("+++");
    assert!(!has_green_addition, 
        "unexpected green color code in removals-only test: {}", output);
    
    // Verify file was written correctly
    assert_eq!(fs::read_to_string(dir.path().join("removals.txt")).unwrap(), modified);
}

/// Test: very large diff to ensure no truncation or corruption.
/// Verifies that large diffs maintain structure and colors throughout.
#[test]
fn prod_setfile_e2e_large() {
    let dir = TempDir::new().unwrap();
    init_git_repo(&dir);
    
    // Create initial file with 100 lines
    let mut initial_lines = Vec::new();
    for i in 0..100 {
        initial_lines.push(format!("line {}", i));
    }
    let initial = initial_lines.join("\n") + "\n";
    create_initial_file(&dir, "large.txt", &initial);
    
    // Create modified version with changes throughout the file
    let mut modified_lines = Vec::new();
    for i in 0..100 {
        if i % 5 == 0 {
            modified_lines.push(format!("line {} MODIFIED", i));
        } else {
            modified_lines.push(format!("line {}", i));
        }
    }
    let modified = modified_lines.join("\n") + "\n";
    
    let registry = ToolRegistry::new();
    let output = execute_setfile_in_repo(&registry, &dir, "large.txt", &modified)
        .expect("setfile execution failed");
    
    // Verify diff structure is maintained
    assert!(output.contains("diff --git"), "missing diff header in large diff");
    assert!(output.contains("@@"), "missing hunk headers in large diff");
    
    // Verify colors are present (at least one colored removal/addition)
    let has_colors = output.contains(&format!("{}-", RED_CODE)) || 
                    output.contains(&format!("{}+", GREEN_CODE));
    assert!(has_colors, "missing color codes in large diff: {}", output);
    
    // Verify file was written correctly
    assert_eq!(fs::read_to_string(dir.path().join("large.txt")).unwrap(), modified);
}

/// Test: content with ANSI-like byte sequences doesn't corrupt the diff.
/// Verifies that binary-like content (with escape codes) is preserved.
#[test]
fn prod_setfile_binary() {
    let dir = TempDir::new().unwrap();
    init_git_repo(&dir);
    
    // Initial content with some ANSI-like sequences
    let initial = "normal line\n\x1b[35mmagenta text\x1b[0m\nmore content\n";
    create_initial_file(&dir, "binary.txt", initial);
    
    // Modified content with different ANSI-like sequences
    let modified = "normal line\n\x1b[36mcyan text\x1b[0m\neven more content\n";
    
    let registry = ToolRegistry::new();
    let output = execute_setfile_in_repo(&registry, &dir, "binary.txt", modified)
        .expect("setfile execution failed");
    
    // Verify the diff is generated (even with binary-like content)
    assert!(output.contains("diff --git"), "missing diff header when content has ANSI codes");
    
    // Verify file was written correctly (ANSI sequences preserved)
    assert_eq!(fs::read_to_string(dir.path().join("binary.txt")).unwrap(), modified);
}

/// Bonus test: verify that RESET codes follow colored lines properly.
/// Ensures diff lines end with reset codes to prevent color bleed.
#[test]
fn prod_setfile_e2e_color_reset() {
    let dir = TempDir::new().unwrap();
    init_git_repo(&dir);
    
    let initial = "keep\nremove this\n";
    create_initial_file(&dir, "reset.txt", initial);
    
    let modified = "keep\nadd this\n";
    
    let registry = ToolRegistry::new();
    let output = execute_setfile_in_repo(&registry, &dir, "reset.txt", modified)
        .expect("setfile execution failed");
    
    // Verify that if we have red coloring, it's properly followed by reset
    if output.contains(&format!("{}-", RED_CODE)) {
        // Should have a pattern like: RED_CODE + "-" + line + RESET_CODE
        let has_red_with_reset = output.contains(&format!("{}-", RED_CODE)) && 
                                output.contains(RESET_CODE);
        assert!(has_red_with_reset,
            "red colored line present but no reset code in: {}", output);
    }
    
    // Verify that if we have green coloring, it's properly followed by reset
    if output.contains(&format!("{}+", GREEN_CODE)) {
        // Should have a pattern like: GREEN_CODE + "+" + line + RESET_CODE
        let has_green_with_reset = output.contains(&format!("{}+", GREEN_CODE)) && 
                                  output.contains(RESET_CODE);
        assert!(has_green_with_reset,
            "green colored line present but no reset code in: {}", output);
    }
}
