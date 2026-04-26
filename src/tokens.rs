/// Token estimation for prompts.
///
/// This module provides heuristic-based token counting without requiring an external
/// tokenizer (which varies by model). The estimates are conservative and suitable for
/// warning when prompts may exceed context windows.

/// Estimate the number of tokens in a text string.
///
/// Uses a simple but effective heuristic: ~1 token per 4 characters of English text.
/// This is conservative and errs on the side of overestimation, which is safer for
/// context window management.
///
/// - Counts characters (excluding control chars)
/// - Applies ~0.25 tokens per character (1 token = 4 chars)
/// - Adds 1 token per line for spacing overhead
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    // Count printable characters (rough filter for control codes)
    let char_count = text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .count();

    // Count lines (adds overhead for formatting)
    let line_count = text.lines().count().max(1);

    // Estimate: 1 token ~= 4 chars, plus 1 token per line
    let char_tokens = (char_count + 3) / 4;  // round up
    let line_tokens = line_count / 2;  // ~0.5 tokens per line for spacing

    char_tokens + line_tokens
}

/// Check if a prompt will fit within a context window with overhead for response.
///
/// Returns `(fits, warn_threshold)` where:
/// - `fits` is true if the prompt uses < 75% of the context window (safety margin)
/// - `warn_threshold` is the token limit for warnings (80% of context)
pub fn check_fits_in_context(prompt_tokens: usize, context_size: usize) -> (bool, usize) {
    let fits_threshold = (context_size as f64 * 0.75) as usize;
    let warn_threshold = (context_size as f64 * 0.80) as usize;
    (prompt_tokens < fits_threshold, warn_threshold)
}

#[cfg(test)]
mod tokens_tests {
    use super::*;

    #[test]
    fn empty_string_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn short_text() {
        // "hello" = 5 chars, ~2 tokens (5/4 rounded up)
        let tokens = estimate_tokens("hello");
        assert!(tokens > 0 && tokens <= 3, "Expected 1-3 tokens, got {}", tokens);
    }

    #[test]
    fn longer_text() {
        // ~40 chars + newlines
        let text = "fn main() {}\nfn helper() {}\nfn other() {}";
        let tokens = estimate_tokens(text);
        assert!(tokens >= 8 && tokens <= 15, "Expected ~8-15 tokens, got {}", tokens);
    }

    #[test]
    fn multiline_adds_overhead() {
        let single = "hello world";
        let multi = "hello\nworld\nfoo\nbar";
        let single_tokens = estimate_tokens(single);
        let multi_tokens = estimate_tokens(multi);
        // Multi should be a bit higher due to line count bonus
        assert!(multi_tokens >= single_tokens, "Multiline should have overhead");
    }

    #[test]
    fn fits_in_context_threshold() {
        // 75% = fits with headroom, 80% = warn zone, 100% = danger
        let (fits, warn_threshold) = check_fits_in_context(7000, 10000);
        assert!(fits, "7000 should fit in 10000 (< 75%)");
        assert_eq!(warn_threshold, 8000, "80% of 10000 is 8000");

        let (fits, warn_threshold) = check_fits_in_context(8000, 10000);
        assert!(!fits, "8000 should NOT fit in 10000 (>= 75%, in warn zone)");
        assert_eq!(warn_threshold, 8000);
        
        let (fits, warn_threshold) = check_fits_in_context(9000, 10000);
        assert!(!fits, "9000 should NOT fit in 10000 (> 80%)");
        assert_eq!(warn_threshold, 8000);
    }

    #[test]
    fn fits_in_context_large_window() {
        let (fits, warn_threshold) = check_fits_in_context(70000, 100000);
        assert!(fits, "70k should fit in 100k (< 75%)");
        assert_eq!(warn_threshold, 80000, "80% of 100k is 80k");
    }
}
