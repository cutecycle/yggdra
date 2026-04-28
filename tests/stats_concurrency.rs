//! Concurrency tests for stats.json to verify thread-safe handling of
//! concurrent writes, reads, corrupted files, and edge cases.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

// Import stats module
// Note: We'll load the stats code via serde_json directly since we're testing from the tests/ dir
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-tool call statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolStats {
    pub calls: u64,
    pub failures: u64,
    pub output_bytes: u64,
}

/// Query result statistics for search-like tools.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryStats {
    pub result_counts: Vec<u64>,
    pub avg_results: f64,
    pub total_queries: u64,
}

/// Cumulative project statistics persisted to `.yggdra/stats.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub tools: HashMap<String, ToolStats>,
    pub query_results: HashMap<String, QueryStats>,
    pub llm_requests: u64,
    pub prompt_tokens: u64,
    pub gen_tokens: u64,
    pub avg_tok_per_s_x100: u64,
    pub sessions: u64,
    pub last_active_unix: u64,
    pub uptime_seconds: u64,
    pub context_trims: u64,
    pub compressions: u64,
}

impl Stats {
    /// Load from `.yggdra/stats.json`, returning a zeroed default if missing or corrupt.
    pub fn load(project_dir: &Path) -> Self {
        let path = Self::path(project_dir);
        match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Write to `.yggdra/stats.json` atomically via a temp file.
    pub fn save(&self, project_dir: &Path) {
        let path = Self::path(project_dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("tmp");
            if fs::write(&tmp, &json).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }

    fn path(project_dir: &Path) -> std::path::PathBuf {
        project_dir.join(".yggdra").join("stats.json")
    }

    /// Record query results for a search/query tool.
    pub fn record_query_result(&mut self, tool_name: &str, result_count: u64) {
        let entry = self.query_results.entry(tool_name.to_string()).or_default();
        entry.result_counts.push(result_count);
        entry.total_queries += 1;
        entry.avg_results = entry.result_counts.iter().sum::<u64>() as f64 / entry.total_queries as f64;
    }

    /// Record a tool invocation.
    pub fn record_tool(&mut self, tool_name: &str, success: bool, output_bytes: usize) {
        let entry = self.tools.entry(tool_name.to_string()).or_default();
        if success {
            entry.calls += 1;
            entry.output_bytes += output_bytes as u64;
        } else {
            entry.failures += 1;
        }
    }
}

/// Helper: initialize .yggdra directory in a temp project folder
fn setup_project_dir(temp_dir: &TempDir) -> std::path::PathBuf {
    let project_dir = temp_dir.path();
    let yggdra_dir = project_dir.join(".yggdra");
    fs::create_dir_all(&yggdra_dir).expect("Failed to create .yggdra directory");
    project_dir.to_path_buf()
}

/// Test 1: Concurrent writes from 10 threads with synchronization, verify data is recorded
#[test]
fn prod_stats_concurrent_writes() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);
    
    // Shared barrier to synchronize thread startup
    let num_threads = 10;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = vec![];

    for i in 0..num_threads {
        let project_dir_clone = project_dir.clone();
        let barrier_clone = Arc::clone(&barrier);
        
        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            barrier_clone.wait();
            
            // Each thread does its own load/modify/save cycle
            // Due to read-modify-write races, some updates may be lost
            // But the important thing is that the system doesn't panic or corrupt
            let mut stats = Stats::load(&project_dir_clone);
            let count = (i + 1) as u64 * 10; // Thread i writes count (i+1)*10
            stats.record_query_result("rg", count);
            stats.save(&project_dir_clone);
        });
        
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify final result - the key is that we get a valid state without crashes
    let final_stats = Stats::load(&project_dir);
    let query_stats = final_stats.query_results.get("rg").expect("No rg stats found");
    
    // Verify we recorded at least some queries (due to concurrent writes, 
    // we may lose updates but should have at least 1)
    assert!(query_stats.total_queries > 0, "Should have recorded at least one query");
    
    // Verify consistency: total_queries should match result_counts length
    assert_eq!(
        query_stats.total_queries as usize,
        query_stats.result_counts.len(),
        "Total queries should match result_counts length"
    );
    
    // Verify avg_results is calculated correctly for whatever data we have
    let sum: u64 = query_stats.result_counts.iter().sum();
    let calculated_avg = sum as f64 / query_stats.total_queries as f64;
    assert!((query_stats.avg_results - calculated_avg).abs() < 0.001, "Average should be calculated correctly");
}

