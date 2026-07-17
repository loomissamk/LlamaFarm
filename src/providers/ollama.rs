use crate::agent::loop_::parsing::parse_tool_calls;
use crate::multimodal;
use crate::providers::traits::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, TokenUsage, ToolCall,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub struct OllamaProvider {
    base_url: String,
    api_key: Option<String>,
    reasoning_enabled: Option<bool>,
    /// Number of model layers to load onto GPU(s).
    /// - `None`  → let the Ollama server decide (uses its own `OLLAMA_NUM_GPU` or auto-detect).
    /// - `Some(0)` → CPU-only (no GPU).
    /// - `Some(999)` → fill GPU to capacity, spill remaining layers to CPU (max-GPU mode).
    gpu_layers: Option<i32>,
    /// Index of the GPU to use for the largest tensor weight (default 0).
    main_gpu: Option<u32>,
    /// Context window override (tokens).
    /// - `None` → auto-detect: LlamaFarm fetches the model's native context length via
    ///   `/api/show` and uses it, so the full context the model was trained on is available.
    /// - `Some(n)` → use exactly n tokens (manual override or from `OLLAMA_NUM_CTX`).
    /// Pair with `OLLAMA_KV_CACHE_TYPE=q8_0` server env to halve KV cache VRAM usage.
    num_ctx: Option<u32>,
    /// Maximum tokens produced by one `/api/chat` inference segment. Ollama
    /// counts hidden thinking tokens here too, so this is the hard per-turn
    /// reasoning/output budget used to prevent a single thought from pinning a
    /// local GPU indefinitely. The agent loop continues a length-stopped
    /// segment automatically with its checkpointed history.
    max_output_tokens: Option<u32>,
    /// Cache of model-name → resolved num_ctx to avoid a `/api/show` round-trip on
    /// every request when operating in auto-detect mode.
    ctx_cache: Arc<RwLock<HashMap<String, u32>>>,
    /// Cache of model-name → Ollama-reported capabilities (e.g. ["tools", "completion"]).
    /// Populated lazily on first `/api/show` call for each model.
    caps_cache: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

// ─── Request Structures ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    options: Options,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    /// How long Ollama keeps the model loaded after the request. "-1" pins it
    /// in VRAM so subsequent turns skip the multi-second reload that shows up
    /// as a huge time-to-first-token. Configurable; defaults to keeping it hot.
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<String>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OutgoingToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct OutgoingToolCall {
    #[serde(rename = "type")]
    kind: String,
    function: OutgoingFunction,
}

#[derive(Debug, Serialize)]
struct OutgoingFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Options {
    temperature: f64,
    /// GPU layer count: 0 = CPU-only, 999 = fill GPU then spill to CPU.
    /// Omitted if None so the Ollama server uses its own default.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_gpu: Option<i32>,
    /// Which GPU index to use for the largest tensors (0-indexed).
    #[serde(skip_serializing_if = "Option::is_none")]
    main_gpu: Option<u32>,
    /// Override the model's default context window size.
    /// Set higher (e.g. 32768, 65536) to use more context for long autonomous runs.
    /// Requires sufficient VRAM; pair with `OLLAMA_KV_CACHE_TYPE=q8_0` to stretch VRAM.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_ctx: Option<u32>,
    /// Maximum generated tokens for this inference segment. Ollama calls this
    /// `num_predict`; it includes hidden thinking/reasoning tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

// ─── Response Structures ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    message: ResponseMessage,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    /// Nanoseconds spent loading the model into memory (0 when already loaded).
    #[serde(default)]
    load_duration: Option<u64>,
    /// Nanoseconds spent processing the input prompt (prefill phase).
    #[serde(default)]
    prompt_eval_duration: Option<u64>,
    /// Nanoseconds spent generating output tokens (decode phase).
    #[serde(default)]
    eval_duration: Option<u64>,
    /// Total nanoseconds for the full request.
    #[serde(default)]
    total_duration: Option<u64>,
    /// Ollama reports `"length"` when `options.num_predict` ended generation.
    #[serde(default)]
    done_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
    /// Some models return a "thinking" field with internal reasoning
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    id: Option<String>,
    function: OllamaFunction,
}

#[derive(Debug, Deserialize)]
struct OllamaFunction {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

// ─── Implementation ───────────────────────────────────────────────────────────

/// Keep-alive for the loaded model. Defaults to "-1" (pin in VRAM) so back-to-
/// back chat turns skip the reload that otherwise shows as a large TTFT. Set
/// `OLLAMA_KEEP_ALIVE` (e.g. "5m", "0", "-1") to override.
fn resolve_keep_alive() -> String {
    std::env::var("OLLAMA_KEEP_ALIVE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "-1".to_string())
}

impl OllamaProvider {
    fn normalize_base_url(raw_url: &str) -> String {
        let trimmed = raw_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return String::new();
        }

        trimmed
            .strip_suffix("/api")
            .unwrap_or(trimmed)
            .trim_end_matches('/')
            .to_string()
    }

    pub fn new(base_url: Option<&str>, api_key: Option<&str>) -> Self {
        Self::new_with_reasoning(base_url, api_key, None)
    }

    pub fn new_with_reasoning(
        base_url: Option<&str>,
        api_key: Option<&str>,
        reasoning_enabled: Option<bool>,
    ) -> Self {
        Self::new_with_gpu(base_url, api_key, reasoning_enabled, None, None)
    }

    /// Full constructor.
    ///
    /// `gpu_layers`:
    /// - `None`     → defer to Ollama server / `OLLAMA_NUM_GPU` env var.
    /// - `Some(0)`  → CPU-only inference.
    /// - `Some(999)` → fill every available GPU layer slot; layers that don't fit
    ///   automatically overflow to CPU RAM. This is the recommended setting for
    ///   "max GPU, fallback CPU" behaviour on a local box.
    ///
    /// If `gpu_layers` is `None` *and* the `OLLAMA_GPU_LAYERS` env var is set,
    /// its value is used so callers don't need to thread it through manually.
    pub fn new_with_gpu(
        base_url: Option<&str>,
        api_key: Option<&str>,
        reasoning_enabled: Option<bool>,
        gpu_layers: Option<i32>,
        main_gpu: Option<u32>,
    ) -> Self {
        Self::new_full(
            base_url,
            api_key,
            reasoning_enabled,
            gpu_layers,
            main_gpu,
            None,
            None,
        )
    }

    /// Full constructor with all inference options.
    ///
    /// - `gpu_layers`: GPU layer offload count (999 = fill GPU, spill rest to CPU).
    /// - `main_gpu`: GPU index for largest tensors.
    /// - `num_ctx`: Context window override. Pair with `OLLAMA_KV_CACHE_TYPE=q8_0`
    ///   to fit larger contexts in the same VRAM (turboquant-style KV compression).
    /// - `max_output_tokens`: Per-request output/reasoning segment budget. For
    ///   Ollama this becomes `options.num_predict` and applies to hidden thinking
    ///   as well as visible output.
    pub fn new_full(
        base_url: Option<&str>,
        api_key: Option<&str>,
        reasoning_enabled: Option<bool>,
        gpu_layers: Option<i32>,
        main_gpu: Option<u32>,
        num_ctx: Option<u32>,
        max_output_tokens: Option<u32>,
    ) -> Self {
        let api_key = api_key.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });

        // If caller didn't specify, check environment variables.
        // Ollama itself also reads OLLAMA_NUM_GPU but we mirror it here so the
        // per-request options field is populated even when the server default
        // differs from what the user configured in LlamaFarm.
        let gpu_layers = gpu_layers.or_else(|| {
            std::env::var("OLLAMA_GPU_LAYERS")
                .or_else(|_| std::env::var("OLLAMA_NUM_GPU"))
                .ok()
                .and_then(|v| v.trim().parse::<i32>().ok())
        });

        // Resolve num_ctx: caller value → OLLAMA_NUM_CTX env var → None.
        let num_ctx = num_ctx.or_else(|| {
            std::env::var("OLLAMA_NUM_CTX")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
        });

        let max_output_tokens = max_output_tokens.filter(|value| *value > 0).or_else(|| {
            std::env::var("LLAMAFARM_AGENT_MAX_OUTPUT_TOKENS")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|value| *value > 0)
        });

        Self {
            base_url: Self::normalize_base_url(base_url.unwrap_or("http://localhost:11434")),
            api_key,
            reasoning_enabled,
            gpu_layers,
            main_gpu,
            num_ctx,
            max_output_tokens,
            ctx_cache: Arc::new(RwLock::new(HashMap::new())),
            caps_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn is_local_endpoint(&self) -> bool {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(|host| host.to_string()))
            .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.ollama", 300, 10)
    }

    /// Generation deliberately has no fixed wall-clock deadline. A local model
    /// may be slow while it is CPU-spilling or pre-filling a large context; the
    /// per-segment `num_predict` budget gives it regular safe continuation
    /// boundaries instead. The agent's cancellation token can still abort the
    /// request immediately when the operator presses Stop.
    fn chat_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_optional_timeouts(
            "provider.ollama.chat",
            None,
            Some(10),
        )
    }

    fn output_budget_exhausted(done_reason: Option<&str>) -> bool {
        matches!(
            done_reason
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(|reason| reason.to_ascii_lowercase())
                .as_deref(),
            Some("length" | "max_tokens" | "max_output_tokens" | "num_predict")
        )
    }

    fn resolve_request_details(&self, model: &str) -> anyhow::Result<(String, bool)> {
        let requests_cloud = model.ends_with(":cloud");
        let is_local_endpoint = self.is_local_endpoint();
        let normalized_model = if requests_cloud && !is_local_endpoint {
            model.strip_suffix(":cloud").unwrap_or(model).to_string()
        } else {
            model.to_string()
        };

        // Local Ollama instances can proxy cloud-model requests after
        // `ollama signin`, so only direct remote ollama.com usage needs an API key.
        if requests_cloud && !is_local_endpoint && self.api_key.is_none() {
            anyhow::bail!(
                "Model '{}' requested cloud routing, but no API key is configured. Set OLLAMA_API_KEY or config api_key.",
                model
            );
        }

        let should_auth = self.api_key.is_some() && !is_local_endpoint;

        Ok((normalized_model, should_auth))
    }

    fn parse_tool_arguments(arguments: &str) -> serde_json::Value {
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}))
    }

    fn normalize_response_text(content: String) -> Option<String> {
        if content.trim().is_empty() {
            None
        } else {
            Some(content)
        }
    }

    fn fallback_text_for_empty_content(model: &str, thinking: Option<&str>) -> String {
        if let Some(thinking) = thinking.map(str::trim).filter(|value| !value.is_empty()) {
            let thinking_log_excerpt: String = thinking.chars().take(100).collect();
            tracing::warn!(
                "Ollama returned empty content with only thinking for model '{}': '{}'. Model may have stopped prematurely.",
                model,
                thinking_log_excerpt
            );
            return "Ollama returned internal reasoning but no final answer. Retrying the request or enabling a tool-followthrough retry is recommended."
                .to_string();
        }

        tracing::warn!(
            "Ollama returned empty or whitespace content with no tool calls for model '{}'",
            model
        );
        "I couldn't get a complete response from Ollama. Please try again or switch to a different model."
            .to_string()
    }

    /// Return the effective num_ctx for `model`.
    ///
    /// Resolution order:
    /// 1. Caller-configured / env-var value (`self.num_ctx`) — explicit wins.
    /// 2. Cache hit from a previous auto-detect for this model.
    /// 3. Live `/api/show` lookup → model's native `context_length`.
    /// 4. Fallback: 32768 (safe default for agentic runs, much better than
    ///    Ollama's hardcoded 2048).
    ///
    /// The result is memoized so subsequent requests pay zero extra latency.
    async fn resolve_num_ctx(&self, model: &str) -> u32 {
        if let Some(n) = self.num_ctx {
            return n;
        }
        // Check cache first (read lock — contention-free in the normal case).
        if let Ok(cache) = self.ctx_cache.read() {
            if let Some(&cached) = cache.get(model) {
                return cached;
            }
        }
        // Cache miss: ask Ollama for the model's native context length.
        let resolved = match self.show_model(model).await {
            Ok(info) => {
                let native = info.context_length();
                // Cap at 131072 (128k) — sensible ceiling even for models that
                // report larger values, and avoids KV-cache OOM on most hardware.
                u32::try_from(native.min(131_072)).unwrap_or(131_072)
            }
            Err(e) => {
                tracing::debug!(model, "auto ctx lookup failed ({e}), using 32768 fallback");
                32_768
            }
        };
        if let Ok(mut cache) = self.ctx_cache.write() {
            cache.insert(model.to_string(), resolved);
        }
        resolved
    }

    fn build_chat_request(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: f64,
        tools: Option<&[serde_json::Value]>,
        num_ctx: u32,
    ) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            messages,
            stream: false,
            options: Options {
                temperature,
                num_gpu: self.gpu_layers,
                main_gpu: self.main_gpu,
                num_ctx: Some(num_ctx),
                num_predict: self.max_output_tokens,
            },
            keep_alive: Some(resolve_keep_alive()),
            // Only send think:true when the model reports "thinking" capability.
            // Sending it to non-thinking models causes a 400 from llama-server.
            think: if self.reasoning_enabled == Some(true) {
                let model_supports_thinking = self
                    .caps_cache
                    .read()
                    .ok()
                    .and_then(|cache| {
                        cache
                            .get(model)
                            .map(|caps| caps.iter().any(|c| c == "thinking"))
                    })
                    .unwrap_or(false); // unknown → don't send think, safer default
                if model_supports_thinking {
                    Some(true)
                } else {
                    None
                }
            } else {
                self.reasoning_enabled
            },
            tools: tools.map(|t| t.to_vec()),
        }
    }

    fn convert_user_message_content(&self, content: &str) -> (Option<String>, Option<Vec<String>>) {
        let (cleaned, image_refs) = multimodal::parse_image_markers(content);
        if image_refs.is_empty() {
            return (Some(content.to_string()), None);
        }

        let images: Vec<String> = image_refs
            .iter()
            .filter_map(|reference| multimodal::extract_ollama_image_payload(reference))
            .collect();

        if images.is_empty() {
            return (Some(content.to_string()), None);
        }

        let cleaned = cleaned.trim();
        let content = if cleaned.is_empty() {
            None
        } else {
            Some(cleaned.to_string())
        };

        (content, Some(images))
    }

    /// Convert internal chat history format to Ollama's native tool-call message schema.
    ///
    /// `run_tool_call_loop` stores native assistant/tool entries as JSON strings in
    /// `ChatMessage.content`. We decode those payloads here so follow-up requests send
    /// structured `assistant.tool_calls` and `tool.tool_name`, as expected by Ollama.
    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<Message> {
        let mut tool_name_by_id: HashMap<String, String> = HashMap::new();

        messages
            .iter()
            .map(|message| {
                if message.role == "assistant" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) {
                        if let Some(tool_calls_value) = value.get("tool_calls") {
                            if let Ok(parsed_calls) =
                                serde_json::from_value::<Vec<ToolCall>>(tool_calls_value.clone())
                            {
                                let outgoing_calls: Vec<OutgoingToolCall> = parsed_calls
                                    .into_iter()
                                    .map(|call| {
                                        tool_name_by_id.insert(call.id.clone(), call.name.clone());
                                        OutgoingToolCall {
                                            kind: "function".to_string(),
                                            function: OutgoingFunction {
                                                name: call.name,
                                                arguments: Self::parse_tool_arguments(
                                                    &call.arguments,
                                                ),
                                            },
                                        }
                                    })
                                    .collect();
                                let content = value
                                    .get("content")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToString::to_string);
                                return Message {
                                    role: "assistant".to_string(),
                                    content,
                                    images: None,
                                    tool_calls: Some(outgoing_calls),
                                    tool_name: None,
                                };
                            }
                        }
                    }
                }

                if message.role == "tool" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) {
                        let tool_name = value
                            .get("tool_name")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string)
                            .or_else(|| {
                                value
                                    .get("tool_call_id")
                                    .and_then(serde_json::Value::as_str)
                                    .and_then(|id| tool_name_by_id.get(id))
                                    .cloned()
                            });
                        let content = value
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string)
                            .or_else(|| {
                                (!message.content.trim().is_empty())
                                    .then_some(message.content.clone())
                            });

                        return Message {
                            role: "tool".to_string(),
                            content,
                            images: None,
                            tool_calls: None,
                            tool_name,
                        };
                    }
                }

                if message.role == "user" {
                    let (content, images) = self.convert_user_message_content(&message.content);
                    return Message {
                        role: "user".to_string(),
                        content,
                        images,
                        tool_calls: None,
                        tool_name: None,
                    };
                }

                Message {
                    role: message.role.clone(),
                    content: Some(message.content.clone()),
                    images: None,
                    tool_calls: None,
                    tool_name: None,
                }
            })
            .collect()
    }

    /// Send a request to Ollama and get the parsed response.
    /// Pass `tools` to enable native function-calling for models that support it.
    async fn send_request(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: f64,
        should_auth: bool,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<ApiChatResponse> {
        let num_ctx = self.resolve_num_ctx(model).await;
        let request = self.build_chat_request(messages, model, temperature, tools, num_ctx);

        let url = format!("{}/api/chat", self.base_url);

        tracing::debug!(
            "Ollama request: url={} model={} message_count={} temperature={} think={:?} tool_count={} num_predict={:?}",
            url,
            model,
            request.messages.len(),
            temperature,
            request.think,
            request.tools.as_ref().map_or(0, |t| t.len()),
            request.options.num_predict,
        );

        let mut request_builder = self.chat_client().post(&url).json(&request);

        if should_auth {
            if let Some(key) = self.api_key.as_ref() {
                request_builder = request_builder.bearer_auth(key);
            }
        }

        let response = request_builder.send().await?;
        let status = response.status();
        tracing::debug!("Ollama response status: {}", status);

        let body = response.bytes().await?;
        tracing::debug!("Ollama response body length: {} bytes", body.len());

        if !status.is_success() {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            tracing::error!(
                "Ollama error response: status={} body_excerpt={}",
                status,
                sanitized
            );
            anyhow::bail!(
                "Ollama API error ({}): {}. Is Ollama running? (brew install ollama && ollama serve)",
                status,
                sanitized
            );
        }

        let chat_response: ApiChatResponse = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                let raw = String::from_utf8_lossy(&body);
                let sanitized = super::sanitize_api_error(&raw);
                tracing::error!(
                    "Ollama response deserialization failed: {e}. body_excerpt={}",
                    sanitized
                );
                anyhow::bail!("Failed to parse Ollama response: {e}");
            }
        };

        Ok(chat_response)
    }

    /// Convert Ollama tool calls to the JSON format expected by parse_tool_calls in loop_.rs
    ///
    /// Handles quirky model behavior where tool calls are wrapped:
    /// - `{"name": "tool_call", "arguments": {"name": "shell", "arguments": {...}}}`
    /// - `{"name": "tool.shell", "arguments": {...}}`
    fn format_tool_calls_for_loop(&self, tool_calls: &[OllamaToolCall]) -> String {
        let formatted_calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .map(|tc| {
                let (tool_name, tool_args) = self.extract_tool_name_and_args(tc);

                // Arguments must be a JSON string for parse_tool_calls compatibility
                let args_str =
                    serde_json::to_string(&tool_args).unwrap_or_else(|_| "{}".to_string());

                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": args_str
                    }
                })
            })
            .collect();

        serde_json::json!({
            "content": "",
            "tool_calls": formatted_calls
        })
        .to_string()
    }

    /// Extract the actual tool name and arguments from potentially nested structures
    fn extract_tool_name_and_args(&self, tc: &OllamaToolCall) -> (String, serde_json::Value) {
        let name = &tc.function.name;
        let args = &tc.function.arguments;

        // Pattern 1: Nested tool_call wrapper (various malformed versions)
        // {"name": "tool_call", "arguments": {"name": "shell", "arguments": {"command": "date"}}}
        // {"name": "tool_call><json", "arguments": {"name": "shell", ...}}
        // {"name": "tool.call", "arguments": {"name": "shell", ...}}
        if name == "tool_call"
            || name == "tool.call"
            || name.starts_with("tool_call>")
            || name.starts_with("tool_call<")
        {
            if let Some(nested_name) = args.get("name").and_then(|v| v.as_str()) {
                let nested_args = args
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                tracing::debug!(
                    "Unwrapped nested tool call: {} -> {} with args {:?}",
                    name,
                    nested_name,
                    nested_args
                );
                return (nested_name.to_string(), nested_args);
            }
        }

        // Pattern 2: Prefixed tool name (tool.shell, tool.file_read, etc.)
        if let Some(stripped) = name.strip_prefix("tool.") {
            return (stripped.to_string(), args.clone());
        }

        // Pattern 3: Normal tool call
        (name.clone(), args.clone())
    }

    fn strip_json_code_fence(content: &str) -> &str {
        let trimmed = content.trim();
        if let Some(inner) = trimmed.strip_prefix("```") {
            let inner = inner
                .strip_prefix("json")
                .or_else(|| inner.strip_prefix("JSON"))
                .unwrap_or(inner);
            if let Some(inner) = inner.strip_suffix("```") {
                return inner.trim();
            }
        }
        trimmed
    }

    fn parse_content_tool_response(&self, content: &str) -> (Option<String>, Vec<ToolCall>) {
        let trimmed = Self::strip_json_code_fence(content);
        if trimmed.is_empty() {
            return (None, Vec::new());
        }

        let (text, parsed_calls) = parse_tool_calls(trimmed);
        let tool_calls = parsed_calls
            .into_iter()
            .enumerate()
            .map(|(idx, call)| ToolCall {
                id: call
                    .tool_call_id
                    .unwrap_or_else(|| format!("content-tool-call-{idx}")),
                name: call.name,
                arguments: serde_json::to_string(&call.arguments)
                    .unwrap_or_else(|_| "{}".to_string()),
            })
            .collect();

        (Self::normalize_response_text(text), tool_calls)
    }

    fn parse_content_tool_calls(&self, content: &str) -> Vec<ToolCall> {
        self.parse_content_tool_response(content).1
    }

    // ── Embeddings ───────────────────────────────────────────────

    /// Generate embeddings for `text` using the given Ollama model.
    ///
    /// Calls `POST /api/embeddings`. Returns a flat `Vec<f32>` embedding vector.
    pub async fn embed(&self, model: &str, text: &str) -> anyhow::Result<Vec<f32>> {
        #[derive(serde::Serialize)]
        struct EmbedRequest<'a> {
            model: &'a str,
            prompt: &'a str,
        }

        #[derive(serde::Deserialize)]
        struct EmbedResponse {
            embedding: Vec<f32>,
        }

        let url = format!("{}/api/embeddings", self.base_url);
        let client = self.http_client();
        let body = EmbedRequest {
            model,
            prompt: text,
        };

        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<EmbedResponse>()
            .await?;

        Ok(resp.embedding)
    }

    // ── Model management ─────────────────────────────────────────

    /// Pull a model from the Ollama registry (`POST /api/pull`).
    ///
    /// Streams progress lines until the pull is complete. Returns the final
    /// status string (typically `"success"`).
    pub async fn pull_model(&self, model: &str) -> anyhow::Result<String> {
        #[derive(serde::Serialize)]
        struct PullRequest<'a> {
            name: &'a str,
            stream: bool,
        }

        #[derive(serde::Deserialize)]
        struct PullStatus {
            status: String,
        }

        let url = format!("{}/api/pull", self.base_url);
        let client = self.http_client();
        let body = PullRequest {
            name: model,
            stream: true,
        };

        let mut response = client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        let mut last_status = String::from("unknown");
        while let Some(chunk) = response.chunk().await? {
            let text = String::from_utf8_lossy(&chunk);
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(status) = serde_json::from_str::<PullStatus>(line) {
                    last_status = status.status;
                }
            }
        }

        Ok(last_status)
    }

    /// Show metadata for an installed model (`POST /api/show`).
    ///
    /// Returns a `ModelInfo` containing context length, family, parameter size,
    /// quantization level, and supported capabilities.
    pub async fn show_model(&self, model: &str) -> anyhow::Result<OllamaModelInfo> {
        #[derive(serde::Serialize)]
        struct ShowRequest<'a> {
            name: &'a str,
        }

        let url = format!("{}/api/show", self.base_url);
        let client = self.http_client();
        let body = ShowRequest { name: model };

        let info = client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<OllamaModelInfo>()
            .await?;

        // Cache capabilities for sync lookup in the agent loop
        if let Ok(mut cache) = self.caps_cache.write() {
            cache.insert(model.to_string(), info.capabilities.clone());
        }

        Ok(info)
    }

    /// Returns `Some(true)` if Ollama reported this model supports tools (cached from last
    /// `/api/show` call), `Some(false)` if it reported it doesn't, or `None` if we haven't
    /// queried this model yet.
    pub fn cached_model_supports_tools(&self, model: &str) -> Option<bool> {
        self.caps_cache.read().ok().and_then(|cache| {
            cache
                .get(model)
                .map(|caps| caps.iter().any(|c| c == "tools"))
        })
    }

    /// List all locally installed models (`GET /api/tags`).
    ///
    /// Returns a `Vec<OllamaModelEntry>` with name, size, and details.
    pub async fn list_models(&self) -> anyhow::Result<Vec<OllamaModelEntry>> {
        #[derive(serde::Deserialize)]
        struct TagsResponse {
            models: Vec<OllamaModelEntry>,
        }

        let url = format!("{}/api/tags", self.base_url);
        let client = self.http_client();

        let resp = client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json::<TagsResponse>()
            .await?;

        Ok(resp.models)
    }
}

