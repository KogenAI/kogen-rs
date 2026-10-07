mod body;

use serde_json::Value;
use std::fmt;
use url::Url;

use crate::provider::auth::RequestCredential;
use crate::provider::session::{
    ConversationBinding, derive_cache_key, derive_lite_session_id, derive_thread_id,
};
use crate::provider::{ProviderErrorKind, ProviderFailure};

#[cfg(test)]
mod tests;

const OWNED_ENDPOINT: &str = "https://api.openai.com/v1/responses";
const INJECTED_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const GROK_ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1/responses";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiMode {
    Responses,
    Lite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseMode {
    Owned,
    /// Owned ChatGPT authentication and body with the Codex backend's
    /// compatibility headers. This is used by the private cache replay tool;
    /// product callers should continue to use `from_auth`.
    OwnedBackend,
    Injected,
    Lite,
    Grok,
}

#[derive(Clone, Debug)]
pub struct WireConfig {
    pub endpoint_override: Option<Url>,
    pub mode: ResponseMode,
    pub supports_generation_cap: bool,
    pub user_agent_version: String,
}

impl WireConfig {
    pub fn from_auth(auth: &RequestCredential, mode: ApiMode) -> Result<Self, ProviderFailure> {
        let mode = match (auth, mode) {
            (RequestCredential::Grok(_), _) => ResponseMode::Grok,
            (RequestCredential::Injected(_), ApiMode::Responses) => ResponseMode::Injected,
            (_, ApiMode::Responses) => ResponseMode::Owned,
            (_, ApiMode::Lite) => ResponseMode::Lite,
        };
        let endpoint_override = std::env::var("KOGEN_PROVIDER_URL")
            .ok()
            .map(|endpoint| {
                Url::parse(&endpoint)
                    .ok()
                    .filter(|url| {
                        matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
                    })
                    .ok_or_else(|| {
                        ProviderFailure::new(
                            ProviderErrorKind::Malformed,
                            "ChatGPT provider URL is invalid.",
                        )
                    })
            })
            .transpose()?;
        Ok(Self {
            endpoint_override,
            mode,
            supports_generation_cap: false,
            user_agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> Url {
        if let Some(endpoint) = &self.endpoint_override {
            return endpoint.clone();
        }
        let raw = if self.mode == ResponseMode::Owned {
            OWNED_ENDPOINT
        } else if self.mode == ResponseMode::Grok {
            GROK_ENDPOINT
        } else {
            INJECTED_ENDPOINT
        };
        Url::parse(raw).expect("constant Responses endpoint")
    }
}

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub model: String,
    pub effort: String,
    pub instructions: String,
    pub shared_instructions: String,
    pub role_instructions: String,
    pub input: Vec<Value>,
    pub tools: Vec<Value>,
    pub callable_tools: Vec<String>,
    pub tool_choice: String,
    pub development_request: bool,
    pub generation_tokens: Option<u64>,
    pub cache_key: String,
    pub thread_id: String,
    pub lite_session_id: String,
    /// Server-provided sticky routing state, scoped to this conversation.
    pub sticky_routing_token: Option<String>,
}

impl RequestContext {
    pub fn for_conversation(
        binding: &ConversationBinding,
        model: impl Into<String>,
        effort: impl Into<String>,
        instructions: impl Into<String>,
        input: Vec<Value>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            model: model.into(),
            effort: effort.into(),
            instructions: instructions.into(),
            shared_instructions: String::new(),
            role_instructions: String::new(),
            input,
            tools: Vec::new(),
            callable_tools: Vec::new(),
            tool_choice: "auto".to_owned(),
            development_request: false,
            generation_tokens: None,
            cache_key: derive_cache_key(&binding.run_dir)?,
            thread_id: derive_thread_id(binding)?,
            lite_session_id: derive_lite_session_id(&binding.run_dir)?,
            sticky_routing_token: None,
        })
    }
}

