//! Ollama client: interface to Ollama API for model inference.
//! Handles model discovery and streaming message generation with steering directives.

use crate::message::Message;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

/// Ollama client for communicating with Ollama API
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
}

/// Message format for Ollama API
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OllamaMessage {
    pub role: String,
    pub content: String,
}

/// Single chunk from streaming response
#[derive(Debug, Deserialize)]
struct StreamChunk {
    message: Option<OllamaMessage>,
    done: bool,
}

/// Response from Ollama generate endpoint (non-streaming)
#[derive(Debug, Deserialize)]
pub struct GenerateResponse {
    pub message: OllamaMessage,
}

/// Token event sent from streaming to UI
pub enum StreamEvent {
    Token(String),
    Done,
    Error(String),
}

impl OllamaClient {
    /// Create a new Ollama client and validate connection
    pub async fn new(endpoint: &str, model: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(300))
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
    fn build_messages(messages: &[Message], steering: Option<&str>) -> Vec<OllamaMessage> {
        let mut ollama_messages = Vec::new();

        if let Some(steer) = steering {
            ollama_messages.push(OllamaMessage {
                role: "system".to_string(),
                content: steer.to_string(),
            });
        }

        for msg in messages {
            // Skip "system" role messages — those are UI-only events (context warnings,
            // offline notices etc.) and must not be forwarded to the model.
            // The actual system prompt comes from the `steering` parameter above.
            if msg.role == "system" { continue; }
            let role = if msg.role == "tool" { "user" } else { &msg.role };
            ollama_messages.push(OllamaMessage {
                role: role.to_string(),
                content: msg.content.clone(),
            });
        }

        ollama_messages
    }

    /// Start a streaming generation. Returns a receiver that yields tokens as they arrive.
    pub fn generate_streaming(
        &self,
        messages: Vec<Message>,
        steering: Option<&str>,
    ) -> mpsc::UnboundedReceiver<StreamEvent> {
        let (tx, rx) = mpsc::unbounded_channel();

        let ollama_messages = Self::build_messages(&messages, steering);
        let request = GenerateRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: true,
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

            // Read streaming NDJSON response
            use futures_util::StreamExt;
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();

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
                                        if !msg.content.is_empty() {
                                            if tx.send(StreamEvent::Token(msg.content.clone())).is_err() {
                                                return; // receiver dropped
                                            }
                                        }
                                    }
                                    if chunk.done {
                                        let _ = tx.send(StreamEvent::Done);
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
            let _ = tx.send(StreamEvent::Done);
        });

        rx
    }

    /// Non-streaming generate (kept for agent loop use)
    pub async fn generate(
        &self,
        messages: Vec<Message>,
        steering: Option<&str>,
    ) -> Result<String> {
        let ollama_messages = Self::build_messages(&messages, steering);

        let request = GenerateRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: false,
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
    ) -> Result<GenerateResponse> {
        let request = GenerateRequest {
            model: model.to_string(),
            messages,
            stream: false,
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
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("qwen:3.5"));
        assert!(json.contains("stream\":true"));
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
    }

    #[test]
    fn test_build_messages_with_steering() {
        let msgs = vec![Message::new("user", "hi")];
        let result = OllamaClient::build_messages(&msgs, Some("[STEERING: be nice]"));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "system");
        assert!(result[0].content.contains("be nice"));
        assert_eq!(result[1].role, "user");
    }

    #[test]
    fn test_build_messages_no_steering() {
        let msgs = vec![Message::new("user", "hi")];
        let result = OllamaClient::build_messages(&msgs, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, "user");
    }

    #[test]
    fn test_build_messages_maps_tool_to_user() {
        let msgs = vec![
            Message::new("user", "search for main"),
            Message::new("assistant", "I'll search. [TOOL: rg main .]"),
            Message::new("tool", "[TOOL_OUTPUT: rg = found matches]"),
        ];
        let result = OllamaClient::build_messages(&msgs, None);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[1].role, "assistant");
        // "tool" role should be mapped to "user" for Ollama API compatibility
        assert_eq!(result[2].role, "user");
        assert!(result[2].content.contains("TOOL_OUTPUT"));
    }

    #[test]
    fn test_models_response_parsing() {
        let json = r#"{"models":[{"name":"llama2"},{"name":"qwen:3.5"}]}"#;
        let response: ModelsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.models.len(), 2);
        assert_eq!(response.models[0].name, "llama2");
    }
}