// ── Public Ollama model metadata types ───────────────────────────────────────

/// Metadata returned by `POST /api/show` for an installed Ollama model.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OllamaModelInfo {
    /// Model file template (optional).
    #[serde(default)]
    pub template: Option<String>,
    /// Model parameters metadata.
    #[serde(default)]
    pub details: OllamaModelDetails,
    /// Model info from modelfile (parameter size, context length, etc).
    #[serde(default)]
    pub model_info: std::collections::HashMap<String, serde_json::Value>,
    /// Ollama-reported capabilities (e.g. `["completion", "tools", "vision"]`).
    /// Available in Ollama ≥ 0.6. Empty for older versions.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl OllamaModelInfo {
    /// Returns true if Ollama reports this model supports native function calling.
    pub fn has_tools_capability(&self) -> bool {
        self.capabilities.iter().any(|c| c == "tools")
    }
}

impl OllamaModelInfo {
    /// Extract the context window size from model_info, falling back to 4096.
    pub fn context_length(&self) -> u64 {
        // Common keys across model families
        for key in &[
            "llama.context_length",
            "qwen2.context_length",
            "mistral.context_length",
            "phi3.context_length",
            "gemma.context_length",
            "gemma3.context_length",
            "gemma4.context_length",
            "context_length",
        ] {
            if let Some(v) = self.model_info.get(*key) {
                if let Some(n) = v.as_u64() {
                    return n;
                }
            }
        }
        4096
    }

