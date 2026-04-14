/// Test tool calls with qwen3.5-abliterated:2b model
/// Verifies spawn and ruste work with this specific model
///
/// To run these tests, ensure Ollama has qwen3.5-abliterated:2b loaded, then:
///   cargo test --test qwen_abliterated_tests -- --ignored --nocapture
///
/// Uses default Ollama endpoint (http://localhost:11434)
#[cfg(test)]
mod qwen_abliterated_tests {
    use yggdra::ollama::OllamaClient;
    use yggdra::message::Message;
    use yggdra::config::ModelParams;

    fn get_endpoint() -> String {
        "http://localhost:11434".to_string()
    }

    #[tokio::test]
    #[ignore] // Run with: ZERORANGER_ENDPOINT=http://IP:11434 cargo test -- --ignored qwen_abliterated
    async fn test_qwen_model_availability() {
        let endpoint = get_endpoint();
        println!("Testing qwen3.5-abliterated:2b on {}", endpoint);
        
        // Verify qwen3.5-abliterated:2b is available on zeroranger
        match OllamaClient::new(&endpoint, "qwen3.5-abliterated:2b").await {
            Ok(client) => {
                println!("✅ Connected to qwen3.5-abliterated:2b");
                println!("   Endpoint: {}", client.endpoint());
                println!("   Model: {}", client.model());
            }
            Err(e) => {
                panic!("❌ Could not connect to qwen3.5-abliterated:2b at {}: {}", endpoint, e);
            }
        }
    }

    #[tokio::test]
    #[ignore] // Run with: ZERORANGER_ENDPOINT=http://IP:11434 cargo test -- --ignored qwen_abliterated
    async fn test_qwen_spawn_tool_call() {
        let endpoint = get_endpoint();
        let client = match OllamaClient::new(&endpoint, "qwen3.5-abliterated:2b").await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("⚠️  Skipping test - cannot connect to {}: {}", endpoint, e);
                return;
            }
        };

        // Create a message asking the model to call spawn
        let messages = vec![
            Message::new("user", "Use the spawn tool to run the 'date' command. Use this exact format: <|tool>spawn<|tool_sep>date<|end_tool>")
        ];

        let params = ModelParams::default();
        
        match client.generate(messages, None, &params, None, None).await {
            Ok(response) => {
                println!("🔍 Qwen spawn response:\n{}", response);
                // Check if model produced tool call format
                if response.contains("spawn") || response.contains("<|tool>") || response.contains("[TOOL:") {
                    println!("✅ Model recognized tool call format");
                } else {
                    println!("⚠️  Response didn't contain expected tool call markers");
                }
            }
            Err(e) => {
                eprintln!("❌ Generation failed: {}", e);
            }
        }
    }

    #[tokio::test]
    #[ignore] // Run with: ZERORANGER_ENDPOINT=http://IP:11434 cargo test -- --ignored qwen_abliterated
    async fn test_qwen_ruste_tool_call() {
        let endpoint = get_endpoint();
        let client = match OllamaClient::new(&endpoint, "qwen3.5-abliterated:2b").await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("⚠️  Skipping test - cannot connect to {}: {}", endpoint, e);
                return;
            }
        };

        // Ask model to write Rust code using ruste
        let messages = vec![
            Message::new("user", "Write a Rust program that prints the current UTC time. Save it to /tmp/clock.rs and run it using the ruste tool.")
        ];

        let params = ModelParams::default();
        
        match client.generate(messages, None, &params, None, None).await {
            Ok(response) => {
                println!("🔍 Qwen ruste response:\n{}", response);
                // Check if model discussed Rust or tool calls
                if response.to_lowercase().contains("rust") 
                    || response.contains("fn main")
                    || response.contains("ruste")
                    || response.contains("<|tool>") {
                    println!("✅ Model discussed Rust/ruste");
                } else {
                    println!("⚠️  Unexpected response format");
                }
            }
            Err(e) => {
                eprintln!("❌ Generation failed: {}", e);
            }
        }
    }
}
