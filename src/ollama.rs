//! Ollama client: interface to Ollama API for model inference.
//! Handles model discovery and streaming message generation with steering directives.
//! Supports both Ollama NDJSON protocol and OpenAI-compatible SSE protocol.

use crate::dlog;
use crate::message::Message;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// Detect endpoint type for display in status bar.
/// Returns: "Ollama-local", "Ollama-external", "OpenRouter", "llama.cpp", etc.
pub fn detect_endpoint_type(endpoint: &str) -> String {
    let lower = endpoint.to_lowercase();
    let is_local = lower.contains("localhost") 
        || lower.contains("127.0.0.1") 
        || lower.contains("::1")
        || lower.contains("192.168.") 
        || lower.contains("10.")
        || lower.contains("172.16.")
        || lower.contains("172.17.")
        || lower.contains("172.18.")
        || lower.contains("172.19.")
        || lower.contains("172.20.")
        || lower.contains("172.21.")
        || lower.contains("172.22.")
        || lower.contains("172.23.")
        || lower.contains("172.24.")
        || lower.contains("172.25.")
        || lower.contains("172.26.")
        || lower.contains("172.27.")
        || lower.contains("172.28.")
        || lower.contains("172.29.")
        || lower.contains("172.30.")
        || lower.contains("172.31.");
    
    if lower.contains("openrouter") {
        "OpenRouter".to_string()
    } else if lower.contains("groq") {
        "Groq".to_string()
    } else if lower.contains("openai.com") || lower.contains("api.mistral.ai") {
        "OpenAI".to_string()
    } else if lower.contains("/v1") {
        "OpenAI-compat".to_string()
    } else if lower.contains("localhost:8080") || lower.contains("127.0.0.1:8080") {
        "llama.cpp".to_string()
    } else if is_local {
        // Local network Ollama instance
        let url = endpoint.trim_start_matches("http://").trim_start_matches("https://");
        let host = url.split('/').next().unwrap_or("").split(':').next().unwrap_or("");
        if !host.is_empty() {
            format!("Ollama ({})", host)
        } else {
            "Ollama-local".to_string()
        }
    } else {
        // External network endpoint
        "Ollama-external".to_string()
    }
}

/// Detect API format from endpoint URL.
/// Public so config.rs / ui.rs can use it for display hints.
pub fn detect_api_format(endpoint: &str) -> ApiFormat {
    let lower = endpoint.to_lowercase();
    if lower.contains("openai.com")
        || lower.contains("openrouter.ai")
        || lower.contains("groq.com")
        || lower.contains("together.ai")
        || lower.contains("api.mistral.ai")
        || lower.contains("/v1")
    {
        ApiFormat::OpenAI
    } else if lower.contains("localhost:8080") || lower.contains("127.0.0.1:8080") {
        // Only treat port 8080 as llama.cpp (its default); other ports are Ollama
        ApiFormat::LlamaCpp
    } else {
        ApiFormat::Ollama
    }
}

/// Build an OpenAI-compatible chat completions URL from a base endpoint.
/// Handles both `https://host/v1` and `https://host` forms.
fn openai_chat_url(endpoint: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/v1/chat/completions", base)
    }
}

