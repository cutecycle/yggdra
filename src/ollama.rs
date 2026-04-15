//! Ollama client: interface to Ollama API for model inference.
//! Handles model discovery and streaming message generation with steering directives.

use crate::dlog;
use crate::message::Message;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

/// Ollama client for communicating with Ollama API
#[derive(Clone)]
pub struct OllamaClient {
    endpoint: String,
    model: String,
    client: reqwest::Client,
}

/// Model information from Ollama API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub modified_at: Option<String>,
    pub size: Option<u64>,
}

/// Response from Ollama models endpoint
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
}

/// Request format for Ollama chat endpoint
#[derive(Debug, Serialize)]
struct GenerateRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
}

/// Sampling options forwarded to Ollama (all fields optional — unset = Ollama defaults)
#[derive(Debug, Serialize, Default)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_ctx: Option<u32>,
}

impl OllamaOptions {
    fn from_params(p: &crate::config::ModelParams) -> Option<Self> {
        if p.is_empty() {
            return None;
        }
        Some(OllamaOptions {
            temperature: p.temperature,
            top_k: p.top_k,
            top_p: p.top_p,
            repeat_penalty: p.repeat_penalty,
            num_predict: p.num_predict,
            num_ctx: p.num_ctx,
        })
    }
}

/// Message format for Ollama API
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OllamaMessage {
    pub role: String,
    pub content: String,
}

/// Single chunk from streaming response
#[derive(Debug, Deserialize)]
struct StreamChunkMessage {
    content: String,
    #[serde(default)]
    thinking: String,
}

/// Single chunk from streaming response
#[derive(Debug, Deserialize)]
struct StreamChunk {
    message: Option<StreamChunkMessage>,
    done: bool,
    /// Tokens in prompt (only present on final done=true chunk)
    prompt_eval_count: Option<u32>,
    /// Tokens generated (only present on final done=true chunk)
    eval_count: Option<u32>,
    /// Time spent generating tokens in nanoseconds (only on final chunk)
    eval_duration: Option<u64>,
}

/// Response from Ollama generate endpoint (non-streaming)
#[derive(Debug, Deserialize)]
pub struct GenerateResponse {
    pub message: OllamaMessage,
}

/// Token event sent from streaming to UI
pub enum StreamEvent {
    Token(String),
    /// Stream finished with generation stats
    Done {
        prompt_tokens: u32,
        gen_tokens: u32,
        had_thinking: bool,
        /// Time spent generating tokens (nanoseconds), for tok/s computation
        eval_duration_ns: Option<u64>,
    },
    Error(String),
}

impl OllamaClient {
    /// Create a new Ollama client and validate connection
    pub async fn new(endpoint: &str, model: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(1800))
            .build()?;

        let ollama_client = Self {
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            client,
        };

