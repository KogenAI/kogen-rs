//! Bounded incremental Responses SSE framing and response assembly.

use serde_json::{Map, Value};

use super::{ModelResponse, ModelToolCall, ModelUsage, ProviderErrorKind, ProviderFailure};

pub const MAX_RESPONSE_BYTES: usize = 16_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamOutputLimits {
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub hard_budget_output_tokens: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamLimitExceeded {
    OutputTokens,
    ReasoningTokens,
    GlobalBudget,
}

impl StreamLimitExceeded {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OutputTokens => "output_tokens_exceeded_512",
            Self::ReasoningTokens => "reasoning_tokens_exceeded_1024",
            Self::GlobalBudget => "global_token_budget_output_allowance_reached",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SseError {
    BodyTooLarge,
}

/// Incremental parser. It retains at most the 16 MiB response limit.
#[derive(Default)]
pub struct SseAssembler {
    consumed: usize,
    line: Vec<u8>,
    data_lines: Vec<Vec<u8>>,
    pending_cr: bool,
    collected_items: Vec<Value>,
    completed: Option<Value>,
    raw_usage: Option<Value>,
    response_model: Option<String>,
    failure: Option<ProviderFailure>,
    malformed: bool,
    finished: bool,
    stream_output_limits: Option<StreamOutputLimits>,
    stream_limit_exceeded: Option<StreamLimitExceeded>,
    output_text_tokens: u64,
    reasoning_text_tokens: u64,
    output_in_word: bool,
    reasoning_in_word: bool,
}

impl SseAssembler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<(), SseError> {
        self.feed_with_output_limits(chunk, None).map(|_| ())
    }

    /// Feed one transport chunk and stop parsing as soon as a configured
    /// streamed-output threshold is crossed. Text is measured with the
    /// replay tool's whitespace tokenizer; provider usage fields take
    /// precedence whenever the stream supplies them.
    pub fn feed_with_output_limits(
        &mut self,
        chunk: &[u8],
        limits: Option<StreamOutputLimits>,
    ) -> Result<Option<StreamLimitExceeded>, SseError> {
        if self.finished {
            return Ok(self.stream_limit_exceeded);
        }
        if self.stream_limit_exceeded.is_some() {
            return Ok(self.stream_limit_exceeded);
        }
        self.stream_output_limits = limits;
        self.consumed = self.consumed.saturating_add(chunk.len());
        if self.consumed > MAX_RESPONSE_BYTES {
            self.malformed = true;
            return Err(SseError::BodyTooLarge);
        }
        for byte in chunk {
            if self.pending_cr {
                self.pending_cr = false;
                self.emit_line();
                if *byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => self.pending_cr = true,
                b'\n' => self.emit_line(),
                _ => self.line.push(*byte),
            }
            if self.stream_limit_exceeded.is_some() {
                break;
            }
        }
        Ok(self.stream_limit_exceeded)
    }

    /// Flush an unterminated final line/frame and assemble the response.
    pub fn finish(&mut self) -> Result<ModelResponse, ProviderFailure> {
        if !self.finished {
            self.finished = true;
            if self.pending_cr {
                self.pending_cr = false;
                self.emit_line();
            } else if !self.line.is_empty() {
                self.emit_line();
            }
            if !self.data_lines.is_empty() {
                self.emit_frame();
            }
        }
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        if self.malformed {
            return Err(malformed("ChatGPT returned a malformed response."));
        }
        let Some(completed) = self.completed.as_ref() else {
            return Err(malformed("ChatGPT response stream did not complete."));
        };
        assemble(completed, &self.collected_items)
    }

    #[must_use]
    pub fn has_items(&self) -> bool {
        !self.collected_items.is_empty()
    }

    #[must_use]
    pub fn collected_items(&self) -> &[Value] {
        &self.collected_items
    }

    /// The provider's unnormalized usage object, including when assembly later
    /// fails after the usage event arrived.
    #[must_use]
    pub fn raw_usage(&self) -> Option<&Value> {
        self.raw_usage.as_ref()
    }

    /// Model name reported by the completed response, when present.
    #[must_use]
    pub fn response_model(&self) -> Option<&str> {
        self.response_model.as_deref()
    }

    fn emit_line(&mut self) {
        if self.line.is_empty() {
            self.emit_frame();
            return;
        }
        if let Some(data) = self.line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            self.data_lines.push(data.to_vec());
        }
        self.line.clear();
    }

    fn emit_frame(&mut self) {
        if self.data_lines.is_empty() {
            return;
        }
        let mut bytes = Vec::new();
        for (index, line) in self.data_lines.drain(..).enumerate() {
            if index > 0 {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(&line);
        }
        if bytes.is_empty() || bytes == b"[DONE]" {
            return;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            self.malformed = true;
            return;
        };
        let Ok(event) = serde_json::from_str::<Value>(text) else {
            self.malformed = true;
            return;
        };
        self.observe_event(event);
    }

    fn observe_event(&mut self, event: Value) {
        let Some(object) = event.as_object() else {
            self.malformed = true;
            return;
        };
        self.observe_response_metadata(object.get("response"));
        if let Some(usage) = object.get("usage") {
            self.raw_usage = Some(usage.clone());
        }
        self.observe_stream_limits(object);
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if object.get("error").is_some_and(|error| !error.is_null())
            || matches!(kind, "error" | "response.failed" | "response.incomplete")
        {
            self.observe_response_metadata(object.get("response"));
            let usage = response_usage(object.get("response"));
            let failure = event_failure(kind, &event, usage);
            if self.failure.is_none() {
                self.failure = Some(failure);
            }
            return;
        }
        match kind {
            "response.output_item.done" => {
                if let Some(item) = object.get("item") {
                    self.collected_items.push(item.clone());
                } else {
                    self.malformed = true;
                }
            }
            "response.completed" => {
                if self.completed.is_some() {
                    self.malformed = true;
                } else if let Some(response) = object.get("response") {
                    self.observe_response_metadata(Some(response));
                    self.completed = Some(response.clone());
                } else {
                    self.malformed = true;
                }
            }
            _ => {}
        }
    }

    fn observe_stream_limits(&mut self, event: &Map<String, Value>) {
        let Some(limits) = self.stream_output_limits else {
            return;
        };
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let delta = event
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "response.output_text.delta" => {
                count_whitespace_tokens(
                    delta,
                    &mut self.output_text_tokens,
                    &mut self.output_in_word,
                );
                if self
                    .output_text_tokens
                    .saturating_add(self.reasoning_text_tokens)
                    >= limits.hard_budget_output_tokens
                {
                    self.stream_limit_exceeded = Some(StreamLimitExceeded::GlobalBudget);
                    return;
                }
                if self.output_text_tokens > limits.output_tokens {
                    self.stream_limit_exceeded = Some(StreamLimitExceeded::OutputTokens);
                    return;
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning.delta" => {
                count_whitespace_tokens(
                    delta,
                    &mut self.reasoning_text_tokens,
                    &mut self.reasoning_in_word,
                );
                if self
                    .output_text_tokens
                    .saturating_add(self.reasoning_text_tokens)
                    >= limits.hard_budget_output_tokens
                {
                    self.stream_limit_exceeded = Some(StreamLimitExceeded::GlobalBudget);
                    return;
                }
                if self.reasoning_text_tokens > limits.reasoning_tokens {
                    self.stream_limit_exceeded = Some(StreamLimitExceeded::ReasoningTokens);
                    return;
                }
            }
            _ => {}
        }

        let usage = event
            .get("response")
            .and_then(|response| response.get("usage"))
            .or_else(|| event.get("usage"));
        let Some(usage) = usage else {
            return;
        };
        if usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .is_some_and(|count| count >= limits.hard_budget_output_tokens)
        {
            self.stream_limit_exceeded = Some(StreamLimitExceeded::GlobalBudget);
            return;
        }
        if usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .is_some_and(|count| count > limits.output_tokens)
        {
            self.stream_limit_exceeded = Some(StreamLimitExceeded::OutputTokens);
            return;
        }
        if usage
            .get("output_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .is_some_and(|count| count > limits.reasoning_tokens)
        {
            self.stream_limit_exceeded = Some(StreamLimitExceeded::ReasoningTokens);
        }
    }

    fn observe_response_metadata(&mut self, response: Option<&Value>) {
        let Some(response) = response else {
            return;
        };
        if let Some(usage) = response.get("usage") {
            self.raw_usage = Some(usage.clone());
        }
        if let Some(model) = response.get("model").and_then(Value::as_str) {
            self.response_model = Some(model.to_owned());
        }
    }
}

fn count_whitespace_tokens(text: &str, count: &mut u64, in_word: &mut bool) {
    for character in text.chars() {
        if character.is_whitespace() {
            *in_word = false;
        } else if !*in_word {
            *count = count.saturating_add(1);
            *in_word = true;
        }
    }
}

fn assemble(response: &Value, collected: &[Value]) -> Result<ModelResponse, ProviderFailure> {
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status == "incomplete" {
        let reason = response
            .get("incomplete_details")
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(ProviderFailure {
            kind: ProviderErrorKind::Incomplete,
            message: format!("Model response incomplete ({reason}); no tool calls were executed."),
            retry_after_ms: None,
            usage: Some(Box::new(ModelUsage::from_response_value(
                response.get("usage"),
            ))),
        });
    }
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "completed" || id.is_empty() {
        return Err(malformed("ChatGPT returned a malformed response."));
    }
    let output = response.get("output").and_then(Value::as_array);
    let raw_items = output
        .filter(|items| !items.is_empty())
        .cloned()
        .unwrap_or_else(|| collected.to_vec());
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for item in &raw_items {
        let Some(item) = item.as_object() else {
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("message") => append_output_text(item, &mut text),
            Some("function_call")
                if item.get("status").and_then(Value::as_str) != Some("in_progress") =>
            {
                tool_calls.push(parse_tool_call(item)?);
            }
            _ => {}
        }
    }
    let usage_value = response.get("usage");
    if let Some(usage) = usage_value
        && let (Some(total), Some(cached)) = (
            usage.get("input_tokens").and_then(Value::as_u64),
            usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64),
        )
        && cached > total
    {
        return Err(malformed("ChatGPT returned invalid token usage."));
    }
    Ok(ModelResponse {
        id: id.to_owned(),
        text,
        tool_calls,
        usage: ModelUsage::from_response_value(usage_value),
        raw_items,
    })
}

fn append_output_text(item: &Map<String, Value>, output: &mut String) {
    if let Some(content) = item.get("content").and_then(Value::as_array) {
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                output.push_str(text);
            }
        }
    }
}

