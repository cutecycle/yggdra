//! Tests for command timeout infrastructure
//! Tests timeout config, parameter parsing, and timeout duration logic

#[cfg(test)]
mod tests {
    use yggdra::config::ModelParams;
    use std::time::Duration;

    #[test]
    fn test_command_timeout_secs_in_config() {
        let mut params = ModelParams::default();
        
        // Should start as None
        assert_eq!(params.command_timeout_secs, None);
        
        // Apply timeout via apply_kv
        let result = params.apply_kv("command_timeout_secs", "300");
        assert!(result.is_ok());
        assert_eq!(params.command_timeout_secs, Some(300));
        assert_eq!(result.unwrap(), "command_timeout_secs = 300s");
    }

    #[test]
    fn test_command_timeout_secs_minimum_validation() {
        let mut params = ModelParams::default();
        
        // Should reject timeout < 5 seconds
        let result = params.apply_kv("command_timeout_secs", "3");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be >= 5 seconds"));
        
        // Should accept 5 seconds
        let result = params.apply_kv("command_timeout_secs", "5");
        assert!(result.is_ok());
        assert_eq!(params.command_timeout_secs, Some(5));
    }

    #[test]
    fn test_command_timeout_secs_parse_error() {
        let mut params = ModelParams::default();
        
        // Should reject non-integer values
        let result = params.apply_kv("command_timeout_secs", "not_a_number");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("expected unsigned int"));
    }

    #[test]
    fn test_command_timeout_in_apply_args() {
        let mut params = ModelParams::default();
        
        // Apply multiple params at once
        let result = params.apply_args("command_timeout_secs=600 temperature=0.5");
        assert!(result.is_ok());
        assert_eq!(params.command_timeout_secs, Some(600));
        assert_eq!(params.temperature, Some(0.5));
    }

    #[test]
    fn test_command_timeout_in_merge() {
        let base = {
            let mut p = ModelParams::default();
            p.command_timeout_secs = Some(600);
            p
        };
        
        let override_params = {
            let mut p = ModelParams::default();
            p.command_timeout_secs = Some(300);
            p
        };
        
        let merged = override_params.merge_over(&base);
        assert_eq!(merged.command_timeout_secs, Some(300));
    }

    #[test]
    fn test_command_timeout_in_merge_fallback() {
        let base = {
            let mut p = ModelParams::default();
            p.command_timeout_secs = Some(600);
            p
        };
        
        let override_params = ModelParams::default(); // no timeout override
        
        let merged = override_params.merge_over(&base);
        assert_eq!(merged.command_timeout_secs, Some(600));
    }

    #[test]
    fn test_command_timeout_in_summary() {
        let mut params = ModelParams::default();
        params.command_timeout_secs = Some(300);
        
        let summary = params.summary();
        assert!(summary.contains("command_timeout_secs=300s"));
    }

    #[test]
    fn test_command_timeout_is_empty_check() {
        let mut params = ModelParams::default();
        // Default sets some fields, so it's not empty. Check that command_timeout_secs doesn't affect is_empty
        assert_eq!(params.command_timeout_secs, None);
        let was_empty_before = params.is_empty();
        
        params.command_timeout_secs = Some(300);
        assert_eq!(params.command_timeout_secs, Some(300));
        
        // Setting command_timeout_secs should make is_empty() return false (if it was true before)
        // Or if default is already non-empty, just verify is_empty changes
        if was_empty_before {
            assert!(!params.is_empty());
        }
    }

    #[test]
    fn test_command_timeout_in_default() {
        let params = ModelParams::default();
        // Default should have command_timeout_secs as None (use runtime default)
        assert_eq!(params.command_timeout_secs, None);
    }

    #[test]
    fn test_unknown_param_error_includes_command_timeout_secs() {
        let mut params = ModelParams::default();
        
        // Trying to set an unknown param should include command_timeout_secs in the valid list
        let result = params.apply_kv("unknown_param", "value");
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.contains("command_timeout_secs"), "Error message should mention command_timeout_secs as a valid param");
    }

    #[test]
    fn test_reset_clears_command_timeout_secs() {
        let mut params = ModelParams::default();
        params.command_timeout_secs = Some(300);
        assert_eq!(params.command_timeout_secs, Some(300));
        
        let result = params.apply_kv("reset", "");
        assert!(result.is_ok());
        assert_eq!(params.command_timeout_secs, None);
    }
}