/// Return the `/v1`-normalised base URL for use with `async_openai::OpenAIConfig::with_api_base`.
/// async-openai appends `/chat/completions` itself, so we only return the base up to `/v1`.
fn openai_api_base(endpoint: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    if base.ends_with("/v1") {
        base.to_string()
    } else {
        format!("{}/v1", base)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApiFormat { Ollama, OpenAI, LlamaCpp }

#[derive(Clone)]
enum Backend {
    Ollama { endpoint: String },
    OpenAI { endpoint: String, api_key: String },
    LlamaCpp { endpoint: String },
}

impl Backend {
    fn endpoint(&self) -> &str {
        match self { Backend::Ollama { endpoint } | Backend::OpenAI { endpoint, .. } | Backend::LlamaCpp { endpoint } => endpoint }
    }
    fn api_key(&self) -> Option<&str> {
        match self { Backend::OpenAI { api_key, .. } => Some(api_key), _ => None }
    }
    fn format(&self) -> ApiFormat {
        match self { Backend::Ollama { .. } => ApiFormat::Ollama, Backend::OpenAI { .. } => ApiFormat::OpenAI, Backend::LlamaCpp { .. } => ApiFormat::LlamaCpp }
    }
}

/// Ollama client for communicating with Ollama or OpenAI-compatible API
#[derive(Clone)]
pub struct OllamaClient {
    backend: Backend,
    http_client: reqwest::Client,
    model: String,
    /// Native context length reported by /api/show for the current model.
    /// None until fetch_native_ctx() is called.
    native_ctx: Arc<Mutex<Option<u32>>>,
    /// Whether the model advertises "thinking" in its capabilities.
    /// None until fetch_native_ctx() is called; defaults to false for non-Ollama backends.
    supports_thinking: Arc<Mutex<Option<bool>>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
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
    /// Pin the first N tokens in the KV cache (Ollama prefix caching).
    /// Set to the estimated token length of the static system prompt so
    /// Ollama reuses the cached prefix across turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_keep: Option<i32>,
    /// Reasoning effort level (e.g. "xhigh"). Forwarded verbatim; ignored by models that don't support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

impl OllamaOptions {
    fn from_params(p: &crate::config::ModelParams, native_ctx: Option<u32>) -> Option<Self> {
        let effective_num_ctx = p.num_ctx.or(native_ctx);
        if p.is_empty() && effective_num_ctx.is_none() {
            return None;
        }
        Some(OllamaOptions {
            temperature: p.temperature,
            top_k: p.top_k,
            top_p: p.top_p,
            repeat_penalty: p.repeat_penalty,
            num_predict: p.num_predict,
            num_ctx: effective_num_ctx,
            num_keep: None,
            reasoning_effort: p.reasoning_effort.clone(),
        })
    }

    fn with_num_keep(mut self, n: i32) -> Self {
        self.num_keep = Some(n);
        self
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

/// Internal deserialization struct for Ollama non-streaming response.
/// Captures the `thinking` field that thinking models (qwen3.5, QwQ, etc.)
/// return separately from `content` when native thinking is enabled.
#[derive(Deserialize)]
struct OllamaRespMsg {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: String,
}
#[derive(Deserialize)]
struct OllamaRespBody {
    message: OllamaRespMsg,
}

/// Merge thinking + content: if content is non-empty return it; otherwise wrap
/// thinking in `<think>` tags so callers (parsers, test gauntlet) can handle it.
fn merge_thinking(content: String, thinking: String) -> String {
    if !content.is_empty() {
        content
    } else if !thinking.is_empty() {
        format!("<think>{}</think>", thinking)
    } else {
        String::new()
    }
}

/// Token event sent from streaming to UI
pub enum StreamEvent {
    Token(String),
    /// A chunk of the model's internal reasoning/thinking (displayed but not sent back)
    ThinkToken(String),
    /// Stream finished with generation stats
    Done {
        prompt_tokens: u32,
        gen_tokens: u32,
        had_thinking: bool,
        /// Time spent generating tokens (nanoseconds), for tok/s computation
        eval_duration_ns: Option<u64>,
        /// Whether the sliding-window context trim fired for this request
        context_trimmed: bool,
        /// Number of messages dropped by the sliding-window trim (0 if none)
        msgs_dropped: usize,
    },
    Error(String),
}

// ---- OpenAI API structs ----

#[derive(Debug, Serialize)]
struct OAIRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<OAIStreamOptions>,
}

#[derive(Debug, Serialize)]
struct OAIStreamOptions {
    include_usage: bool,
}

#[derive(Debug, Deserialize)]
struct OAIStreamChunk {
    choices: Vec<OAIStreamChoice>,
    #[serde(default)]
    usage: Option<OAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OAIStreamChoice {
    delta: OAIStreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OAIStreamDelta {
    #[serde(default)]
    content: String,
    #[serde(default)]
    reasoning_content: String,
}

#[derive(Debug, Deserialize)]
struct OAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OAINonStreamResponse {
    choices: Vec<OAINonStreamChoice>,
    #[serde(default)]
    usage: Option<OAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OAINonStreamChoice {
    message: OAINonStreamMessage,
}

#[derive(Debug, Deserialize)]
struct OAINonStreamMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    reasoning_content: Option<String>,
}

// ---- llama.cpp API structs ----

#[derive(Debug, Serialize)]
struct LlamaCppRequest {
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    n_predict: Option<i32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct LlamaCppStreamChunk {
    #[serde(default)]
    content: String,
    #[serde(default)]
    stop: bool,
}

// ---- Streaming parser helpers ----

/// Accumulated state for an OpenAI SSE stream (carries across chunk boundaries).
#[derive(Default)]
pub(crate) struct OaiStreamState {
    pub had_thinking: bool,
    pub prompt_tokens: u32,
    pub gen_tokens: u32,
}

/// Result of parsing one `data:` line from an OpenAI SSE stream.
pub(crate) enum SseDataResult {
    /// Zero or more intermediate events (Token / ThinkToken).
    Events(Vec<StreamEvent>),
    /// Terminal event — caller must send it and return.
    Done(StreamEvent),
    /// Nothing to emit (empty line, SSE comment, JSON parse error).
    Skip,
}

/// Split `buffer` on newlines.  Returns complete trimmed lines and the
/// still-incomplete remainder (no trailing newline yet).
pub(crate) fn split_newline_buffer(buffer: &str) -> (Vec<String>, String) {
    let mut lines = Vec::new();
    let mut remaining = buffer;
    while let Some(pos) = remaining.find('\n') {
        lines.push(remaining[..pos].trim().to_string());
        remaining = &remaining[pos + 1..];
    }
    (lines, remaining.to_string())
}

/// Parse one NDJSON line from the Ollama `/api/chat` streaming response.
/// Returns zero or more events; empty vec means skip (blank or malformed).
/// `context_trimmed` / `msgs_dropped` are forwarded into the `Done` event.
pub(crate) fn parse_ollama_chunk(
    line: &str,
    had_thinking: &mut bool,
    context_trimmed: bool,
    msgs_dropped: usize,
) -> Vec<StreamEvent> {
    if line.is_empty() {
        return vec![];
    }
    match serde_json::from_str::<StreamChunk>(line) {
        Ok(chunk) => {
            let mut events = Vec::new();
            if let Some(msg) = &chunk.message {
                if !msg.thinking.is_empty() {
                    *had_thinking = true;
                    events.push(StreamEvent::ThinkToken(msg.thinking.clone()));
                }
                if !msg.content.is_empty() {
                    events.push(StreamEvent::Token(msg.content.clone()));
                }
            }
            if chunk.done {
                events.push(StreamEvent::Done {
                    prompt_tokens: chunk.prompt_eval_count.unwrap_or(0),
                    gen_tokens: chunk.eval_count.unwrap_or(0),
                    had_thinking: *had_thinking,
                    eval_duration_ns: chunk.eval_duration,
                    context_trimmed,
                    msgs_dropped,
                });
            }
            events
        }
        Err(_) => vec![],
    }
}

/// Parse one `data:` payload from an OpenAI SSE stream.
/// The caller is responsible for stripping the `data: ` prefix and filtering
/// empty lines / SSE comments before calling this function.
pub(crate) fn parse_openai_sse_data(
    data: &str,
    state: &mut OaiStreamState,
    context_trimmed: bool,
    msgs_dropped: usize,
) -> SseDataResult {
    if data == "[DONE]" {
        return SseDataResult::Done(StreamEvent::Done {
            prompt_tokens: state.prompt_tokens,
            gen_tokens: state.gen_tokens,
            had_thinking: state.had_thinking,
            eval_duration_ns: None,
            context_trimmed,
            msgs_dropped,
        });
    }
    match serde_json::from_str::<OAIStreamChunk>(data) {
        Ok(chunk) => {
            if let Some(usage) = &chunk.usage {
                state.prompt_tokens = usage.prompt_tokens;
                state.gen_tokens = usage.completion_tokens;
            }
            let mut events = Vec::new();
            for choice in &chunk.choices {
                if !choice.delta.reasoning_content.is_empty() {
                    state.had_thinking = true;
                    events.push(StreamEvent::ThinkToken(choice.delta.reasoning_content.clone()));
                }
                if !choice.delta.content.is_empty() {
                    events.push(StreamEvent::Token(choice.delta.content.clone()));
                }
            }
            SseDataResult::Events(events)
        }
        Err(_) => SseDataResult::Skip,
    }
}

/// Process one already-deserialized OpenAI stream chunk (from `async_openai` byot stream).
/// Updates `state` with usage and thinking flag; returns Token/ThinkToken events.
/// The caller is responsible for emitting `StreamEvent::Done` when the stream ends.
pub(crate) fn process_oai_chunk(chunk: &OAIStreamChunk, state: &mut OaiStreamState) -> Vec<StreamEvent> {
    if let Some(usage) = &chunk.usage {
        state.prompt_tokens = usage.prompt_tokens;
        state.gen_tokens = usage.completion_tokens;
    }
    let mut events = Vec::new();
    for choice in &chunk.choices {
        if !choice.delta.reasoning_content.is_empty() {
            state.had_thinking = true;
            events.push(StreamEvent::ThinkToken(choice.delta.reasoning_content.clone()));
        }
        if !choice.delta.content.is_empty() {
            events.push(StreamEvent::Token(choice.delta.content.clone()));
        }
    }
    events
}

/// Parse one NDJSON line from the llama.cpp `/completion` streaming response.
/// Returns zero or more events; empty vec means skip (blank or malformed).
pub(crate) fn parse_llamacpp_chunk(
    line: &str,
    context_trimmed: bool,
    msgs_dropped: usize,
) -> Vec<StreamEvent> {
    if line.is_empty() {
        return vec![];
    }
    match serde_json::from_str::<LlamaCppStreamChunk>(line) {
        Ok(chunk) => {
            let mut events = Vec::new();
            if !chunk.content.is_empty() {
                events.push(StreamEvent::Token(chunk.content.clone()));
            }
            if chunk.stop {
                events.push(StreamEvent::Done {
                    prompt_tokens: 0,
                    gen_tokens: 0,
                    had_thinking: false,
                    eval_duration_ns: None,
                    context_trimmed,
                    msgs_dropped,
                });
            }
            events
        }
        Err(_) => vec![],
    }
}

fn build_openai_request(model: &str, messages: Vec<OllamaMessage>, params: &crate::config::ModelParams) -> OAIRequest {
    OAIRequest {
        model: model.to_string(),
        messages,
        stream: true,
        temperature: params.temperature,
        max_tokens: params.num_predict,
        top_p: params.top_p,
        stream_options: Some(OAIStreamOptions { include_usage: true }),
    }
}

async fn stream_openai(
    endpoint: String,
    api_key: String,
    mut request: OAIRequest,
    tx: mpsc::UnboundedSender<StreamEvent>,
    context_trimmed: bool,
    msgs_dropped: usize,
) {
    use async_openai::{Client as OAIClient, config::OpenAIConfig};
    use futures_util::StreamExt;

    request.stream = true;
    let api_base = openai_api_base(&endpoint);
    crate::dlog!("stream_openai: POST {}/chat/completions model={} msgs={} api_key_set={}",
        api_base, request.model, request.messages.len(), !api_key.is_empty());

    let config = OpenAIConfig::new()
        .with_api_base(&api_base)
        .with_api_key(&api_key);
    let oai_client = OAIClient::with_config(config);

    let mut stream = match oai_client.chat().create_stream_byot::<OAIRequest, OAIStreamChunk>(request).await {
        Ok(s) => s,
        Err(e) => {
            crate::dlog!("stream_openai: request failed: {}", e);
            let _ = tx.send(StreamEvent::Error(format!("Request failed: {}", e)));
            return;
        }
    };

    let mut state = OaiStreamState::default();
    let mut chunks_seen = 0u32;
    let mut content_chunks = 0u32;

    while let Some(result) = stream.next().await {
        match result {
            Ok(chunk) => {
                chunks_seen += 1;
                let events = process_oai_chunk(&chunk, &mut state);
                for event in events {
                    if matches!(event, StreamEvent::Token(_)) { content_chunks += 1; }
                    if tx.send(event).is_err() { return; }
                }
            }
            Err(e) => {
                crate::dlog!("stream_openai: stream error: {}", e);
                let _ = tx.send(StreamEvent::Error(format!("Stream error: {}", e)));
                return;
            }
        }
    }
    crate::dlog!("stream_openai: stream ended chunks={} content_chunks={} prompt_tokens={} gen_tokens={} had_thinking={}",
        chunks_seen, content_chunks, state.prompt_tokens, state.gen_tokens, state.had_thinking);

    let _ = tx.send(StreamEvent::Done {
        prompt_tokens: state.prompt_tokens,
        gen_tokens: state.gen_tokens,
        had_thinking: state.had_thinking,
        eval_duration_ns: None,
        context_trimmed,
        msgs_dropped,
    });
}

async fn stream_llamacpp(
    client: reqwest::Client,
    url: String,
    request: LlamaCppRequest,
    tx: mpsc::UnboundedSender<StreamEvent>,
    context_trimmed: bool,
    msgs_dropped: usize,
) {
    crate::dlog!("stream_llamacpp: POST {} stream=true", url);
    let response = match client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            crate::dlog!("stream_llamacpp: request failed: {}", e);
            let _ = tx.send(StreamEvent::Error(format!("Request failed: {}", e)));
            return;
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        crate::dlog!("stream_llamacpp: HTTP {}: {}", status, body);
        let _ = tx.send(StreamEvent::Error(format!("llama.cpp error {}: {}", status, body)));
        return;
    }
    crate::dlog!("stream_llamacpp: HTTP OK, reading stream");

    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut chunks_seen = 0u32;
    let mut content_chunks = 0u32;

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(bytes) => {
                buffer.push_str(&String::from_utf8_lossy(&bytes));
                let (lines, remainder) = split_newline_buffer(&buffer);
                buffer = remainder;

                for line in lines {
                    if line.is_empty() { continue; }
                    chunks_seen += 1;
                    let events = parse_llamacpp_chunk(&line, context_trimmed, msgs_dropped);
                    for event in events {
                        let is_done = matches!(event, StreamEvent::Done { .. });
                        if is_done {
                            crate::dlog!("stream_llamacpp: stop received chunks={} content_chunks={}", chunks_seen, content_chunks);
                            let _ = tx.send(event);
                            return;
                        } else {
                            if matches!(event, StreamEvent::Token(_)) { content_chunks += 1; }
                            if tx.send(event).is_err() { return; }
                        }
                    }
                }
            }
            Err(e) => {
                crate::dlog!("stream_llamacpp: stream error: {}", e);
                let _ = tx.send(StreamEvent::Error(format!("Stream error: {}", e)));
                return;
            }
        }
    }
    crate::dlog!("stream_llamacpp: stream ended without stop chunks={} content_chunks={}", chunks_seen, content_chunks);

    let _ = tx.send(StreamEvent::Done {
        prompt_tokens: 0,
        gen_tokens: 0,
        had_thinking: false,
        eval_duration_ns: None,
        context_trimmed,
        msgs_dropped,
    });
}

impl OllamaClient {
    /// Create a new client and validate connection
    pub async fn new(endpoint: &str, model: &str) -> Result<Self> {
        Self::new_with_key(endpoint, model, None).await
    }

    /// Create a new client with an optional API key (required for OpenAI-compatible endpoints).
    pub async fn new_with_key(endpoint: &str, model: &str, api_key: Option<&str>) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))  // 2-minute timeout for entire request/response
            // TCP keepalive: 1 hour to maintain connection across idle periods
            .tcp_keepalive(Duration::from_secs(3600))
            // Connection pool idle timeout: 1 hour
            .pool_idle_timeout(Duration::from_secs(3600))
            .build()?;

        let backend = match detect_api_format(endpoint) {
            ApiFormat::OpenAI => Backend::OpenAI {
                endpoint: endpoint.to_string(),
                api_key: api_key.unwrap_or("").to_string(),
            },
            ApiFormat::Ollama => Backend::Ollama {
                endpoint: endpoint.to_string(),
            },
            ApiFormat::LlamaCpp => Backend::LlamaCpp {
                endpoint: endpoint.to_string(),
            },
        };

        let ollama_client = Self {
            backend,
            http_client,
            model: model.to_string(),
            native_ctx: Arc::new(Mutex::new(None)),
            supports_thinking: Arc::new(Mutex::new(None)),
        };

        match ollama_client.list_models().await {
            Ok(_) => {
                crate::dlog!("✅ Connection validated: {}", endpoint);
                Ok(ollama_client)
            }
            Err(e) => {
                let friendly_msg = if e.to_string().contains("connection refused") {
                    format!("Service is not running at {}", endpoint)
                } else if e.to_string().contains("timeout") {
                    format!("Service at {} is not responding", endpoint)
                } else {
                    e.to_string()
                };
                crate::dlog!("❌ Connection failed: {}", friendly_msg);
                Err(e)
            }
        }
    }

    /// Create a client immediately without a connectivity probe (no HTTP round-trip).
    /// The caller should spawn a background task to validate connectivity if desired.
    pub fn new_unchecked(endpoint: &str, model: &str, api_key: Option<&str>) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))  // 2-minute timeout for entire request/response
            .tcp_keepalive(Duration::from_secs(3600))
            .pool_idle_timeout(Duration::from_secs(3600))
            .build()?;

        let backend = match detect_api_format(endpoint) {
            ApiFormat::OpenAI => Backend::OpenAI {
                endpoint: endpoint.to_string(),
                api_key: api_key.unwrap_or("").to_string(),
            },
            ApiFormat::Ollama => Backend::Ollama {
                endpoint: endpoint.to_string(),
            },
            ApiFormat::LlamaCpp => Backend::LlamaCpp {
                endpoint: endpoint.to_string(),
            },
        };

        Ok(Self {
            backend,
            http_client,
            model: model.to_string(),
            native_ctx: Arc::new(Mutex::new(None)),
            supports_thinking: Arc::new(Mutex::new(None)),
        })
    }

    pub fn endpoint(&self) -> &str { self.backend.endpoint() }
    pub fn api_format(&self) -> ApiFormat { self.backend.format() }
    pub fn model(&self) -> &str { &self.model }

    /// Reuse an existing validated client but switch to a different model name.
    /// No network round-trip — the underlying reqwest::Client is Arc-backed and cheap to clone.
    /// native_ctx and supports_thinking are reset to None since the model changed (call fetch_native_ctx to re-detect).
    pub fn new_with_existing(existing: Self, model: &str) -> Self {
        Self {
            model: model.to_string(),
            native_ctx: Arc::new(Mutex::new(None)),
            supports_thinking: Arc::new(Mutex::new(None)),
            ..existing
        }
    }

    /// Fetch list of available models
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        match &self.backend {
            Backend::Ollama { endpoint } => {
                let url = format!("{}/api/tags", endpoint);
                let response = tokio::time::timeout(
                    Duration::from_secs(15),
                    self.http_client.get(&url).send()
                ).await
                    .map_err(|_| anyhow!("Timed out connecting to Ollama at {}", endpoint))?
                    .map_err(|e| anyhow!("Failed to connect to Ollama at {}: {}", endpoint, e))?;
                if !response.status().is_success() {
                    return Err(anyhow!("Ollama returned error: {}", response.status()));
                }
                let data: ModelsResponse = tokio::time::timeout(
                    Duration::from_secs(10),
                    response.json()
                ).await
                    .map_err(|_| anyhow!("Timed out reading models response"))?
                    .map_err(|e| anyhow!("Failed to parse models response: {}", e))?;
                Ok(data.models)
            }
            Backend::OpenAI { endpoint, api_key } => {
                let base = endpoint.trim_end_matches('/');
                let url = if base.ends_with("/v1") {
                    format!("{}/models", base)
                } else {
                    format!("{}/v1/models", base)
                };
                let response = tokio::time::timeout(
                    Duration::from_secs(15),
                    self.http_client.get(&url)
                        .header("Authorization", format!("Bearer {}", api_key))
                        .header("Content-Type", "application/json")
                        .send()
                ).await
                    .map_err(|_| anyhow!("Timed out connecting to {}", endpoint))?
                    .map_err(|e| anyhow!("Failed to connect to {} : {}", endpoint, e))?;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = tokio::time::timeout(Duration::from_secs(5), response.text())
                        .await.ok().and_then(|r| r.ok()).unwrap_or_default();
                    return Err(anyhow!("OpenAI endpoint returned {}: {}", status, body));
                }
                #[derive(Deserialize)] struct OAIModelsResp { data: Vec<OAIModelEntry> }
                #[derive(Deserialize)] struct OAIModelEntry { id: String }
                let data: OAIModelsResp = tokio::time::timeout(
                    Duration::from_secs(10),
                    response.json()
                ).await
                    .map_err(|_| anyhow!("Timed out reading models response"))?
                    .map_err(|e| anyhow!("Failed to parse models response: {}", e))?;
                Ok(data.data.into_iter().map(|m| ModelInfo { name: m.id, modified_at: None, size: None }).collect())
            }
            Backend::LlamaCpp { endpoint } => {
                // Try the OpenAI-compatible /v1/models endpoint; fall back to placeholder
                let base = endpoint.trim_end_matches('/');
                let url = format!("{}/v1/models", base);
                match tokio::time::timeout(Duration::from_secs(15), self.http_client.get(&url).send()).await {
                    Ok(Ok(response)) if response.status().is_success() => {
                        #[derive(Deserialize)] struct LCModelsResp { data: Vec<LCModelEntry> }
                        #[derive(Deserialize)] struct LCModelEntry { id: String }
                        match tokio::time::timeout(Duration::from_secs(10), response.json::<LCModelsResp>()).await {
                            Ok(Ok(data)) => Ok(data.data.into_iter()
                                .map(|m| ModelInfo { name: m.id, modified_at: None, size: None })
                                .collect()),
                            _ => Ok(vec![ModelInfo { name: self.model.clone(), modified_at: None, size: None }]),
                        }
                    }
                    _ => Ok(vec![ModelInfo { name: self.model.clone(), modified_at: None, size: None }]),
                }
            }
        }
    }

    /// Fetch the native context length and thinking capability for the current model from Ollama's /api/show endpoint.
    /// Looks for any key ending in `.context_length` in `model_info` (e.g. `qwen35.context_length`).
    /// Checks `capabilities` array for `"thinking"` to set supports_thinking.
    /// Stores results internally so getters can return them without another network call.
    /// Returns None immediately for OpenAI-compatible backends.
    pub async fn fetch_native_ctx(&self) -> Option<u32> {
        let (endpoint, client) = match &self.backend {
            Backend::Ollama { endpoint } => (endpoint.as_str(), &self.http_client),
            Backend::OpenAI { .. } => return None,
            Backend::LlamaCpp { .. } => return None,
        };
        #[derive(Deserialize)]
        struct ShowResponse {
            model_info: Option<std::collections::HashMap<String, serde_json::Value>>,
            #[serde(default)]
            capabilities: Vec<String>,
        }
        let url = format!("{}/api/show", endpoint);
        let body = serde_json::json!({"model": self.model});
        let resp = tokio::time::timeout(
            Duration::from_secs(15),
            client.post(&url).json(&body).send()
        ).await.ok()?.ok()?;
        let show: ShowResponse = tokio::time::timeout(
            Duration::from_secs(10),
            resp.json()
        ).await.ok()?.ok()?;

        let thinks = show.capabilities.iter().any(|c| c == "thinking");
        if let Ok(mut guard) = self.supports_thinking.lock() {
            *guard = Some(thinks);
        }
        dlog!("🧠 {} supports_thinking={}", self.model, thinks);

        let info = show.model_info?;
        let ctx = info.iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
            .map(|v| v as u32)?;
        if let Ok(mut guard) = self.native_ctx.lock() {
            *guard = Some(ctx);
        }
        dlog!("🔍 native context for {}: {}", self.model, ctx);
        Some(ctx)
    }

    /// Return the previously fetched native context length (None if fetch_native_ctx not yet called).
    pub fn get_native_ctx(&self) -> Option<u32> {
        self.native_ctx.lock().ok().and_then(|g| *g)
    }

    /// Whether the model supports native thinking (detected via /api/show capabilities).
    pub fn supports_thinking(&self) -> bool {
        self.supports_thinking.lock().ok().and_then(|g| *g).unwrap_or(false)
    }

    /// Resolve the `think` parameter: if user explicitly set it, use that;
    /// otherwise auto-enable for models that advertise thinking capability.
    pub fn resolve_think(&self, params_think: Option<bool>) -> Option<bool> {
        match params_think {
            Some(v) => Some(v),
            None => if self.supports_thinking() { Some(true) } else { None },
        }
    }

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
    ///
    /// Returns `(messages, steering_token_estimate)`.
    fn build_messages(
        messages: &[Message],
        steering: Option<&str>,
        tool_output_cap: Option<usize>,
        context_window: Option<u32>,
    ) -> (Vec<OllamaMessage>, usize, bool, usize) {
        let mut ollama_messages = Vec::new();

        // --- System prompt (pinned prefix for KV-cache) ---
        let steering_chars = steering.map(|s| s.len()).unwrap_or(0);
        if let Some(steer) = steering {
            ollama_messages.push(OllamaMessage {
                role: "system".to_string(),
                content: steer.to_string(),
            });
        }

        let cap = tool_output_cap.unwrap_or(crate::config::OUTPUT_CHARACTER_LIMIT);

        // How many recent assistant turns to keep verbatim when rolling trim fires.
        const KEEP_RECENT_ASSISTANT: usize = 10;

        // --- First pass: collect all messages with role mapping + cap ---
        let mut raw: Vec<OllamaMessage> = Vec::new();
        for msg in messages {
            // Skip UI-only roles — not forwarded to the model.
            if msg.role == "system" || msg.role == "clock" { continue; }
            // Strip `think` scratchpad results — the model doesn't need to re-read its own reasoning.
            if msg.role == "tool" && msg.content.contains("[TOOL_OUTPUT: think =") { continue; }

            // "notice" is a system-level notice from the app (context warnings, gap reflections, etc.)
            let role = if msg.role == "notice" {
                "system"
            } else if msg.role == "tool" || msg.role == "kick" {
                "user"
            } else {
                &msg.role
            };

            // Truncate tool output messages that exceed the cap
            let content = if msg.content.contains("[TOOL_OUTPUT:") && msg.content.len() > cap {
                let safe_cap = floor_char_boundary(&msg.content, cap);
                let truncated = &msg.content[..safe_cap];
                let cut = truncated.rfind('\n').unwrap_or(safe_cap);
                let dropped = msg.content.len() - cut;
                format!("{}\n…({} omitted)", &msg.content[..cut], dropped)
            } else {
                msg.content.clone()
            };

            raw.push(OllamaMessage {
                role: role.to_string(),
                content,
            });
        }

        // --- Tool output deduplication ---
        // Walk newest→oldest. For each message, scan ALL [TOOL_OUTPUT:] blocks (a batch
        // result may have several). Record first-seen keys; if ALL keys in a message were
        // already seen from a newer message, collapse the whole message to a stub.
        // Partial-superseded batch messages are left intact so the LLM retains context.
        {
            use std::collections::HashSet;

            // Extract all dedup keys from a message, in order.
            let extract_keys = |content: &str| -> Vec<String> {
                let mut keys = Vec::new();
                let mut offset = 0usize;
                while let Some(rel) = content[offset..].find("[TOOL_OUTPUT:") {
                    let start = offset + rel;
                    let fragment = &content[start..];
                    let key: String = fragment
                        .splitn(2, '=')
                        .next()
                        .unwrap_or(fragment)
                        .trim()
                        .to_string();
                    keys.push(key);
                    offset = start + "[TOOL_OUTPUT:".len();
                }
                keys
            };

            let mut seen_keys: HashSet<String> = HashSet::new();
            for msg in raw.iter_mut().rev() {
                if !msg.content.contains("[TOOL_OUTPUT:") { continue; }
                let keys = extract_keys(&msg.content);
                // Fully superseded: every key was already seen from a more-recent message.
                let all_superseded = !keys.is_empty() && keys.iter().all(|k| seen_keys.contains(k));
                if all_superseded {
                    let stub_key = keys.first().cloned().unwrap_or_default();
                    msg.content = format!("{} (superseded by later call)]", stub_key);
                } else {
                    for key in keys {
                        seen_keys.insert(key);
                    }
                }
            }
        }

        // --- Rolling assistant trim ---
        // Only fire when context is under pressure (> ~48% of window used).
        // When the model has plenty of room, preserving the full conversation
        // history prevents repetition — the model remembers what it already did.
        // When pressure is real, trim older assistant turns to make room.
        let rolling_trim_budget = context_window
            .map(|w| (w as usize * 4 * 48) / 100); // 48% of window in chars
        let raw_total_chars: usize = raw.iter().map(|m| m.content.len()).sum();
        let under_pressure = rolling_trim_budget
            .map(|budget| raw_total_chars > budget)
            .unwrap_or(false);

        if under_pressure {
            let total_assistant = raw.iter().filter(|m| m.role == "assistant").count();
            let trim_threshold = total_assistant.saturating_sub(KEEP_RECENT_ASSISTANT);

            let mut seen_assistant = 0usize;
            for msg in raw.iter_mut() {
                if msg.role != "assistant" { continue; }
                if seen_assistant < trim_threshold {
                    let trimmed = msg.content.trim();
                    // Tool-call messages (pure JSON or narration+JSON) → drop entirely.
                    // We match both pure-JSON starts AND any message containing tool_calls,
                    // to handle "Running: `cmd`.\n{\"tool_calls\":[...]}" combos that
                    // would otherwise leak JSON formatting into the model's context window
                    // and cause it to echo "[assistant: {" in its next response.
                    if trimmed.starts_with('{') || trimmed.starts_with("[{")
                        || msg.content.contains("\"tool_calls\"")
                    {
                        msg.content = String::new();
                    } else {
                        // Prose-only response → keep a short preview.
                        // Use [...prev] format (not [assistant: ...]) to avoid training
                        // the model to output that prefix in future responses.
                        let preview: String = msg.content.chars().take(120).collect();
                        let had_more = msg.content.chars().count() > 120;
                        msg.content = if had_more {
                            format!("[…prev: {}…]", preview)
                        } else {
                            format!("[…prev: {}]", preview)
                        };
                    }
                }
                seen_assistant += 1;
            }
            // Remove messages blanked out above
            raw.retain(|m| !m.content.is_empty());
            if trim_threshold > 0 {
                dlog!("build_messages: trimmed {} old assistant turns (kept {} verbatim)",
                    trim_threshold, total_assistant - trim_threshold);
            }
        }

        ollama_messages.extend(raw);

        // Sliding window: if we have a context window budget, drop oldest turns
        // until the estimated token count fits in 80% of the window.
        let mut context_trimmed = false;
        let mut msgs_dropped: usize = 0;
        if let Some(window) = context_window {
            let budget_chars = (window as usize * 4 * 8) / 10; // 80% of window, chars = tokens*4
            let total_chars: usize = ollama_messages.iter().map(|m| m.content.len()).sum();
            if total_chars > budget_chars {
                let before = ollama_messages.len();
                // Always keep index 0 (system prompt if present, else first user message).
                let first_user_idx = ollama_messages.iter().position(|m| m.role == "user");
                let keep_until = first_user_idx.map(|i| i + 1).unwrap_or(1);

                while ollama_messages.len() > keep_until + 1 {
                    let chars: usize = ollama_messages.iter().map(|m| m.content.len()).sum();
                    if chars <= budget_chars { break; }
                    ollama_messages.remove(keep_until);
                }
                let after = ollama_messages.len();
                if after < before {
                    context_trimmed = true;
                    msgs_dropped = before - after;
                }
                dlog!("build_messages: sliding-window dropped {} msgs — total_chars={} budget={} window={}",
                    before - after, total_chars, budget_chars, window);
            }
        }

        let total_est_tokens: usize = ollama_messages.iter().map(|m| m.content.len() / 4).sum();
        dlog!("build_messages: out={} msgs est_tokens={}", ollama_messages.len(), total_est_tokens);

        (ollama_messages, steering_chars, context_trimmed, msgs_dropped)
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
        let model = self.model.clone();
        let (tx, rx) = mpsc::unbounded_channel();

        let (ollama_messages, steering_chars, context_trimmed, msgs_dropped) = Self::build_messages(&messages, steering, tool_output_cap, context_window);
        dlog!("generate_streaming: model={} num_ctx={:?} in_msgs={} out_msgs={} est_tokens={} ctx_trimmed={}",
            model,
            params.num_ctx,
            messages.len(),
            ollama_messages.len(),
            ollama_messages.iter().map(|m| m.content.len() / 4).sum::<usize>(),
            context_trimmed);

        // Apply prefix caching: pin the steering prefix in Ollama's KV cache.
        let options = if steering_chars > 0 {
            let steering_tokens = (steering_chars / 4).max(1) as i32;
            let base = OllamaOptions::from_params(&params, self.get_native_ctx()).unwrap_or_default();
            Some(base.with_num_keep(steering_tokens))
        } else {
            OllamaOptions::from_params(&params, self.get_native_ctx())
        };

        match &self.backend {
            Backend::Ollama { endpoint } => {
                let request = GenerateRequest {
                    model,
                    messages: ollama_messages,
                    stream: true,
                    options,
                    think: self.resolve_think(params.think),
                };
                let url = format!("{}/api/chat", endpoint);
                let client = self.http_client.clone();
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
                    use futures_util::StreamExt;
                    let mut stream = response.bytes_stream();
                    let mut buffer = String::new();
                    let mut had_thinking = false;
                    while let Some(chunk_result) = stream.next().await {
                        match chunk_result {
                            Ok(bytes) => {
                                buffer.push_str(&String::from_utf8_lossy(&bytes));
                                let (lines, remainder) = split_newline_buffer(&buffer);
                                buffer = remainder;
                                for line in lines {
                                    let events = parse_ollama_chunk(&line, &mut had_thinking, context_trimmed, msgs_dropped);
                                    for event in events {
                                        let is_done = matches!(event, StreamEvent::Done { .. });
                                        if is_done {
                                            if let StreamEvent::Done { prompt_tokens, gen_tokens, .. } = &event {
                                                dlog!("generate_streaming: stream DONE prompt_tokens={prompt_tokens} gen_tokens={gen_tokens} had_thinking={had_thinking}");
                                            }
                                            let _ = tx.send(event);
                                            return;
                                        } else if tx.send(event).is_err() {
                                            return;
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
                    let _ = tx.send(StreamEvent::Done {
                        prompt_tokens: 0,
                        gen_tokens: 0,
                        had_thinking,
                        eval_duration_ns: None,
                        context_trimmed,
                        msgs_dropped,
                    });
                });
            }
            Backend::OpenAI { endpoint, api_key } => {
                let request = build_openai_request(&model, ollama_messages, &params);
                let endpoint = endpoint.clone();
                let api_key = api_key.clone();
                tokio::spawn(async move {
                    stream_openai(endpoint, api_key, request, tx, context_trimmed, msgs_dropped).await;
                });
            }
            Backend::LlamaCpp { endpoint } => {
                let prompt = ollama_messages
                    .iter()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                let request = LlamaCppRequest {
                    prompt,
                    n_predict: params.num_predict,
                    stream: true,
                    temperature: params.temperature,
                    top_p: params.top_p,
                };
                let url = format!("{}/completion", endpoint.trim_end_matches('/'));
                let client = self.http_client.clone();
                tokio::spawn(async move {
                    stream_llamacpp(client, url, request, tx, context_trimmed, msgs_dropped).await;
                });
            }
        }

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

        match &self.backend {
            Backend::Ollama { endpoint } => {
                let request = GenerateRequest {
                    model: model.to_string(),
                    messages,
                    stream: true,
                    options: OllamaOptions::from_params(params, self.get_native_ctx()),
                    think: self.resolve_think(params.think),
                };
                let url = format!("{}/api/chat", endpoint);
                let client = self.http_client.clone();
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
                                let (lines, remainder) = split_newline_buffer(&buffer);
                                buffer = remainder;
                                for line in lines {
                                    let events = parse_ollama_chunk(&line, &mut had_thinking, false, 0);
                                    for event in events {
                                        let is_done = matches!(event, StreamEvent::Done { .. });
                                        if is_done {
                                            let _ = tx.send(event);
                                            return;
                                        } else if tx.send(event).is_err() {
                                            return;
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
                    let _ = tx.send(StreamEvent::Done {
                        prompt_tokens: 0,
                        gen_tokens: 0,
                        had_thinking,
                        eval_duration_ns: None,
                        context_trimmed: false,
                        msgs_dropped: 0,
                    });
                });
            }
            Backend::OpenAI { endpoint, api_key } => {
                let request = build_openai_request(model, messages, params);
                let endpoint = endpoint.clone();
                let api_key = api_key.clone();
                tokio::spawn(async move {
                    stream_openai(endpoint, api_key, request, tx, false, 0).await;
                });
            }
            Backend::LlamaCpp { endpoint } => {
                let prompt = messages
                    .iter()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                let request = LlamaCppRequest {
                    prompt,
                    n_predict: params.num_predict,
                    stream: true,
                    temperature: params.temperature,
                    top_p: params.top_p,
                };
                let url = format!("{}/completion", endpoint.trim_end_matches('/'));
                let client = self.http_client.clone();
                tokio::spawn(async move {
                    stream_llamacpp(client, url, request, tx, false, 0).await;
                });
            }
        }

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
        let (ollama_messages, steering_chars, _context_trimmed, _msgs_dropped) = Self::build_messages(&messages, steering, tool_output_cap, context_window);

        match &self.backend {
            Backend::Ollama { endpoint } => {
                let options = if steering_chars > 0 {
                    let steering_tokens = (steering_chars / 4).max(1) as i32;
                    let base = OllamaOptions::from_params(params, self.get_native_ctx()).unwrap_or_default();
                    Some(base.with_num_keep(steering_tokens))
                } else {
                    OllamaOptions::from_params(params, self.get_native_ctx())
                };
                let request = GenerateRequest {
                    model: self.model.clone(),
                    messages: ollama_messages,
                    stream: false,
                    options,
                    think: self.resolve_think(params.think),
                };
                let url = format!("{}/api/chat", endpoint);
                let response = self.http_client.post(&url).json(&request).send().await
                    .map_err(|e| anyhow!("Failed to send request to Ollama at {}: {}", endpoint, e))?;
                if !response.status().is_success() {
                    return Err(anyhow!(
                        "Ollama returned error: {} - {}",
                        response.status(),
                        response.text().await.unwrap_or_default()
                    ));
                }
                let data: OllamaRespBody = response.json().await
                    .map_err(|e| anyhow!("Failed to parse generate response: {}", e))?;
                Ok(merge_thinking(data.message.content, data.message.thinking))
            }
            Backend::OpenAI { endpoint, api_key } => {
                use async_openai::{Client as OAIClient, config::OpenAIConfig};
                let api_base = openai_api_base(endpoint);
                let config = OpenAIConfig::new()
                    .with_api_base(&api_base)
                    .with_api_key(api_key.as_str());
                let oai_client = OAIClient::with_config(config);
                let mut request = build_openai_request(&self.model, ollama_messages, params);
                request.stream = false;
                request.stream_options = None;
                let data: OAINonStreamResponse = oai_client.chat()
                    .create_byot::<OAIRequest, OAINonStreamResponse>(request)
                    .await
                    .map_err(|e| anyhow!("OpenAI request failed: {}", e))?;
                let choice = data.choices.into_iter().next()
                    .ok_or_else(|| anyhow!("OpenAI returned no choices"))?;
                let thinking = choice.message.reasoning_content.unwrap_or_default();
                Ok(merge_thinking(choice.message.content, thinking))
            }
            Backend::LlamaCpp { endpoint } => {
                let prompt = ollama_messages
                    .iter()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                let request = LlamaCppRequest {
                    prompt,
                    n_predict: params.num_predict,
                    stream: false,
                    temperature: params.temperature,
                    top_p: params.top_p,
                };
                let url = format!("{}/completion", endpoint.trim_end_matches('/'));
                let response = self.http_client.post(&url)
                    .header("Content-Type", "application/json")
                    .json(&request)
                    .send().await
                    .map_err(|e| anyhow!("Failed to send request to llama.cpp at {}: {}", endpoint, e))?;
                if !response.status().is_success() {
                    return Err(anyhow!("llama.cpp returned error: {}", response.status()));
                }
                #[derive(Deserialize)]
                struct LlamaCppResp {
                    #[serde(default)]
                    content: String,
                }
                let data: LlamaCppResp = response.json().await
                    .map_err(|e| anyhow!("Failed to parse llama.cpp response: {}", e))?;
                Ok(data.content)
            }
        }
    }

    /// Send messages directly (raw OllamaMessage format)
    pub async fn generate_with_messages(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        params: &crate::config::ModelParams,
    ) -> Result<GenerateResponse> {
        match &self.backend {
            Backend::Ollama { endpoint } => {
                let request = GenerateRequest {
                    model: model.to_string(),
                    messages,
                    stream: false,
                    options: OllamaOptions::from_params(params, self.get_native_ctx()),
                    think: self.resolve_think(params.think),
                };
                let url = format!("{}/api/chat", endpoint);
                let response = self.http_client.post(&url).json(&request).send().await
                    .map_err(|e| anyhow!("Failed to send request to Ollama at {}: {}", endpoint, e))?;
                if !response.status().is_success() {
                    return Err(anyhow!(
                        "Ollama returned error: {} - {}",
                        response.status(),
                        response.text().await.unwrap_or_default()
                    ));
                }
                let data: OllamaRespBody = response.json().await
                    .map_err(|e| anyhow!("Failed to parse generate response: {}", e))?;
                let content = merge_thinking(data.message.content, data.message.thinking);
                Ok(GenerateResponse {
                    message: OllamaMessage { role: "assistant".to_string(), content },
                })
            }
            Backend::OpenAI { endpoint, api_key } => {
                let mut request = build_openai_request(model, messages, params);
                request.stream = false;
                request.stream_options = None;
                let url = openai_chat_url(endpoint);
                let response = self.http_client.post(&url)
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json")
                    .json(&request)
                    .send().await
                    .map_err(|e| anyhow!("Failed to send request to OpenAI at {}: {}", endpoint, e))?;
                if !response.status().is_success() {
                    return Err(anyhow!(
                        "OpenAI returned error: {} - {}",
                        response.status(),
                        response.text().await.unwrap_or_default()
                    ));
                }
                let data: OAINonStreamResponse = response.json().await
                    .map_err(|e| anyhow!("Failed to parse OpenAI response: {}", e))?;
                let choice = data.choices.into_iter().next()
                    .ok_or_else(|| anyhow!("OpenAI returned no choices"))?;
                let content = choice.message.content;
                let thinking = choice.message.reasoning_content.unwrap_or_default();
                Ok(GenerateResponse {
                    message: OllamaMessage {
                        role: "assistant".to_string(),
                        content: if thinking.is_empty() {
                            content
                        } else {
                            format!("<think>{}</think>{}", thinking, content)
                        },
                    },
                })
            }
            Backend::LlamaCpp { endpoint } => {
                let prompt = messages
                    .iter()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                let request = LlamaCppRequest {
                    prompt,
                    n_predict: params.num_predict,
                    stream: false,
                    temperature: params.temperature,
                    top_p: params.top_p,
                };
                let url = format!("{}/completion", endpoint.trim_end_matches('/'));
                let response = self.http_client.post(&url)
                    .header("Content-Type", "application/json")
                    .json(&request)
                    .send().await
                    .map_err(|e| anyhow!("Failed to send request to llama.cpp at {}: {}", endpoint, e))?;
                if !response.status().is_success() {
                    return Err(anyhow!(
                        "llama.cpp returned error: {} - {}",
                        response.status(),
                        response.text().await.unwrap_or_default()
                    ));
                }
                #[derive(Deserialize)]
                struct LlamaCppResp {
                    #[serde(default)]
                    content: String,
                }
                let data: LlamaCppResp = response.json().await
                    .map_err(|e| anyhow!("Failed to parse llama.cpp response: {}", e))?;
                Ok(GenerateResponse {
                    message: OllamaMessage {
                        role: "assistant".to_string(),
                        content: data.content,
                    },
                })
            }
        }
    }
}

/// Return the largest index ≤ `max` that is a valid UTF-8 char boundary in `s`.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() { return s.len(); }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
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
            think: None,
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
        let opts = OllamaOptions::from_params(&params, None).unwrap();
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
        let (result, steering_chars, _, _) = OllamaClient::build_messages(&msgs, Some("[STEERING: be nice]"), None, None);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "system");
        assert!(result[0].content.contains("be nice"));
        assert_eq!(result[1].role, "user");
        assert!(steering_chars > 0);
    }

    #[test]
    fn test_build_messages_no_steering() {
        let msgs = vec![Message::new("user", "hi")];
        let (result, steering_chars, _, _) = OllamaClient::build_messages(&msgs, None, None, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, "user");
        assert_eq!(steering_chars, 0);
    }

    #[test]
    fn test_build_messages_maps_tool_to_user() {
        let msgs = vec![
            Message::new("user", "search for main"),
            Message::new("assistant", r#"{"tool_calls": [{"name": "rg", "parameters": {"pattern": "main", "directory": "."}}]}"#),
            Message::new("tool", "[TOOL_OUTPUT: rg = found matches]"),
        ];
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, None);
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
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, Some(3000), None);
        // The tool output message should be truncated
        let tool_msg = &result[2];
        assert!(tool_msg.content.len() < 5000 + 50);
        assert!(tool_msg.content.contains("…(") && tool_msg.content.contains("omitted)"));
    }

    #[test]
    fn test_build_messages_no_cap_passthrough() {
        // Small tool output under cap should be left intact
        let msgs = vec![
            Message::new("tool", "[TOOL_OUTPUT: rg = found 3 lines]"),
        ];
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, Some(3000), None);
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
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, Some(100));
        // Some old turns should have been dropped
        assert!(result.len() < msgs.len());
        // First user message must always be preserved
        assert_eq!(result[0].content, "first user message");
    }

    #[test]
    fn test_build_messages_strips_think_output() {
        let msgs = vec![
            Message::new("user", "plan something"),
            Message::new("assistant", "let me think"),
            Message::new("tool", "[TOOL_OUTPUT: think = I should do X then Y]"),
            Message::new("assistant", "ok I'll do X"),
        ];
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, None);
        // think output must not appear in the sent messages
        for msg in &result {
            assert!(!msg.content.contains("[TOOL_OUTPUT: think ="),
                "think output must be stripped from context: {}", msg.content);
        }
        assert_eq!(result.len(), 3); // user + 2 assistant, no think
    }

    #[test]
    fn test_build_messages_deduplicates_tool_outputs() {
        // Same readfile call twice — only the latest should be verbatim
        let msgs = vec![
            Message::new("user", "help"),
            Message::new("tool", "[TOOL_OUTPUT: readfile src/main.rs = first read]"),
            Message::new("assistant", "let me check again"),
            Message::new("tool", "[TOOL_OUTPUT: readfile src/main.rs = second read (updated)]"),
        ];
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, None);
        // Latest readfile should be intact
        let has_second = result.iter().any(|m| m.content.contains("second read (updated)"));
        assert!(has_second, "latest tool output must be preserved");
        // First readfile should be stubbed out
        let has_first_verbatim = result.iter().any(|m| m.content.contains("first read") && !m.content.contains("superseded"));
        assert!(!has_first_verbatim, "earlier duplicate must be replaced with stub");
    }

    #[test]
    fn test_build_messages_rolling_assistant_trim() {
        // Generate 12 prose assistant turns under context pressure.
        // Rolling trim should reduce old turns; combined with sliding window, total is smaller.
        let mut msgs = vec![Message::new("user", "go")];
        for i in 0..12 {
            msgs.push(Message::new("assistant", &format!("step {} with lots of text here: {}", i, "x".repeat(200))));
            msgs.push(Message::new("user", "continue"));
        }
        // context_window=500 → rolling pressure threshold ≈ 960 chars; full content ≈ 2666 > threshold
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, Some(500));
        let assistant_count = result.iter().filter(|m| m.role == "assistant").count();
        // Under pressure: fewer than 12 assistant turns should survive
        assert!(assistant_count < 12,
            "under pressure, some assistant turns must be trimmed; got {}", assistant_count);
        // The most recent assistant message should be verbatim (not abbreviated)
        let last_assistant = result.iter().rfind(|m| m.role == "assistant").unwrap();
        assert!(!last_assistant.content.starts_with("[…prev:"),
            "most recent assistant turn must be verbatim: {}", last_assistant.content);
    }

    #[test]
    fn test_build_messages_rolling_trim_skipped_when_no_pressure() {
        // Without pressure (no context_window), rolling trim should NOT fire — full history preserved
        let mut msgs = vec![Message::new("user", "go")];
        for i in 0..12 {
            msgs.push(Message::new("assistant", &format!("step {} with lots of text here: {}", i, "x".repeat(200))));
            msgs.push(Message::new("user", "continue"));
        }
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, None);
        // With no context_window: no rolling trim, no sliding window — all 12 assistant turns preserved verbatim
        let assistant_count = result.iter().filter(|m| m.role == "assistant").count();
        assert_eq!(assistant_count, 12,
            "without pressure, all 12 assistant turns must be preserved; got {}", assistant_count);
        let first_assistant = result.iter().find(|m| m.role == "assistant").unwrap();
        assert!(!first_assistant.content.starts_with("[…prev:"),
            "without pressure, assistant turns must be verbatim: {}", first_assistant.content);
    }

    #[test]
    fn test_build_messages_trims_old_tool_call_json() {
        // Pure JSON tool-call messages outside the keep window should be dropped entirely (under pressure)
        let tool_call = r#"{"tool_calls": [{"name": "readfile", "parameters": {"path": "src/a.rs"}}]}"#;
        let mut msgs = vec![Message::new("user", "go")];
        for _ in 0..12 {
            msgs.push(Message::new("assistant", tool_call));
            msgs.push(Message::new("user", "continue"));
        }
        // context_window=200 → pressure threshold ≈ 384 chars; 12 * 81 ≈ 972 > 384, so trim fires
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, Some(200));
        // Old tool-call-only assistant turns must be dropped (not summarised as [assistant: {…}])
        let old_tool_call_visible = result.iter().any(|m| m.role == "assistant"
            && m.content.contains("tool_calls")
            && (m.content.starts_with("[assistant:") || m.content.starts_with("[…prev:")));
        assert!(!old_tool_call_visible,
            "old JSON tool-call messages must be dropped not summarised");
    }

    #[test]
    fn test_build_messages_drops_narration_plus_json_under_pressure() {
        // synthesize_tool_narration combines prose + raw JSON: "Running: `cmd`.\n{\"tool_calls\":[...]}"
        // Rolling trim must DROP these entirely when trimming (not summarise them), to prevent
        // the model from echoing "[assistant: {" in its next response.
        let narration_json = "Running: `ls src/`.\n{\"tool_calls\": [{\"name\": \"shell\", \"parameters\": {\"command\": \"ls src/\"}}]}";
        let mut msgs = vec![Message::new("user", "go")];
        for _ in 0..12 {
            msgs.push(Message::new("assistant", narration_json));
            msgs.push(Message::new("user", "continue"));
        }
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, Some(200));
        // The failure mode: old narration+JSON gets summarised as "[…prev: Running: `ls`.\n{...}"
        // which leaks the JSON into context.  Ensure no such message exists.
        let summarised_with_json = result.iter().any(|m| m.role == "assistant"
            && m.content.starts_with("[…prev:")
            && m.content.contains("tool_calls"));
        assert!(!summarised_with_json,
            "narration+JSON assistant messages must be dropped (not summarised) under pressure");
    }

    #[test]
    fn test_build_messages_batch_dedup_all_keys() {
        // A batch result message contains two [TOOL_OUTPUT:] blocks.
        // Both keys appear in a more-recent message → the old batch message must be collapsed.
        let msgs = vec![
            Message::new("user", "go"),
            // older batch result
            Message::new("tool", "[TOOL_OUTPUT: shell = old ls]\n[TOOL_OUTPUT: readfile src/a.rs = old contents]"),
            Message::new("assistant", "ok"),
            // newer individual results for same keys
            Message::new("tool", "[TOOL_OUTPUT: shell = new ls]"),
            Message::new("tool", "[TOOL_OUTPUT: readfile src/a.rs = new contents]"),
        ];
        let (result, _, _, _) = OllamaClient::build_messages(&msgs, None, None, None);
        let old_batch_verbatim = result.iter().any(|m| m.content.contains("old ls") && !m.content.contains("superseded"));
        assert!(!old_batch_verbatim,
            "old batch result whose all keys are superseded must be collapsed");
    }

    #[test]
    fn test_num_keep_in_options() {
        // Verify num_keep is serialised correctly
        let opts = OllamaOptions::default().with_num_keep(42);
        let json = serde_json::to_string(&opts).unwrap();
        assert!(json.contains("\"num_keep\":42"), "num_keep must appear in JSON: {json}");
    }

    #[test]
    fn test_models_response_parsing() {
        let json = r#"{"models":[{"name":"llama2"},{"name":"qwen:3.5"}]}"#;
        let response: ModelsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.models.len(), 2);
        assert_eq!(response.models[0].name, "llama2");
    }

    #[test]
    fn test_detect_api_format_openrouter() {
        assert_eq!(detect_api_format("https://openrouter.ai/api"), ApiFormat::OpenAI);
    }
    #[test]
    fn test_detect_api_format_openai() {
        assert_eq!(detect_api_format("https://api.openai.com/v1"), ApiFormat::OpenAI);
    }
    #[test]
    fn test_detect_api_format_groq() {
        assert_eq!(detect_api_format("https://api.groq.com/openai/v1"), ApiFormat::OpenAI);
    }
    #[test]
    fn test_detect_api_format_localhost() {
        assert_eq!(detect_api_format("http://localhost:11434"), ApiFormat::Ollama);
    }
    #[test]
    fn test_detect_api_format_custom() {
        assert_eq!(detect_api_format("http://192.168.1.5:11434"), ApiFormat::Ollama);
    }

    #[test]
    fn test_openai_chat_url_with_v1_suffix() {
        assert_eq!(openai_chat_url("https://openrouter.ai/api/v1"), "https://openrouter.ai/api/v1/chat/completions");
    }
    #[test]
    fn test_openai_chat_url_with_v1_suffix_trailing_slash() {
        assert_eq!(openai_chat_url("https://openrouter.ai/api/v1/"), "https://openrouter.ai/api/v1/chat/completions");
    }
    #[test]
    fn test_openai_chat_url_without_v1() {
        assert_eq!(openai_chat_url("https://api.openai.com"), "https://api.openai.com/v1/chat/completions");
    }
    #[test]
    fn test_openai_sse_parse_content_token() {
        let line = r#"{"id":"gen-1","object":"chat.completion.chunk","created":1,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello","role":"assistant"},"finish_reason":null}]}"#;
        let chunk: OAIStreamChunk = serde_json::from_str(line).unwrap();
        assert_eq!(chunk.choices[0].delta.content, "Hello");
        assert!(chunk.choices[0].delta.reasoning_content.is_empty());
    }
    #[test]
    fn test_openai_sse_parse_reasoning_token() {
        let line = r#"{"id":"gen-2","object":"chat.completion.chunk","created":1,"model":"deepseek-r1","choices":[{"index":0,"delta":{"reasoning_content":"thinking...","content":""},"finish_reason":null}]}"#;
        let chunk: OAIStreamChunk = serde_json::from_str(line).unwrap();
        assert_eq!(chunk.choices[0].delta.reasoning_content, "thinking...");
    }
    #[test]
    fn test_openai_sse_parse_finish_reason() {
        let line = r#"{"id":"gen-3","object":"chat.completion.chunk","created":1,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let chunk: OAIStreamChunk = serde_json::from_str(line).unwrap();
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));
    }
    #[test]
    fn test_openai_sse_parse_usage() {
        let line = r#"{"id":"gen-4","object":"chat.completion.chunk","created":1,"model":"gpt-4","choices":[],"usage":{"prompt_tokens":14,"completion_tokens":10}}"#;
        let chunk: OAIStreamChunk = serde_json::from_str(line).unwrap();
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 14);
        assert_eq!(usage.completion_tokens, 10);
    }

    // ---- split_newline_buffer ----

    #[test]
    fn test_split_newline_single_complete() {
        let (lines, remainder) = split_newline_buffer("hello\n");
        assert_eq!(lines, vec!["hello"]);
        assert_eq!(remainder, "");
    }

    #[test]
    fn test_split_newline_line_and_partial() {
        let (lines, remainder) = split_newline_buffer("hello\nwor");
        assert_eq!(lines, vec!["hello"]);
        assert_eq!(remainder, "wor");
    }

    #[test]
    fn test_split_newline_multi_with_partial() {
        let (lines, remainder) = split_newline_buffer("a\nb\nc\npar");
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert_eq!(remainder, "par");
    }

    // ---- merge_thinking ----

    #[test]
    fn test_merge_thinking_content_wins() {
        assert_eq!(merge_thinking("hi".into(), "thought".into()), "hi");
    }

    #[test]
    fn test_merge_thinking_fallback_to_think_tag() {
        assert_eq!(merge_thinking("".into(), "thought".into()), "<think>thought</think>");
    }

    #[test]
    fn test_merge_thinking_both_empty() {
        assert_eq!(merge_thinking("".into(), "".into()), "");
    }

    // ---- parse_ollama_chunk ----

    #[test]
    fn test_parse_ollama_chunk_token() {
        let line = r#"{"model":"q","created_at":"","message":{"role":"assistant","content":"Hello"},"done":false}"#;
        let mut ht = false;
        let events = parse_ollama_chunk(line, &mut ht, false, 0);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Token(ref s) if s == "Hello"));
    }

    #[test]
    fn test_parse_ollama_chunk_think_token() {
        let line = r#"{"model":"q","created_at":"","message":{"role":"assistant","content":"","thinking":"step1"},"done":false}"#;
        let mut ht = false;
        let events = parse_ollama_chunk(line, &mut ht, false, 0);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::ThinkToken(ref s) if s == "step1"));
        assert!(ht);
    }

    #[test]
    fn test_parse_ollama_chunk_dual_think_and_content() {
        let line = r#"{"model":"q","created_at":"","message":{"role":"assistant","content":"ans","thinking":"t1"},"done":false}"#;
        let mut ht = false;
        let events = parse_ollama_chunk(line, &mut ht, false, 0);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::ThinkToken(_)));
        assert!(matches!(events[1], StreamEvent::Token(_)));
    }

    #[test]
    fn test_parse_ollama_chunk_done_with_stats() {
        let line = r#"{"model":"q","created_at":"","message":null,"done":true,"prompt_eval_count":10,"eval_count":20,"eval_duration":1000}"#;
        let mut ht = false;
        let events = parse_ollama_chunk(line, &mut ht, false, 0);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { prompt_tokens, gen_tokens, eval_duration_ns, .. } => {
                assert_eq!(*prompt_tokens, 10);
                assert_eq!(*gen_tokens, 20);
                assert_eq!(*eval_duration_ns, Some(1000));
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn test_parse_ollama_chunk_done_no_stats() {
        let line = r#"{"model":"q","created_at":"","done":true}"#;
        let mut ht = false;
        let events = parse_ollama_chunk(line, &mut ht, false, 0);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { prompt_tokens, gen_tokens, .. } => {
                assert_eq!(*prompt_tokens, 0);
                assert_eq!(*gen_tokens, 0);
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn test_parse_ollama_chunk_empty_content_no_event() {
        let line = r#"{"model":"q","created_at":"","message":{"role":"assistant","content":""},"done":false}"#;
        let mut ht = false;
        let events = parse_ollama_chunk(line, &mut ht, false, 0);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_ollama_chunk_malformed_no_panic() {
        let mut ht = false;
        let events = parse_ollama_chunk("not json {{{{", &mut ht, false, 0);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_ollama_chunk_empty_line() {
        let mut ht = false;
        let events = parse_ollama_chunk("", &mut ht, false, 0);
        assert!(events.is_empty());
    }

    // ---- parse_openai_sse_data ----

    #[test]
    fn test_sse_data_content_token() {
        let data = r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#;
        let mut state = OaiStreamState::default();
        let result = parse_openai_sse_data(data, &mut state, false, 0);
        match result {
            SseDataResult::Events(events) => {
                assert_eq!(events.len(), 1);
                assert!(matches!(events[0], StreamEvent::Token(ref s) if s == "hi"));
            }
            _ => panic!("expected Events"),
        }
    }

    #[test]
    fn test_sse_data_reasoning_content() {
        let data = r#"{"id":"x","choices":[{"index":0,"delta":{"reasoning_content":"think","content":""},"finish_reason":null}]}"#;
        let mut state = OaiStreamState::default();
        let result = parse_openai_sse_data(data, &mut state, false, 0);
        match result {
            SseDataResult::Events(events) => {
                assert_eq!(events.len(), 1);
                assert!(matches!(events[0], StreamEvent::ThinkToken(ref s) if s == "think"));
                assert!(state.had_thinking);
            }
            _ => panic!("expected Events"),
        }
    }

    #[test]
    fn test_sse_data_usage_chunk() {
        let data = r#"{"id":"x","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":8}}"#;
        let mut state = OaiStreamState::default();
        let result = parse_openai_sse_data(data, &mut state, false, 0);
        match result {
            SseDataResult::Events(events) => {
                assert!(events.is_empty());
                assert_eq!(state.prompt_tokens, 5);
                assert_eq!(state.gen_tokens, 8);
            }
            _ => panic!("expected Events"),
        }
    }

    #[test]
    fn test_sse_data_done_with_prior_usage() {
        let mut state = OaiStreamState { prompt_tokens: 3, gen_tokens: 7, had_thinking: false };
        let result = parse_openai_sse_data("[DONE]", &mut state, false, 0);
        match result {
            SseDataResult::Done(StreamEvent::Done { prompt_tokens, gen_tokens, .. }) => {
                assert_eq!(prompt_tokens, 3);
                assert_eq!(gen_tokens, 7);
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn test_sse_data_done_no_prior_usage() {
        let mut state = OaiStreamState::default();
        let result = parse_openai_sse_data("[DONE]", &mut state, false, 0);
        match result {
            SseDataResult::Done(StreamEvent::Done { prompt_tokens, gen_tokens, .. }) => {
                assert_eq!(prompt_tokens, 0);
                assert_eq!(gen_tokens, 0);
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn test_sse_data_malformed_skip() {
        let mut state = OaiStreamState::default();
        let result = parse_openai_sse_data("not json", &mut state, false, 0);
        assert!(matches!(result, SseDataResult::Skip));
    }

    #[test]
    fn test_sse_data_empty_skip() {
        let mut state = OaiStreamState::default();
        let result = parse_openai_sse_data("", &mut state, false, 0);
        assert!(matches!(result, SseDataResult::Skip));
    }

    #[test]
    fn test_sse_data_multi_choice() {
        let data = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"a"},"finish_reason":null},{"index":1,"delta":{"content":"b"},"finish_reason":null}]}"#;
        let mut state = OaiStreamState::default();
        let result = parse_openai_sse_data(data, &mut state, false, 0);
        match result {
            SseDataResult::Events(events) => {
                assert_eq!(events.len(), 2);
            }
            _ => panic!("expected Events"),
        }
    }

    // ---- parse_llamacpp_chunk ----

    #[test]
    fn test_llamacpp_content_token() {
        let line = r#"{"content":"hello","stop":false}"#;
        let events = parse_llamacpp_chunk(line, false, 0);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Token(ref s) if s == "hello"));
    }

    #[test]
    fn test_llamacpp_stop_with_content() {
        let line = r#"{"content":"last","stop":true}"#;
        let events = parse_llamacpp_chunk(line, false, 0);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::Token(ref s) if s == "last"));
        assert!(matches!(events[1], StreamEvent::Done { .. }));
    }

    #[test]
    fn test_llamacpp_stop_empty_content() {
        let line = r#"{"content":"","stop":true}"#;
        let events = parse_llamacpp_chunk(line, false, 0);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Done { .. }));
    }

    #[test]
    fn test_llamacpp_malformed_no_panic() {
        let events = parse_llamacpp_chunk("{bad json", false, 0);
        assert!(events.is_empty());
    }

    #[test]
    fn test_llamacpp_empty_line() {
        let events = parse_llamacpp_chunk("", false, 0);
        assert!(events.is_empty());
    }
}