#[derive(Clone)]
pub struct WireRequest {
    pub endpoint: Url,
    pub mode: ResponseMode,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl fmt::Debug for WireRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<_> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("WireRequest")
            .field("endpoint_host", &self.endpoint.host_str())
            .field("endpoint_port", &self.endpoint.port())
            .field("endpoint_path", &self.endpoint.path())
            .field("header_names", &headers)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl WireRequest {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub fn validate_generation_cap(
    endpoint: &Url,
    mode: ResponseMode,
    model: &str,
    generation_tokens: Option<u64>,
    supports_generation_cap: bool,
) -> Result<(), ProviderFailure> {
    if mode == ResponseMode::Lite && (model != "gpt-6-luna" || generation_tokens.is_some()) {
        return Err(ProviderFailure::new(
            ProviderErrorKind::Unsupported,
            "Unsupported adapter or model-generation cap for this backend.",
        ));
    }
    if generation_tokens.is_some_and(|tokens| !(1..=100_000).contains(&tokens)) {
        return Err(ProviderFailure::new(
            ProviderErrorKind::Unsupported,
            "Model-generation cap must be between 1 and 100000.",
        ));
    }
    if generation_tokens.is_some()
        && endpoint.as_str() != OWNED_ENDPOINT
        && !supports_generation_cap
    {
        return Err(ProviderFailure::new(
            ProviderErrorKind::Unsupported,
            "Model-generation cap is unsupported on this endpoint/adapter.",
        ));
    }
    Ok(())
}

pub fn build_wire_request(
    request: &RequestContext,
    auth: &RequestCredential,
    config: &WireConfig,
) -> Result<WireRequest, ProviderFailure> {
    let mut normalized_request = request.clone();
    if config.mode == ResponseMode::Grok {
        if normalized_request.model.is_empty() {
            normalized_request.model = crate::provider::grok::DEFAULT_MODEL.to_owned();
        }
        if normalized_request.effort.is_empty() {
            normalized_request.effort = crate::provider::grok::DEFAULT_EFFORT.to_owned();
        }
    }
    let request = &normalized_request;
    let endpoint = config.endpoint();
    if config.mode != ResponseMode::Grok
        && (request.cache_key.is_empty() || request.thread_id.is_empty())
    {
        return Err(ProviderFailure::new(
            ProviderErrorKind::Malformed,
            "ChatGPT request is missing its cache or conversation identity.",
        ));
    }
    validate_generation_cap(
        &endpoint,
        config.mode,
        &request.model,
        request.generation_tokens,
        config.supports_generation_cap,
    )?;
    if config.mode == ResponseMode::Lite && !matches!(auth, RequestCredential::Injected(_)) {
        return Err(ProviderFailure::new(
            ProviderErrorKind::Unsupported,
            "Unsupported adapter or model-generation cap for this backend.",
        ));
    }
    if config.mode == ResponseMode::Grok && !matches!(auth, RequestCredential::Grok(_)) {
        return Err(ProviderFailure::new(
            ProviderErrorKind::Login,
            "Grok login is missing or invalid; run `kogen provider login grok`.",
        ));
    }
    let account_id = auth.account_id();
    if matches!(
        config.mode,
        ResponseMode::Injected | ResponseMode::OwnedBackend
    ) && account_id.is_none()
    {
        return Err(ProviderFailure::new(
            ProviderErrorKind::Login,
            "Codex login is missing, invalid, or expired.",
        ));
    }
    let body = body::encode(request, config.mode).map_err(|_| {
        ProviderFailure::new(
            ProviderErrorKind::Malformed,
            if config.mode == ResponseMode::Grok {
                "Could not encode Grok request."
            } else {
                "Could not encode ChatGPT request."
            },
        )
    })?;
    if config.mode == ResponseMode::Grok {
        let mut headers = vec![
            (
                "authorization".to_owned(),
                format!("Bearer {}", auth.access_token()),
            ),
            ("content-type".to_owned(), "application/json".to_owned()),
            ("accept".to_owned(), "text/event-stream".to_owned()),
            ("x-xai-token-auth".to_owned(), "xai-grok-cli".to_owned()),
            (
                "x-authenticateresponse".to_owned(),
                "authenticate-response".to_owned(),
            ),
            ("x-grok-model-override".to_owned(), request.model.clone()),
            ("x-grok-client-identifier".to_owned(), "kogen".to_owned()),
            ("x-grok-client-mode".to_owned(), "headless".to_owned()),
            (
                "x-grok-client-version".to_owned(),
                config.user_agent_version.clone(),
            ),
            (
                "user-agent".to_owned(),
                format!("kogen/{}", config.user_agent_version),
            ),
            ("x-grok-req-id".to_owned(), request_id()),
        ];
        if !request.cache_key.is_empty() {
            headers.push(("x-grok-conv-id".to_owned(), request.cache_key.clone()));
            headers.push(("x-grok-session-id".to_owned(), request.cache_key.clone()));
        }
        return Ok(WireRequest {
            endpoint,
            mode: config.mode,
            headers,
            body,
        });
    }
    let mut headers = vec![
        (
            "authorization".to_owned(),
            format!("Bearer {}", auth.access_token()),
        ),
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), "text/event-stream".to_owned()),
        (
            "user-agent".to_owned(),
            match config.mode {
                ResponseMode::Owned | ResponseMode::OwnedBackend => "kogen/0.1".to_owned(),
                _ => format!("kogen/{}", config.user_agent_version),
            },
        ),
    ];
    if matches!(
        config.mode,
        ResponseMode::Injected | ResponseMode::OwnedBackend | ResponseMode::Lite
    ) {
        headers.push((
            "chatgpt-account-id".to_owned(),
            account_id.unwrap_or_default(),
        ));
        headers.push((
            "openai-beta".to_owned(),
            "responses=experimental".to_owned(),
        ));
    }
    if matches!(
        config.mode,
        ResponseMode::Injected | ResponseMode::OwnedBackend | ResponseMode::Lite
    ) {
        headers.push(("originator".to_owned(), "kogen".to_owned()));
    }
    headers.push(("x-client-request-id".to_owned(), request.thread_id.clone()));
    headers.push(("session-id".to_owned(), request.cache_key.clone()));
    headers.push(("thread-id".to_owned(), request.thread_id.clone()));
    let window_id = codex_window_id(request);
    headers.push(("x-codex-window-id".to_owned(), window_id.clone()));
    headers.push((
        "x-codex-turn-metadata".to_owned(),
        codex_turn_metadata(request),
    ));
    if let Some(token) = &request.sticky_routing_token {
        headers.push(("x-codex-turn-state".to_owned(), token.clone()));
    }
    if config.mode == ResponseMode::Lite {
        headers.push((
            "x-openai-internal-codex-responses-lite".to_owned(),
            "true".to_owned(),
        ));
        headers.push(("session_id".to_owned(), request.lite_session_id.clone()));
    }
    Ok(WireRequest {
        endpoint,
        mode: config.mode,
        headers,
        body,
    })
}

pub(super) fn codex_window_id(request: &RequestContext) -> String {
    format!("{}:0", request.thread_id)
}

pub(super) fn codex_turn_metadata(request: &RequestContext) -> String {
    serde_json::json!({
        "session_id": request.cache_key,
        "thread_id": request.thread_id,
        "window_id": codex_window_id(request),
        "request_kind": "turn",
    })
    .to_string()
}

fn request_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}