        match ollama_client.list_models().await {
            Ok(_) => {
                eprintln!("✅ Ollama connection validated: {}", endpoint);
                Ok(ollama_client)
            }
            Err(e) => {
                let friendly_msg = if e.to_string().contains("connection refused") {
                    format!("Ollama is not running at {}", endpoint)
                } else if e.to_string().contains("timeout") {
                    format!("Ollama at {} is not responding", endpoint)
                } else {
                    e.to_string()
                };
                eprintln!("❌ Ollama connection failed: {}", friendly_msg);
                Err(e)
            }
        }
    }

    pub fn endpoint(&self) -> &str { &self.endpoint }
    pub fn model(&self) -> &str { &self.model }

    /// Reuse an existing validated client but switch to a different model name.
    /// No network round-trip — the underlying reqwest::Client is Arc-backed and cheap to clone.
    pub fn new_with_existing(existing: Self, model: &str) -> Self {
        Self { model: model.to_string(), ..existing }
    }

    /// Fetch list of available models from Ollama
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/api/tags", self.endpoint);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to connect to Ollama at {}: {}", self.endpoint, e))?;

        if !response.status().is_success() {
            return Err(anyhow!("Ollama returned error: {}", response.status()));
        }

        let data: ModelsResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse models response: {}", e))?;

        Ok(data.models)
    }

    /// Get the last loaded model (most recently modified) from Ollama
    pub async fn get_last_loaded_model(&self) -> Result<String> {
        let models = self.list_models().await?;

        if models.is_empty() {
            return Err(anyhow!("No models available in Ollama"));
        }

        let last_model = models
            .iter()
            .max_by_key(|m| m.modified_at.as_deref().unwrap_or(""))
            .ok_or_else(|| anyhow!("Failed to determine last loaded model"))?;

        Ok(last_model.name.clone())
    }

    /// Build the OllamaMessage list with steering injected as system message.
    /// Maps "tool" role to "user" for Ollama API compatibility.
    ///
    /// `tool_output_cap`: if Some(n), truncate any [TOOL_OUTPUT:] message content
    /// exceeding n chars (full content stays in SQLite, never lost).
    ///
    /// `context_window`: if Some(n), apply a sliding window budget of 80% of n
    /// tokens (estimated as chars/4), dropping oldest full turns first while
    /// always keeping the system prompt and the first user message.
    fn build_messages(
        messages: &[Message],
        steering: Option<&str>,
        tool_output_cap: Option<usize>,
        context_window: Option<u32>,
    ) -> Vec<OllamaMessage> {
        let mut ollama_messages = Vec::new();

        if let Some(steer) = steering {
            ollama_messages.push(OllamaMessage {
                role: "system".to_string(),
                content: steer.to_string(),
            });
        }

        let cap = tool_output_cap.unwrap_or(3000);

        for msg in messages {
            // Skip UI-only roles — not forwarded to the model.
            if msg.role == "system" || msg.role == "clock" { continue; }
            // "kick" and "tool" are forwarded as "user" turns
            let role = if msg.role == "tool" || msg.role == "kick" { "user" } else { &msg.role };

            // Truncate tool output messages that exceed the cap
            let content = if msg.content.contains("[TOOL_OUTPUT:") && msg.content.len() > cap {
                let truncated = &msg.content[..cap];
                // Find a clean line boundary to avoid cutting mid-line
                let cut = truncated.rfind('\n').unwrap_or(cap);
                format!("{}\n[...{} chars truncated]", &msg.content[..cut], msg.content.len() - cut)
            } else {
                msg.content.clone()
            };

            ollama_messages.push(OllamaMessage {
                role: role.to_string(),
                content,
            });
        }

        // Sliding window: if we have a context window budget, drop oldest turns
        // until the estimated token count fits in 80% of the window.
        if let Some(window) = context_window {
            let budget_chars = (window as usize * 4 * 8) / 10; // 80% of window, chars = tokens*4
            let total_chars: usize = ollama_messages.iter().map(|m| m.content.len()).sum();
            if total_chars > budget_chars {
                let before = ollama_messages.len();
                // Always keep index 0 (system prompt if present, else first user message).
                // Find the first user message index so we can preserve the task framing.
                let first_user_idx = ollama_messages.iter().position(|m| m.role == "user");
                let keep_until = first_user_idx.map(|i| i + 1).unwrap_or(1);

                // Drop turns from position `keep_until` onward until we fit
                while ollama_messages.len() > keep_until + 1 {
                    let chars: usize = ollama_messages.iter().map(|m| m.content.len()).sum();
                    if chars <= budget_chars { break; }
                    // Drop a pair (assistant + following user/tool-result) or single message
                    ollama_messages.remove(keep_until);
                }
                let after = ollama_messages.len();
                dlog!("build_messages: sliding-window dropped {} msgs — total_chars={} budget={} window={}",
                    before - after, total_chars, budget_chars, window);
            }
        }

        let total_est_tokens: usize = ollama_messages.iter().map(|m| m.content.len() / 4).sum();
        dlog!("build_messages: out={} msgs est_tokens={}", ollama_messages.len(), total_est_tokens);

        ollama_messages
    }

    /// Start a streaming generation. Returns a receiver that yields tokens as they arrive.
    pub fn generate_streaming(
        &self,
        messages: Vec<Message>,
        steering: Option<&str>,
        params: crate::config::ModelParams,
        tool_output_cap: Option<usize>,
        context_window: Option<u32>,
    ) -> mpsc::UnboundedReceiver<StreamEvent> {
        let (tx, rx) = mpsc::unbounded_channel();

        let ollama_messages = Self::build_messages(&messages, steering, tool_output_cap, context_window);
        dlog!("generate_streaming: model={} num_ctx={:?} in_msgs={} out_msgs={} est_tokens={}",
            self.model,
            params.num_ctx,
            messages.len(),
            ollama_messages.len(),
            ollama_messages.iter().map(|m| m.content.len() / 4).sum::<usize>());
        let request = GenerateRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: true,
            options: OllamaOptions::from_params(&params),
        };
        let url = format!("{}/api/chat", self.endpoint);
        let client = self.client.clone();

        tokio::spawn(async move {
            let response = match client.post(&url).json(&request).send().await {
                Ok(r) => r,
                Err(e) => {
                    dlog!("generate_streaming: request failed: {e}");
                    let _ = tx.send(StreamEvent::Error(format!("Request failed: {}", e)));
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                dlog!("generate_streaming: HTTP error {status}: {body}");
                let _ = tx.send(StreamEvent::Error(format!("Ollama error {}: {}", status, body)));
                return;
            }

            dlog!("generate_streaming: HTTP OK — reading stream");

            // Read streaming NDJSON response
            use futures_util::StreamExt;
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut had_thinking = false;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        // Process complete lines
                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim().to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.is_empty() { continue; }

                            match serde_json::from_str::<StreamChunk>(&line) {
                                Ok(chunk) => {
                                    if let Some(msg) = &chunk.message {
                                        if !msg.thinking.is_empty() {
                                            had_thinking = true;
                                        }
                                        if !msg.content.is_empty() {
                                            if tx.send(StreamEvent::Token(msg.content.clone())).is_err() {
                                                return; // receiver dropped
                                            }
                                        }
                                    }
                                    if chunk.done {
                                        let prompt_tokens = chunk.prompt_eval_count.unwrap_or(0);
                                        let gen_tokens = chunk.eval_count.unwrap_or(0);
                                        dlog!("generate_streaming: stream DONE prompt_tokens={prompt_tokens} gen_tokens={gen_tokens} had_thinking={had_thinking}");
                                        let _ = tx.send(StreamEvent::Done {
                                            prompt_tokens,
                                            gen_tokens,
                                            had_thinking,
                                            eval_duration_ns: chunk.eval_duration,
                                        });
                                        return;
                                    }
                                }
                                Err(_) => {
                                    // Skip unparseable lines (e.g. partial JSON)
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(StreamEvent::Error(format!("Stream error: {}", e)));
                        return;
                    }
                }
            }

            // Stream ended without done signal
            let _ = tx.send(StreamEvent::Done {
                prompt_tokens: 0,
                gen_tokens: 0,
                had_thinking,
                eval_duration_ns: None,
            });
        });

        rx
    }

    /// Streaming variant that takes pre-built OllamaMessages (used by agent subloops).
    pub fn stream_messages(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        params: &crate::config::ModelParams,
    ) -> mpsc::UnboundedReceiver<StreamEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        let request = GenerateRequest {
            model: model.to_string(),
            messages,
            stream: true,
            options: OllamaOptions::from_params(params),
        };
        let url = format!("{}/api/chat", self.endpoint);
        let client = self.client.clone();

        tokio::spawn(async move {
            let response = match client.post(&url).json(&request).send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(format!("Request failed: {}", e)));
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let _ = tx.send(StreamEvent::Error(format!("Ollama error {}: {}", status, body)));
                return;
            }

            use futures_util::StreamExt;
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut had_thinking = false;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim().to_string();
                            buffer = buffer[newline_pos + 1..].to_string();
                            if line.is_empty() { continue; }
                            match serde_json::from_str::<StreamChunk>(&line) {
                                Ok(chunk) => {
                                    if let Some(msg) = &chunk.message {
                                        if !msg.thinking.is_empty() {
                                            had_thinking = true;
                                        }
                                        if !msg.content.is_empty() {
                                            if tx.send(StreamEvent::Token(msg.content.clone())).is_err() {
                                                return;
                                            }
                                        }
                                    }
                                    if chunk.done {
                                        let p = chunk.prompt_eval_count.unwrap_or(0);
                                        let g = chunk.eval_count.unwrap_or(0);
                                        let _ = tx.send(StreamEvent::Done {
                                            prompt_tokens: p,
                                            gen_tokens: g,
                                            had_thinking,
                                            eval_duration_ns: chunk.eval_duration,
                                        });
                                        return;
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(StreamEvent::Error(format!("Stream error: {}", e)));
                        return;
                    }
                }
            }
            let _ = tx.send(StreamEvent::Done {
                prompt_tokens: 0,
                gen_tokens: 0,
                had_thinking,
                eval_duration_ns: None,
            });
        });

        rx
    }

    /// Non-streaming generate (kept for agent loop use)
    pub async fn generate(
        &self,
        messages: Vec<Message>,
        steering: Option<&str>,
        params: &crate::config::ModelParams,
        tool_output_cap: Option<usize>,
        context_window: Option<u32>,
    ) -> Result<String> {
        let ollama_messages = Self::build_messages(&messages, steering, tool_output_cap, context_window);

        let request = GenerateRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: false,
            options: OllamaOptions::from_params(params),
        };

        let url = format!("{}/api/chat", self.endpoint);

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send request to Ollama at {}: {}", self.endpoint, e))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Ollama returned error: {} - {}",
                response.status(),
                response.text().await.unwrap_or_default()
            ));
        }

        let data: GenerateResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse generate response: {}", e))?;

        Ok(data.message.content)
    }

    /// Send messages directly to Ollama (raw OllamaMessage format)
    pub async fn generate_with_messages(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        params: &crate::config::ModelParams,
    ) -> Result<GenerateResponse> {
        let request = GenerateRequest {
            model: model.to_string(),
            messages,
            stream: false,
            options: OllamaOptions::from_params(params),
        };

        let url = format!("{}/api/chat", self.endpoint);

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send request to Ollama at {}: {}", self.endpoint, e))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Ollama returned error: {} - {}",
                response.status(),
                response.text().await.unwrap_or_default()
            ));
        }

        let data: GenerateResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse generate response: {}", e))?;

        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_info_deserialization() {
        let json = r#"{"name":"qwen:3.5","modified_at":"2024-04-15T10:30:00Z","size":4294967296}"#;
        let model: ModelInfo = serde_json::from_str(json).unwrap();
        assert_eq!(model.name, "qwen:3.5");
        assert_eq!(model.size, Some(4294967296));
    }

    #[test]
    fn test_ollama_message_format() {
        let msg = OllamaMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("user"));
        assert!(json.contains("Hello"));
    }

    #[test]
    fn test_generate_request_format() {
        let messages = vec![OllamaMessage {
            role: "user".to_string(),
            content: "Test".to_string(),
        }];
        let req = GenerateRequest {
            model: "qwen:3.5".to_string(),
            messages,
            stream: true,
            options: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("qwen:3.5"));
        assert!(json.contains("stream\":true"));
        // options absent when None
        assert!(!json.contains("options"));
    }

    #[test]
    fn test_generate_request_with_params() {
        use crate::config::ModelParams;
        let params = ModelParams { temperature: Some(0.7), top_k: Some(40), ..Default::default() };
        let opts = OllamaOptions::from_params(&params).unwrap();
        let json = serde_json::to_string(&opts).unwrap();
        assert!(json.contains("\"temperature\":0.7"));
        assert!(json.contains("\"top_k\":40"));
        assert!(!json.contains("top_p")); // unset field omitted
    }

    #[test]
    fn test_stream_chunk_parsing() {
        let json = r#"{"message":{"role":"assistant","content":"Hello"},"done":false}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert!(!chunk.done);
        assert_eq!(chunk.message.unwrap().content, "Hello");
    }

    #[test]
    fn test_stream_chunk_done() {
        let json = r#"{"message":{"role":"assistant","content":""},"done":true}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert!(chunk.done);
        assert!(chunk.eval_duration.is_none());
    }

    #[test]
    fn test_stream_chunk_with_eval_duration() {
        let json = r#"{"message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":100,"eval_count":50,"eval_duration":2000000000}"#;
        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert!(chunk.done);
        assert_eq!(chunk.eval_count, Some(50));
        assert_eq!(chunk.eval_duration, Some(2_000_000_000));
        // 50 tokens in 2 seconds = 25 tok/s
        let rate = chunk.eval_count.unwrap() as f64
            / (chunk.eval_duration.unwrap() as f64 / 1_000_000_000.0);
        assert!((rate - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_infer_rate_zero_duration() {
        // Zero duration should not cause div-by-zero
        let eval_duration_ns: Option<u64> = Some(0);
        let gen_tokens: u32 = 50;
        let rate = match eval_duration_ns {
            Some(ns) if ns > 0 && gen_tokens > 0 =>
                Some(gen_tokens as f64 / (ns as f64 / 1_000_000_000.0)),
            _ => None,
        };
        assert!(rate.is_none());
    }

    #[test]
    fn test_infer_rate_zero_tokens() {
        // Zero tokens should yield None
        let eval_duration_ns: Option<u64> = Some(1_000_000_000);
        let gen_tokens: u32 = 0;
        let rate = match eval_duration_ns {
            Some(ns) if ns > 0 && gen_tokens > 0 =>
                Some(gen_tokens as f64 / (ns as f64 / 1_000_000_000.0)),
            _ => None,
        };
        assert!(rate.is_none());
    }

    #[test]
    fn test_build_messages_with_steering() {
        let msgs = vec![Message::new("user", "hi")];
        let result = OllamaClient::build_messages(&msgs, Some("[STEERING: be nice]"), None, None);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "system");
        assert!(result[0].content.contains("be nice"));
        assert_eq!(result[1].role, "user");
    }

    #[test]
    fn test_build_messages_no_steering() {
        let msgs = vec![Message::new("user", "hi")];
        let result = OllamaClient::build_messages(&msgs, None, None, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, "user");
    }

    #[test]
    fn test_build_messages_maps_tool_to_user() {
        let msgs = vec![
            Message::new("user", "search for main"),
            Message::new("assistant", r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "main", "directory": "."}}]}"#),
            Message::new("tool", "[TOOL_OUTPUT: rg = found matches]"),
        ];
        let result = OllamaClient::build_messages(&msgs, None, None, None);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[1].role, "assistant");
        // "tool" role should be mapped to "user" for Ollama API compatibility
        assert_eq!(result[2].role, "user");
        assert!(result[2].content.contains("TOOL_OUTPUT"));
    }

    #[test]
    fn test_build_messages_tool_output_cap() {
        let big_output = format!("[TOOL_OUTPUT: rg = {}]", "x".repeat(5000));
        let msgs = vec![
            Message::new("user", "search"),
            Message::new("assistant", r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "x", "directory": "."}}]}"#),
            Message::new("tool", big_output),
        ];
        let result = OllamaClient::build_messages(&msgs, None, Some(3000), None);
        // The tool output message should be truncated
        let tool_msg = &result[2];
        assert!(tool_msg.content.len() < 5000 + 50);
        assert!(tool_msg.content.contains("[...") && tool_msg.content.contains("chars truncated]"));
    }

    #[test]
    fn test_build_messages_no_cap_passthrough() {
        // Small tool output under cap should be left intact
        let msgs = vec![
            Message::new("tool", "[TOOL_OUTPUT: rg = found 3 lines]"),
        ];
        let result = OllamaClient::build_messages(&msgs, None, Some(3000), None);
        assert_eq!(result[0].content, "[TOOL_OUTPUT: rg = found 3 lines]");
    }

    #[test]
    fn test_build_messages_sliding_window() {
        // Create a context that exceeds 80% of a tiny window (100 tokens = 400 chars budget)
        let big_content = "A".repeat(200);
        let msgs = vec![
            Message::new("user", "first user message"),
            Message::new("assistant", big_content.clone()),
            Message::new("user", big_content.clone()),
            Message::new("assistant", big_content.clone()),
            Message::new("user", big_content.clone()),
        ];
        // context_window=100 → budget = 100*4*80% = 320 chars
        let result = OllamaClient::build_messages(&msgs, None, None, Some(100));
        // Some old turns should have been dropped
        assert!(result.len() < msgs.len());
        // First user message must always be preserved
        assert_eq!(result[0].content, "first user message");
    }

    #[test]
    fn test_models_response_parsing() {
        let json = r#"{"models":[{"name":"llama2"},{"name":"qwen:3.5"}]}"#;
        let response: ModelsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.models.len(), 2);
        assert_eq!(response.models[0].name, "llama2");
    }
}
