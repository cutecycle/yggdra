/// Ollama client: interface to Ollama API for model inference
/// Handles model discovery and message generation with steering directives

use crate::message::Message;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

/// Request format for Ollama generate endpoint
#[derive(Debug, Serialize)]
struct GenerateRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
}

/// Message format for Ollama API
#[derive(Debug, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

/// Response from Ollama generate endpoint (non-streaming)
#[derive(Debug, Deserialize)]
struct GenerateResponse {
    message: OllamaMessage,
}

impl OllamaClient {
    /// Create a new Ollama client and validate connection
    pub async fn new(endpoint: &str, model: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        let ollama_client = Self {
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            client,
        };

        // Validate connection by listing models
        let _ = ollama_client.list_models().await?;

        eprintln!("✅ Ollama connection validated: {}", endpoint);
        Ok(ollama_client)
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
            return Err(anyhow!(
                "Ollama returned error: {}",
                response.status()
            ));
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

        // Find model with most recent modified_at timestamp
        let last_model = models
            .iter()
            .max_by_key(|m| m.modified_at.as_deref().unwrap_or(""))
            .ok_or_else(|| anyhow!("Failed to determine last loaded model"))?;

        Ok(last_model.name.clone())
    }

    /// Send a message to Ollama and get response
    /// If steering is provided, it will be prepended to the system prompt
    pub async fn generate(
        &self,
        messages: Vec<Message>,
        steering: Option<&str>,
    ) -> Result<String> {
        // Convert messages to Ollama format, injecting steering into system prompt if needed
        let mut ollama_messages = Vec::new();

        for (i, msg) in messages.iter().enumerate() {
            let content = if i == 0 && steering.is_some() {
                // Inject steering directive into first (system) message
                format!(
                    "{}\n{}",
                    steering.unwrap(),
                    msg.content
                )
            } else {
                msg.content.clone()
            };

            ollama_messages.push(OllamaMessage {
                role: msg.role.clone(),
                content,
            });
        }

        // If no messages but steering exists, prepend a system message
        if ollama_messages.is_empty() && steering.is_some() {
            ollama_messages.push(OllamaMessage {
                role: "system".to_string(),
                content: steering.unwrap().to_string(),
            });
        }

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
            .map_err(|e| {
                anyhow!(
                    "Failed to send request to Ollama at {}: {}",
                    self.endpoint,
                    e
                )
            })?;

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
        let messages = vec![
            OllamaMessage {
                role: "user".to_string(),
                content: "Test".to_string(),
            },
        ];
        let req = GenerateRequest {
            model: "qwen:3.5".to_string(),
            messages,
            stream: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("qwen:3.5"));
        assert!(json.contains("stream\":false"));
    }

    #[test]
    fn test_steering_injection() {
        // Test that steering directive is properly formatted
        let steering = "[STEERING: output must be valid JSON]";
        let injected = format!("{}\nUser message", steering);
        assert!(injected.contains("[STEERING:"));
        assert!(injected.contains("User message"));
    }

    #[test]
    fn test_models_response_parsing() {
        let json = r#"{"models":[{"name":"llama2"},{"name":"qwen:3.5"}]}"#;
        let response: ModelsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.models.len(), 2);
        assert_eq!(response.models[0].name, "llama2");
    }
}