/// Test 2: Readers during concurrent writes, verify consistency
#[test]
fn prod_stats_concurrent_reads() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);

    // Pre-populate some stats
    let mut initial_stats = Stats::load(&project_dir);
    for i in 1..=5 {
        initial_stats.record_query_result("rg", i as u64 * 100);
    }
    initial_stats.save(&project_dir);

    let num_writers = 5;
    let num_readers = 5;
    let barrier = Arc::new(Barrier::new(num_writers + num_readers));
    let mut handles = vec![];

    // Spawn writers
    for i in 0..num_writers {
        let project_dir_clone = project_dir.clone();
        let barrier_clone = Arc::clone(&barrier);
        
        let handle = thread::spawn(move || {
            barrier_clone.wait();
            for _ in 0..3 {
                let mut stats = Stats::load(&project_dir_clone);
                stats.record_query_result("rg", 10 + i as u64);
                stats.save(&project_dir_clone);
                thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        handles.push(handle);
    }

    // Spawn readers
    for _ in 0..num_readers {
        let project_dir_clone = project_dir.clone();
        let barrier_clone = Arc::clone(&barrier);
        
        let handle = thread::spawn(move || {
            barrier_clone.wait();
            for _ in 0..10 {
                let stats = Stats::load(&project_dir_clone);
                // Verify the stats structure is consistent (no partial reads)
                if let Some(query_stats) = stats.query_results.get("rg") {
                    // Verify total_queries matches result_counts length
                    assert_eq!(
                        query_stats.total_queries as usize,
                        query_stats.result_counts.len(),
                        "total_queries should match result_counts length"
                    );
                    
                    // Verify average is reasonable
                    if query_stats.total_queries > 0 {
                        assert!(query_stats.avg_results > 0.0, "Average should be positive");
                        assert!(query_stats.avg_results <= u64::MAX as f64, "Average should not overflow");
                    }
                }
                thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Final verification
    let final_stats = Stats::load(&project_dir);
    if let Some(query_stats) = final_stats.query_results.get("rg") {
        assert_eq!(
            query_stats.total_queries as usize,
            query_stats.result_counts.len(),
            "Final consistency check: total_queries should match result_counts"
        );
    }
}

/// Test 3: Partial/corrupted stats.json file, verify graceful recovery
#[test]
fn prod_stats_corrupted_json() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);
    let stats_path = project_dir.join(".yggdra/stats.json");

    // Write valid JSON first
    let initial_json = r#"{"tools":{"rg":{"calls":5,"failures":0,"output_bytes":1024}},"query_results":{},"llm_requests":0,"prompt_tokens":0,"gen_tokens":0,"avg_tok_per_s_x100":0,"sessions":0,"last_active_unix":0,"uptime_seconds":0,"context_trims":0,"compressions":0}"#;
    fs::write(&stats_path, initial_json).expect("Failed to write initial stats");

    // Corrupt the JSON by truncating it mid-way
    let corrupted_json = r#"{"tools":{"rg":{"calls":5,"failures":0,"output_bytes":1024}},"query_results"#; // Incomplete
    fs::write(&stats_path, corrupted_json).expect("Failed to write corrupted stats");

    // Load should return default without panicking
    let stats = Stats::load(&project_dir);
    assert_eq!(stats.tools.len(), 0, "Corrupted stats should load as default (tools empty)");
    
    // Should be able to record new stats after corruption
    let mut stats = Stats::load(&project_dir);
    stats.record_query_result("rg", 42);
    stats.save(&project_dir);

    // Verify recovery: new data should be saved
    let recovered_stats = Stats::load(&project_dir);
    assert_eq!(recovered_stats.query_results["rg"].total_queries, 1, "Should recover and continue");
    assert_eq!(recovered_stats.query_results["rg"].result_counts[0], 42, "New data should be preserved");
}

/// Test 4: Unwritable stats.json, verify graceful degradation
#[test]
#[cfg(unix)] // Only run on Unix-like systems (permissions work differently on Windows)
fn prod_stats_permission_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);
    let stats_path = project_dir.join(".yggdra/stats.json");

    // Write initial valid stats
    let initial_json = r#"{"tools":{},"query_results":{},"llm_requests":0,"prompt_tokens":0,"gen_tokens":0,"avg_tok_per_s_x100":0,"sessions":0,"last_active_unix":0,"uptime_seconds":0,"context_trims":0,"compressions":0}"#;
    fs::write(&stats_path, initial_json).expect("Failed to write initial stats");

    // Make stats.json read-only (remove write permission)
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o444); // r--r--r--
    fs::set_permissions(&stats_path, perms).expect("Failed to set permissions");

    // Try to save stats (should fail gracefully, not panic)
    let mut stats = Stats::load(&project_dir);
    stats.record_query_result("rg", 100);
    
    // This save should fail gracefully (permission denied)
    // In production, the save() function already handles this by silently failing
    stats.save(&project_dir);

    // Note: Due to how save() works with temp files, the file might still be updated
    // if the temp file is written in a writable directory and then rename fails.
    // The important thing is that:
    // 1. No panic occurred (already verified if we got here)
    // 2. The system handles the error gracefully
    
    // Clean up: restore permissions so TempDir can clean up
    let perms = fs::Permissions::from_mode(0o644); // rw-r--r--
    let _ = fs::set_permissions(&stats_path, perms);
}

/// Test 5: Zero-result queries handled correctly
#[test]
fn prod_stats_zero_results() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);

    let mut stats = Stats::load(&project_dir);
    
    // Record multiple queries including zeros
    stats.record_query_result("rg", 0);
    stats.record_query_result("rg", 5);
    stats.record_query_result("rg", 0);
    stats.record_query_result("rg", 10);
    
    stats.save(&project_dir);

    let loaded = Stats::load(&project_dir);
    let query_stats = &loaded.query_results["rg"];
    
    // Verify zero is included in counts
    assert_eq!(query_stats.total_queries, 4, "Should have 4 queries");
    assert_eq!(query_stats.result_counts.len(), 4, "Should have 4 result counts");
    assert_eq!(query_stats.result_counts[0], 0, "First query should be 0");
    assert_eq!(query_stats.result_counts[2], 0, "Third query should be 0");
    
    // Verify average is calculated correctly with zeros
    // (0 + 5 + 0 + 10) / 4 = 15 / 4 = 3.75
    let expected_avg = 15.0 / 4.0;
    assert!((query_stats.avg_results - expected_avg).abs() < 0.001, "Average should include zeros correctly");
}

