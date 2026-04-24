// Test harness for small local models: qwen:0.8b, qwen:2b, qwen:4b, gemma:2b, gemma:7b
// Runs on localhost:11434 (local Ollama)
// Displays expected vs actual output for debugging

use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone)]
struct TestCase {
    name: &'static str,
    prompt: &'static str,
    /// Expected patterns in output (one should match)
    expected_patterns: Vec<&'static str>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    
    let models = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        vec![
            "qwen:0.5b".to_string(),
            "qwen:2b".to_string(),
            "qwen:4b".to_string(),
            "gemma:2b".to_string(),
            "gemma:7b".to_string(),
        ]
    };

    let endpoint = std::env::var("OLLAMA_ENDPOINT").unwrap_or_else(|_| "http://localhost:11434".to_string());
    println!("🧪 Model Gauntlet Test Suite");
    println!("📍 Endpoint: {}", endpoint);
    println!("🤖 Models to test: {}", models.join(", "));
    println!();

    // Define test cases
    let test_cases = vec![
        TestCase {
            name: "JSON Tool Call",
            prompt: "Respond with a JSON tool call to search the web for weather. Use this format: {\"tool_calls\": [{\"name\": \"search\", \"arguments\": {\"query\": \"weather\"}}]}",
            expected_patterns: vec!["tool_calls", "search", "weather"],
        },
        TestCase {
            name: "Simple Math",
            prompt: "What is 2 + 2?",
            expected_patterns: vec!["4"],
        },
        TestCase {
            name: "Code Snippet",
            prompt: "Write a simple Rust function that adds two numbers.",
            expected_patterns: vec!["fn", "add", "+"],
        },
    ];

    let mut results: HashMap<String, Vec<TestResult>> = HashMap::new();

    for model in &models {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📦 Testing Model: {}", model);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // Check if model is available
        match check_model_available(&endpoint, model).await {
            Ok(true) => println!("✅ Model available"),
            Ok(false) => {
                println!("❌ Model not available on Ollama");
                continue;
            }
            Err(e) => {
                println!("⚠️  Error checking model: {}", e);
                continue;
            }
        }

        let mut model_results = vec![];

        for test_case in &test_cases {
            println!("\n🧪 Test: {}", test_case.name);
            println!("   Prompt: {}", test_case.prompt);

            match run_test(&endpoint, model, test_case).await {
                Ok(result) => {
                    let status = if result.passed { "✅ PASS" } else { "❌ FAIL" };
                    println!("{}", status);
                    println!("   Response (first 200 chars): {}", 
                        if result.response.len() > 200 {
                            format!("{}...", &result.response[..200])
                        } else {
                            result.response.clone()
                        }
                    );
                    println!("   Tokens: {}/{}ms", result.tokens_used, result.duration_ms);
                    println!("   Matched patterns: {}", result.matched_patterns.join(", "));
                    model_results.push(result);
                }
                Err(e) => {
                    println!("❌ ERROR: {}", e);
                    model_results.push(TestResult {
                        test_name: test_case.name.to_string(),
                        passed: false,
                        response: format!("Error: {}", e),
                        tokens_used: 0,
                        duration_ms: 0,
                        matched_patterns: vec![],
                    });
                }
            }
        }

        results.insert(model.clone(), model_results);
        println!();
    }

    // Summary
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📊 Summary");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    for model in &models {
        if let Some(model_results) = results.get(model) {
            let passed = model_results.iter().filter(|r| r.passed).count();
            let total = model_results.len();
            let avg_tokens = if !model_results.is_empty() {
                model_results.iter().map(|r| r.tokens_used).sum::<u32>() / total as u32
            } else {
                0
            };
            println!("{}: {}/{} tests passed (avg {} tokens)", model, passed, total, avg_tokens);
        }
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct TestResult {
    test_name: String,
    passed: bool,
    response: String,
    tokens_used: u32,
    duration_ms: u32,
    matched_patterns: Vec<String>,
}

async fn check_model_available(endpoint: &str, model: &str) -> anyhow::Result<bool> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/tags", endpoint);
    
    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        return Ok(false);
    }

    let body = response.json::<serde_json::Value>().await?;
    if let Some(models) = body.get("models").and_then(|m| m.as_array()) {
        Ok(models.iter().any(|m| {
            m.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == model)
                .unwrap_or(false)
        }))
    } else {
        Ok(false)
    }
}

async fn run_test(endpoint: &str, model: &str, test_case: &TestCase) -> anyhow::Result<TestResult> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/chat", endpoint);

    let request = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": test_case.prompt
            }
        ],
        "stream": false
    });

    let start = Instant::now();
    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await?;
    
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("HTTP {}", response.status()));
    }

    let body = response.json::<serde_json::Value>().await?;
    let duration = start.elapsed();

    let response_text = body
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let prompt_tokens = body
        .get("prompt_eval_count")
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;

    let completion_tokens = body
        .get("eval_count")
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as u32;

    let total_tokens = prompt_tokens + completion_tokens;

    // Check if response matches expected patterns
    let matched_patterns: Vec<String> = test_case
        .expected_patterns
        .iter()
        .filter(|pattern| response_text.to_lowercase().contains(&pattern.to_lowercase()))
        .map(|p| p.to_string())
        .collect();

    let passed = !matched_patterns.is_empty();

    Ok(TestResult {
        test_name: test_case.name.to_string(),
        passed,
        response: response_text,
        tokens_used: total_tokens,
        duration_ms: duration.as_millis() as u32,
        matched_patterns,
    })
}