    /// Whether this model supports native vision inputs.
    pub fn supports_vision(&self) -> bool {
        self.details
            .families
            .iter()
            .any(|f| f.contains("clip") || f.contains("vision") || f.contains("llava"))
    }
}

/// Details block returned by Ollama model listing and show endpoints.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OllamaModelDetails {
    #[serde(default)]
    pub parameter_size: String,
    #[serde(default)]
    pub quantization_level: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub families: Vec<String>,
}

/// A single model entry from `GET /api/tags`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OllamaModelEntry {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub details: OllamaModelDetails,
}

#[async_trait]
impl Provider for OllamaProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let (normalized_model, should_auth) = self.resolve_request_details(model)?;

        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: Some(sys.to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            });
        }

        let (user_content, user_images) = self.convert_user_message_content(message);
        messages.push(Message {
            role: "user".to_string(),
            content: user_content,
            images: user_images,
            tool_calls: None,
            tool_name: None,
        });

        let response = self
            .send_request(messages, &normalized_model, temperature, should_auth, None)
            .await?;

        // If model returned tool calls, format them for loop_.rs's parse_tool_calls
        if !response.message.tool_calls.is_empty() {
            tracing::debug!(
                "Ollama returned {} tool call(s), formatting for loop parser",
                response.message.tool_calls.len()
            );
            return Ok(self.format_tool_calls_for_loop(&response.message.tool_calls));
        }

        // Plain text response
        let content = response.message.content;
        if let Some(content) = Self::normalize_response_text(content) {
            return Ok(content);
        }

        Ok(Self::fallback_text_for_empty_content(
            &normalized_model,
            response.message.thinking.as_deref(),
        ))
    }

    async fn chat_with_history(
        &self,
        messages: &[crate::providers::ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let (normalized_model, should_auth) = self.resolve_request_details(model)?;

        let api_messages = self.convert_messages(messages);

        let response = self
            .send_request(
                api_messages,
                &normalized_model,
                temperature,
                should_auth,
                None,
            )
            .await?;

        // If model returned tool calls, format them for loop_.rs's parse_tool_calls
        if !response.message.tool_calls.is_empty() {
            tracing::debug!(
                "Ollama returned {} tool call(s), formatting for loop parser",
                response.message.tool_calls.len()
            );
            return Ok(self.format_tool_calls_for_loop(&response.message.tool_calls));
        }

        // Plain text response
        let content = response.message.content;
        if let Some(content) = Self::normalize_response_text(content) {
            return Ok(content);
        }

        Ok(Self::fallback_text_for_empty_content(
            &normalized_model,
            response.message.thinking.as_deref(),
        ))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let (normalized_model, should_auth) = self.resolve_request_details(model)?;

        let api_messages = self.convert_messages(messages);

        // Tools arrive pre-formatted in OpenAI/Ollama-compatible JSON from
        // tools_to_openai_format() in loop_.rs — pass them through directly.
        let tools_opt = if tools.is_empty() { None } else { Some(tools) };

        let response = self
            .send_request(
                api_messages,
                &normalized_model,
                temperature,
                should_auth,
                tools_opt,
            )
            .await?;

        let usage = if response.prompt_eval_count.is_some()
            || response.eval_count.is_some()
            || response.done_reason.is_some()
        {
            Some(TokenUsage {
                input_tokens: response.prompt_eval_count,
                output_tokens: response.eval_count,
                output_truncated: Self::output_budget_exhausted(response.done_reason.as_deref()),
            })
        } else {
            None
        };

        // Compute accurate inference metrics from Ollama's nanosecond timing fields.
        // generation_tps (decode TPS) is the meaningful throughput number — it excludes
        // model load and prefill time, showing only the sustained generation rate.
        let metrics = {
            let generation_tps =
                response
                    .eval_count
                    .zip(response.eval_duration)
                    .and_then(|(tokens, ns)| {
                        if ns > 0 {
                            Some(tokens as f64 / (ns as f64 / 1_000_000_000.0))
                        } else {
                            None
                        }
                    });
            let prefill_tps = response
                .prompt_eval_count
                .zip(response.prompt_eval_duration)
                .and_then(|(tokens, ns)| {
                    if ns > 0 {
                        Some(tokens as f64 / (ns as f64 / 1_000_000_000.0))
                    } else {
                        None
                    }
                });
            let ttft_ms = response.prompt_eval_duration.map(|prompt_ns| {
                let load_ns = response.load_duration.unwrap_or(0);
                (load_ns + prompt_ns) as f64 / 1_000_000.0
            });
            let total_ms = response.total_duration.map(|ns| ns as f64 / 1_000_000.0);
            if generation_tps.is_some() || ttft_ms.is_some() {
                Some(crate::providers::traits::InferenceMetrics {
                    ttft_ms,
                    generation_tps,
                    prefill_tps,
                    total_ms,
                })
            } else {
                None
            }
        };

        // Native tool calls returned by the model.
        if !response.message.tool_calls.is_empty() {
            let tool_calls: Vec<ToolCall> = response
                .message
                .tool_calls
                .iter()
                .map(|tc| {
                    let (name, args) = self.extract_tool_name_and_args(tc);
                    ToolCall {
                        id: tc
                            .id
                            .clone()
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        name,
                        arguments: serde_json::to_string(&args)
                            .unwrap_or_else(|_| "{}".to_string()),
                    }
                })
                .collect();
            let text = Self::normalize_response_text(response.message.content);
            return Ok(ChatResponse {
                text,
                tool_calls,
                usage,
                metrics,
                reasoning_content: response.message.thinking.clone(),
            });
        }

        // Some Ollama models advertise tool support but emit prompt-style JSON
        // tool calls in `message.content` instead of native `message.tool_calls`.
        let (prompt_text, prompt_tool_calls) =
            self.parse_content_tool_response(&response.message.content);
        if !prompt_tool_calls.is_empty() {
            tracing::debug!(
                "Ollama returned {} prompt-style tool call(s) in content, promoting to native tool calls",
                prompt_tool_calls.len()
            );
            return Ok(ChatResponse {
                text: prompt_text,
                tool_calls: prompt_tool_calls,
                usage,
                metrics,
                reasoning_content: response.message.thinking.clone(),
            });
        }

        // Plain text response.
        let content = response.message.content;
        let has_thinking = response
            .message
            .thinking
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty());
        let text = if let Some(content) = Self::normalize_response_text(content) {
            Some(content)
        } else if has_thinking {
            // Model finished its internal reasoning but emitted no visible text.
            // Return None so the loop's missing-tool-call retry fires and asks the
            // model to write its actual answer, rather than surfacing a raw error.
            tracing::warn!(
                model = normalized_model.as_str(),
                "model returned thinking with no content — triggering loop retry"
            );
            None
        } else {
            Some(Self::fallback_text_for_empty_content(
                &normalized_model,
                None,
            ))
        };
        Ok(ChatResponse {
            text,
            tool_calls: vec![],
            usage,
            metrics,
            reasoning_content: response.message.thinking.clone(),
        })
    }

    fn supports_native_tools(&self) -> bool {
        // Ollama's /api/chat supports native function-calling for capable models
        // (qwen2.5, llama3.1, mistral-nemo, etc.). chat_with_tools() sends tool
        // definitions in the request and returns structured ToolCall objects.
        true
    }

    fn cached_model_tool_support(&self, model: &str) -> Option<bool> {
        self.cached_model_supports_tools(model)
    }

    async fn prefetch_model_capabilities(&self, model: &str) {
        // If already cached, skip the network call
        if self.cached_model_supports_tools(model).is_some() {
            return;
        }
        let _ = self.show_model(model).await;
    }

    async fn chat(
        &self,
        request: crate::providers::traits::ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        // Convert ToolSpec to OpenAI-compatible JSON and delegate to chat_with_tools.
        if let Some(specs) = request.tools {
            if !specs.is_empty() {
                let tools: Vec<serde_json::Value> = specs
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": s.name,
                                "description": s.description,
                                "parameters": s.parameters
                            }
                        })
                    })
                    .collect();
                return self
                    .chat_with_tools(request.messages, &tools, model, temperature)
                    .await;
            }
        }

        // No tools — fall back to plain text chat.
        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;
        Ok(ChatResponse {
            text: Some(text),
            tool_calls: vec![],
            usage: None,
            metrics: None,
            reasoning_content: None,
        })
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url() {
        let p = OllamaProvider::new(None, None);
        assert_eq!(p.base_url, "http://localhost:11434");
    }

    #[test]
    fn custom_url_trailing_slash() {
        let p = OllamaProvider::new(Some("http://192.168.1.100:11434/"), None);
        assert_eq!(p.base_url, "http://192.168.1.100:11434");
    }

    #[test]
    fn custom_url_no_trailing_slash() {
        let p = OllamaProvider::new(Some("http://myserver:11434"), None);
        assert_eq!(p.base_url, "http://myserver:11434");
    }

    #[test]
    fn custom_url_strips_api_suffix() {
        let p = OllamaProvider::new(Some("https://ollama.com/api/"), None);
        assert_eq!(p.base_url, "https://ollama.com");
    }

    #[test]
    fn empty_url_uses_empty() {
        let p = OllamaProvider::new(Some(""), None);
        assert_eq!(p.base_url, "");
    }

    #[test]
    fn cloud_suffix_strips_model_name() {
        let p = OllamaProvider::new(Some("https://ollama.com"), Some("ollama-key"));
        let (model, should_auth) = p.resolve_request_details("qwen3:cloud").unwrap();
        assert_eq!(model, "qwen3");
        assert!(should_auth);
    }

    #[test]
    fn cloud_suffix_with_local_endpoint_is_allowed_without_api_key() {
        let p = OllamaProvider::new(None, None);
        let (model, should_auth) = p.resolve_request_details("qwen3:cloud").unwrap();
        assert_eq!(model, "qwen3:cloud");
        assert!(!should_auth);
    }

    #[test]
    fn cloud_suffix_without_api_key_errors() {
        let p = OllamaProvider::new(Some("https://ollama.com"), None);
        let error = p
            .resolve_request_details("qwen3:cloud")
            .expect_err("cloud suffix should require API key");
        assert!(
            error
                .to_string()
                .contains("requested cloud routing, but no API key is configured")
        );
    }

    #[test]
    fn remote_endpoint_auth_enabled_when_key_present() {
        let p = OllamaProvider::new(Some("https://ollama.com"), Some("ollama-key"));
        let (_model, should_auth) = p.resolve_request_details("qwen3").unwrap();
        assert!(should_auth);
    }

    #[test]
    fn remote_endpoint_with_api_suffix_still_allows_cloud_models() {
        let p = OllamaProvider::new(Some("https://ollama.com/api"), Some("ollama-key"));
        let (model, should_auth) = p.resolve_request_details("qwen3:cloud").unwrap();
        assert_eq!(model, "qwen3");
        assert!(should_auth);
    }

    #[test]
    fn local_endpoint_auth_disabled_even_with_key() {
        let p = OllamaProvider::new(None, Some("ollama-key"));
        let (_model, should_auth) = p.resolve_request_details("llama3").unwrap();
        assert!(!should_auth);
    }

    #[test]
    fn request_omits_think_when_reasoning_not_configured() {
        let provider = OllamaProvider::new(None, None);
        let request = provider.build_chat_request(
            vec![Message {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            }],
            "llama3",
            0.7,
            None,
            32_768,
        );

        let json = serde_json::to_value(request).unwrap();
        assert!(json.get("think").is_none());
    }

    #[test]
    fn request_includes_think_when_reasoning_configured() {
        let provider = OllamaProvider::new_with_reasoning(None, None, Some(false));
        let request = provider.build_chat_request(
            vec![Message {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            }],
            "llama3",
            0.7,
            None,
            32_768,
        );

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json.get("think"), Some(&serde_json::json!(false)));
    }

    #[test]
    fn request_serializes_num_predict_for_a_bounded_reasoning_segment() {
        let provider = OllamaProvider::new_full(
            None,
            None,
            Some(true),
            None,
            None,
            Some(32_768),
            Some(2_048),
        );
        let request = provider.build_chat_request(
            vec![Message {
                role: "user".to_string(),
                content: Some("reason about this task".to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            }],
            "qwen3.5:9b",
            0.2,
            None,
            32_768,
        );

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["options"]["num_predict"], serde_json::json!(2_048));
        // The cap must not silently turn off the model's reasoning mode.
        assert!(provider.reasoning_enabled == Some(true));
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"message":{"role":"assistant","content":"Hello from Ollama!"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "Hello from Ollama!");
    }

    #[test]
    fn length_done_reason_is_a_normal_output_budget_checkpoint() {
        let json = r#"{
            "message":{"role":"assistant","content":"partial answer"},
            "eval_count":2048,
            "done_reason":"length"
        }"#;
        let response: ApiChatResponse = serde_json::from_str(json).unwrap();

        assert!(OllamaProvider::output_budget_exhausted(
            response.done_reason.as_deref()
        ));
        assert!(!OllamaProvider::output_budget_exhausted(Some("stop")));
    }

    #[test]
    fn response_with_empty_content() {
        let json = r#"{"message":{"role":"assistant","content":""}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn parse_content_tool_calls_handles_top_level_tool_object() {
        let provider = OllamaProvider::new(None, None);
        let calls = provider
            .parse_content_tool_calls(r#"{"name":"shell","arguments":{"command":"printf ok"}}"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, r#"{"command":"printf ok"}"#);
    }

    #[test]
    fn parse_content_tool_calls_handles_native_tool_calls_payload() {
        let provider = OllamaProvider::new(None, None);
        let calls = provider.parse_content_tool_calls(
            r#"{"tool_calls":[{"id":"call_1","function":{"name":"file_read","arguments":{"path":"README.md"}}}]}"#,
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments, r#"{"path":"README.md"}"#);
    }

    #[test]
    fn parse_content_tool_calls_handles_json_fence() {
        let provider = OllamaProvider::new(None, None);
        let calls = provider.parse_content_tool_calls(
            "```json\n{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}\n```",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, r#"{"command":"date"}"#);
    }

    #[test]
    fn parse_content_tool_calls_handles_multiple_json_objects() {
        let provider = OllamaProvider::new(None, None);
        let calls = provider.parse_content_tool_calls(
            "{\"name\":\"file_write\",\"arguments\":{\"path\":\"a.txt\",\"content\":\"ok\"}}\n{\"name\":\"file_read\",\"arguments\":{\"path\":\"a.txt\"}}",
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(calls[0].arguments, r#"{"content":"ok","path":"a.txt"}"#);
        assert_eq!(calls[1].name, "file_read");
        assert_eq!(calls[1].arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn parse_content_tool_calls_handles_bracket_payload() {
        let provider = OllamaProvider::new(None, None);
        let calls =
            provider.parse_content_tool_calls(r#"[TOOL_CALLS]shell[ARGS]{"command":"lsusb"}"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, r#"{"command":"lsusb"}"#);
    }

    #[test]
    fn parse_content_tool_calls_handles_function_style_call() {
        let provider = OllamaProvider::new(None, None);
        let calls = provider.parse_content_tool_calls(r#"shell("lsblk")"#);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, r#"{"command":"lsblk"}"#);
    }

    #[test]
    fn parse_content_tool_calls_handles_explicit_tool_header() {
        let provider = OllamaProvider::new(None, None);
        let calls = provider.parse_content_tool_calls("tool: shell\ncommand: lsusb");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, r#"{"command":"lsusb"}"#);
    }

    #[test]
    fn parse_content_tool_response_preserves_text_around_tool_call() {
        let provider = OllamaProvider::new(None, None);
        let (text, calls) = provider.parse_content_tool_response("Running it now.\nshell('lsblk')");
        assert_eq!(text.as_deref(), Some("Running it now."));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, r#"{"command":"lsblk"}"#);
    }

    #[test]
    fn normalize_response_text_rejects_whitespace_only_content() {
        assert_eq!(
            OllamaProvider::normalize_response_text("\n \t".to_string()),
            None
        );
        assert_eq!(
            OllamaProvider::normalize_response_text(" hello ".to_string()),
            Some(" hello ".to_string())
        );
    }

    #[test]
    fn fallback_text_for_empty_content_without_thinking_is_generic() {
        let text = OllamaProvider::fallback_text_for_empty_content("qwen3-coder", None);
        assert!(text.contains("couldn't get a complete response from Ollama"));
    }

    #[test]
    fn fallback_text_for_empty_content_with_thinking_does_not_leak_reasoning() {
        let text = OllamaProvider::fallback_text_for_empty_content(
            "qwen3-coder",
            Some("secret chain of thought"),
        );
        assert!(text.contains("internal reasoning"));
        assert!(!text.contains("secret chain of thought"));
        assert!(!text.contains("I was thinking about this"));
    }

    #[test]
    fn response_with_missing_content_defaults_to_empty() {
        let json = r#"{"message":{"role":"assistant"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn response_with_thinking_field_extracts_content() {
        let json =
            r#"{"message":{"role":"assistant","content":"hello","thinking":"internal reasoning"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "hello");
    }

    #[test]
    fn response_with_tool_calls_parses_correctly() {
        let json = r#"{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_123","function":{"name":"shell","arguments":{"command":"date"}}}]}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
        assert_eq!(resp.message.tool_calls.len(), 1);
        assert_eq!(resp.message.tool_calls[0].function.name, "shell");
    }

    #[test]
    fn extract_tool_name_handles_nested_tool_call() {
        let provider = OllamaProvider::new(None, None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "tool_call".into(),
                arguments: serde_json::json!({
                    "name": "shell",
                    "arguments": {"command": "date"}
                }),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "shell");
        assert_eq!(args.get("command").unwrap(), "date");
    }

    #[test]
    fn extract_tool_name_handles_prefixed_name() {
        let provider = OllamaProvider::new(None, None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "tool.shell".into(),
                arguments: serde_json::json!({"command": "ls"}),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "shell");
        assert_eq!(args.get("command").unwrap(), "ls");
    }

    #[test]
    fn extract_tool_name_handles_normal_call() {
        let provider = OllamaProvider::new(None, None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "file_read".into(),
                arguments: serde_json::json!({"path": "/tmp/test"}),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "file_read");
        assert_eq!(args.get("path").unwrap(), "/tmp/test");
    }

    #[test]
    fn format_tool_calls_produces_valid_json() {
        let provider = OllamaProvider::new(None, None);
        let tool_calls = vec![OllamaToolCall {
            id: Some("call_abc".into()),
            function: OllamaFunction {
                name: "shell".into(),
                arguments: serde_json::json!({"command": "date"}),
            },
        }];

        let formatted = provider.format_tool_calls_for_loop(&tool_calls);
        let parsed: serde_json::Value = serde_json::from_str(&formatted).unwrap();

        assert!(parsed.get("tool_calls").is_some());
        let calls = parsed.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(calls.len(), 1);

        let func = calls[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap(), "shell");
        // arguments should be a string (JSON-encoded)
        assert!(func.get("arguments").unwrap().is_string());
    }

    #[test]
    fn convert_messages_parses_native_assistant_tool_calls() {
        let provider = OllamaProvider::new(None, None);
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: r#"{"content":null,"tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"ls\"}"}]}"#.into(),
        }];

        let converted = provider.convert_messages(&messages);

        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "assistant");
        assert!(converted[0].content.is_none());
        let calls = converted[0]
            .tool_calls
            .as_ref()
            .expect("tool calls expected");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "shell");
        assert_eq!(calls[0].function.arguments.get("command").unwrap(), "ls");
    }

    #[test]
    fn convert_messages_maps_tool_result_call_id_to_tool_name() {
        let provider = OllamaProvider::new(None, None);
        let messages = vec![
            ChatMessage {
                role: "assistant".into(),
                content: r#"{"content":null,"tool_calls":[{"id":"call_7","name":"file_read","arguments":"{\"path\":\"README.md\"}"}]}"#.into(),
            },
            ChatMessage {
                role: "tool".into(),
                content: r#"{"tool_call_id":"call_7","content":"ok"}"#.into(),
            },
        ];

        let converted = provider.convert_messages(&messages);

        assert_eq!(converted.len(), 2);
        assert_eq!(converted[1].role, "tool");
        assert_eq!(converted[1].tool_name.as_deref(), Some("file_read"));
        assert_eq!(converted[1].content.as_deref(), Some("ok"));
        assert!(converted[1].tool_calls.is_none());
    }

    #[test]
    fn convert_messages_extracts_images_from_user_marker() {
        let provider = OllamaProvider::new(None, None);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Inspect this screenshot [IMAGE:data:image/png;base64,abcd==]".into(),
        }];

        let converted = provider.convert_messages(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "user");
        assert_eq!(
            converted[0].content.as_deref(),
            Some("Inspect this screenshot")
        );
        let images = converted[0]
            .images
            .as_ref()
            .expect("images should be present");
        assert_eq!(images, &vec!["abcd==".to_string()]);
    }

    #[test]
    fn capabilities_include_native_tools_and_vision() {
        let provider = OllamaProvider::new(None, None);
        let caps = <OllamaProvider as Provider>::capabilities(&provider);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn api_response_parses_eval_counts() {
        let json = r#"{
            "message": {"content": "Hello", "tool_calls": []},
            "prompt_eval_count": 50,
            "eval_count": 25
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.prompt_eval_count, Some(50));
        assert_eq!(resp.eval_count, Some(25));
    }

    #[test]
    fn api_response_parses_without_eval_counts() {
        let json = r#"{"message": {"content": "Hello", "tool_calls": []}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.prompt_eval_count.is_none());
        assert!(resp.eval_count.is_none());
    }
}