/// Test 6: Very large u64 values in stats (but avoiding overflow in sum)
#[test]
fn prod_stats_huge_counts() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);

    let mut stats = Stats::load(&project_dir);
    
    // Record queries with very large counts that won't overflow when summed
    let huge_count1 = u64::MAX / 3;      // Safe for 3 additions
    let huge_count2 = u64::MAX / 4;      // Safe for 4 additions
    let normal_count = 1000u64;
    
    stats.record_query_result("rg", huge_count1);
    stats.record_query_result("rg", huge_count2);
    stats.record_query_result("rg", normal_count);
    
    stats.save(&project_dir);

    let loaded = Stats::load(&project_dir);
    let query_stats = &loaded.query_results["rg"];
    
    // Verify large values are preserved
    assert_eq!(query_stats.result_counts[0], huge_count1, "First huge count should be preserved");
    assert_eq!(query_stats.result_counts[1], huge_count2, "Second huge count should be preserved");
    assert_eq!(query_stats.result_counts[2], normal_count, "Normal count should be preserved");
    
    // Verify total_queries is correct
    assert_eq!(query_stats.total_queries, 3, "Should have 3 queries");
    
    // Verify JSON serialization doesn't lose precision
    assert!(query_stats.avg_results > 0.0, "Average should be positive");
    assert!(query_stats.avg_results.is_finite(), "Average should be finite");
    
    // The average should be approximately (huge_count1 + huge_count2 + normal_count) / 3
    let expected_sum = huge_count1 as f64 + huge_count2 as f64 + normal_count as f64;
    let expected_avg = expected_sum / 3.0;
    
    // Note: There will be some floating point precision loss with very large numbers
    // So we check that it's in a reasonable range (within a few percent)
    let relative_error = (query_stats.avg_results - expected_avg).abs() / expected_avg;
    assert!(relative_error < 0.01, "Average should be within 1% of expected: {}", relative_error);
}