fn parse_tool_call(item: &Map<String, Value>) -> Result<ModelToolCall, ProviderFailure> {
    let id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let Some(arguments) = item.get("arguments") else {
        return Err(malformed("ChatGPT returned malformed tool arguments."));
    };
    let arguments = if let Some(text) = arguments.as_str() {
        serde_json::from_str::<Value>(text).ok()
    } else {
        Some(arguments.clone())
    };
    let Some(arguments) = arguments.filter(Value::is_object) else {
        return Err(malformed("ChatGPT returned malformed tool arguments."));
    };
    if id.is_empty() || name.is_empty() {
        return Err(malformed("ChatGPT returned malformed tool arguments."));
    }
    Ok(ModelToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments,
    })
}

fn event_failure(kind: &str, event: &Value, usage: Option<ModelUsage>) -> ProviderFailure {
    let rendered = event.to_string();
    let lower = rendered.to_ascii_lowercase();
    let error = event
        .get("error")
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("ChatGPT response stream failed.");
    let failure_kind = if kind == "response.incomplete" {
        ProviderErrorKind::Incomplete
    } else if ["usage_limit", "usage limit", "rate_limit", "rate limit"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        ProviderErrorKind::UsageLimit
    } else if lower.contains("overload") {
        ProviderErrorKind::Overload
    } else {
        ProviderErrorKind::Transport
    };
    let message = if failure_kind == ProviderErrorKind::Incomplete {
        let reason = event
            .get("response")
            .and_then(|value| value.get("incomplete_details"))
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        format!("Model response incomplete ({reason}); no tool calls were executed.")
    } else {
        error.to_owned()
    };
    ProviderFailure {
        kind: failure_kind,
        message,
        retry_after_ms: None,
        usage: usage.map(Box::new),
    }
}

fn response_usage(response: Option<&Value>) -> Option<ModelUsage> {
    response.map(|response| ModelUsage::from_response_value(response.get("usage")))
}

fn malformed(message: &str) -> ProviderFailure {
    ProviderFailure::new(ProviderErrorKind::Malformed, message)
}

#[cfg(test)]
#[path = "sse/tests.rs"]
mod tests;