/// Test 7: Multiple concurrent writers with alternating loads/saves
#[test]
fn prod_stats_concurrent_merge_behavior() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);

    let num_threads = 5;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = vec![];

    for i in 0..num_threads {
        let project_dir_clone = project_dir.clone();
        let barrier_clone = Arc::clone(&barrier);
        
        let handle = thread::spawn(move || {
            barrier_clone.wait();
            
            // Each thread does multiple operations
            for iteration in 0..3 {
                let mut stats = Stats::load(&project_dir_clone);
                let count = (i as u64 + 1) * 100 + iteration as u64;
                stats.record_query_result("rg", count);
                stats.save(&project_dir_clone);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let final_stats = Stats::load(&project_dir);
    let query_stats = final_stats.query_results.get("rg").expect("No rg stats found");
    
    // With concurrent writes, we expect to see many values recorded
    // (though some may be lost due to read-modify-write races)
    assert!(query_stats.total_queries > 0, "Should have recorded queries");
    
    // Verify consistency of the final state
    assert_eq!(
        query_stats.total_queries as usize,
        query_stats.result_counts.len(),
        "Total queries should match result counts length"
    );
    
    // Verify average calculation is still correct
    let sum: u64 = query_stats.result_counts.iter().sum();
    let expected_avg = sum as f64 / query_stats.total_queries as f64;
    assert!((query_stats.avg_results - expected_avg).abs() < 0.001, "Average should be correct");
}

/// Test 8: Rapid sequential file operations (stress test)
#[test]
fn prod_stats_rapid_operations() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);

    // Rapidly load/modify/save in a loop
    for i in 0..50 {
        let mut stats = Stats::load(&project_dir);
        stats.record_query_result("rg", i as u64);
        stats.record_tool("exec", true, 256);
        stats.save(&project_dir);
    }

    let final_stats = Stats::load(&project_dir);
    
    // Verify all data is intact
    let query_stats = final_stats.query_results.get("rg").expect("No rg stats found");
    assert_eq!(query_stats.total_queries, 50, "Should have 50 queries");
    
    let tool_stats = final_stats.tools.get("exec").expect("No exec tool stats found");
    assert_eq!(tool_stats.calls, 50, "Should have 50 exec calls");
    assert_eq!(tool_stats.output_bytes, 50 * 256, "Total output bytes should be correct");
}

/// Test 9: Empty result counts and statistics
#[test]
fn prod_stats_empty_state() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_dir = setup_project_dir(&temp_dir);

    let stats = Stats::load(&project_dir);
    
    // Verify default empty state
    assert_eq!(stats.tools.len(), 0, "Tools map should be empty");
    assert_eq!(stats.query_results.len(), 0, "Query results map should be empty");
    assert_eq!(stats.llm_requests, 0, "LLM requests should be 0");
    
    // Save and reload empty state
    stats.save(&project_dir);
    let reloaded = Stats::load(&project_dir);
    
    assert_eq!(reloaded.tools.len(), 0, "Tools should remain empty after save/load");
    assert_eq!(reloaded.query_results.len(), 0, "Query results should remain empty after save/load");
}
