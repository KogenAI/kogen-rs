use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{RequestContext, ResponseMode, codex_turn_metadata, codex_window_id};

pub(super) fn encode(
    request: &RequestContext,
    mode: ResponseMode,
) -> Result<Vec<u8>, serde_json::Error> {
    let input = input_items(request, mode);
    serde_json::to_vec(&WireBody::new(request, mode, input))
}

fn input_items(request: &RequestContext, mode: ResponseMode) -> Vec<Value> {
    if mode == ResponseMode::Lite {
        let schemas = sorted_tools(&request.tools);
        let tools_id = stable_id(&request.lite_session_id, "additional_tools");
        let shared_id = stable_id(&request.lite_session_id, "shared_instructions");
        let role_id = stable_id(&request.thread_id, "role_instructions");
        let mut items = vec![
            json!({"id": tools_id, "type":"additional_tools", "role":"developer", "tools":schemas}),
            developer_item(shared_id, &request.shared_instructions),
            developer_item(role_id, &request.role_instructions),
        ];
        items.extend(request.input.iter().cloned());
        return items;
    }
    if mode == ResponseMode::Owned && !request.tools.is_empty() {
        let mut items = vec![json!({
            "type":"additional_tools",
            "role":"developer",
            "tools":sorted_tools(&request.tools)
        })];
        items.extend(request.input.iter().cloned());
        items
    } else {
        request.input.clone()
    }
}

fn developer_item(id: String, text: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "developer",
        "content": [{"type":"input_text", "text":text}]
    })
}

fn sorted_tools(tools: &[Value]) -> Vec<Value> {
    let mut tools = tools.to_vec();
    tools.sort_by(|left, right| {
        left.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(
                right
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });
    tools
}

fn stable_id(key: &str, label: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(format!("{key}\0{label}").as_bytes()))
}

fn client_metadata(request: &RequestContext) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("session_id".to_owned(), request.cache_key.clone()),
        ("thread_id".to_owned(), request.thread_id.clone()),
        ("x-codex-window-id".to_owned(), codex_window_id(request)),
        (
            "x-codex-turn-metadata".to_owned(),
            codex_turn_metadata(request),
        ),
    ])
}

struct WireBody<'a> {
    request: &'a RequestContext,
    mode: ResponseMode,
    input: Vec<Value>,
    tools: Vec<Value>,
    reasoning: Value,
    tool_choice: Value,
}

impl<'a> WireBody<'a> {
    fn new(request: &'a RequestContext, mode: ResponseMode, input: Vec<Value>) -> Self {
        let tools = sorted_tools(&request.tools);
        let mut reasoning = serde_json::Map::new();
        reasoning.insert("effort".to_owned(), Value::String(request.effort.clone()));
        if mode == ResponseMode::Lite {
            reasoning.insert("context".to_owned(), Value::String("all_turns".to_owned()));
        } else if mode != ResponseMode::Grok && request.model != "gpt-6-luna" {
            reasoning.insert("summary".to_owned(), Value::String("auto".to_owned()));
        }
        let mut allowed_tools = request.callable_tools.clone();
        allowed_tools.sort();
        let tool_choice = match request.tool_choice.as_str() {
            "auto" if !request.callable_tools.is_empty() => json!({
                "type":"allowed_tools",
                "mode":"auto",
                "tools":allowed_tools.iter().map(|name| json!({"type":"function","name":name})).collect::<Vec<_>>()
            }),
            "auto" => Value::String("none".to_owned()),
            choice => Value::String(choice.to_owned()),
        };
        Self {
            request,
            mode,
            input,
            tools,
            reasoning: Value::Object(reasoning),
            tool_choice,
        }
    }
}

impl Serialize for WireBody<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let lite = self.mode == ResponseMode::Lite;
        let injected = matches!(self.mode, ResponseMode::Injected | ResponseMode::Lite);
        let include_tools = !self.tools.is_empty()
            && matches!(self.mode, ResponseMode::Injected | ResponseMode::Grok);
        let include = injected || self.mode == ResponseMode::Grok;
        let include_cache_key = !self.request.cache_key.is_empty();
        let include_text = self.request.model == "gpt-6-luna";
        let include_client_metadata = self.mode != ResponseMode::Grok;
        let include_cap =
            self.request.development_request && self.request.generation_tokens.is_some();
        let include_controls = self.mode != ResponseMode::Grok;
        let count = 6
            + 2 * usize::from(include_controls)
            + usize::from(include_tools)
            + usize::from(include)
            + usize::from(include_cache_key)
            + usize::from(include_text)
            + usize::from(include_client_metadata)
            + usize::from(include_cap);
        let mut map = serializer.serialize_map(Some(count))?;
        map.serialize_entry("model", &self.request.model)?;
        map.serialize_entry(
            "instructions",
            if lite { "" } else { &self.request.instructions },
        )?;
        if include_tools {
            map.serialize_entry("tools", &self.tools)?;
        }
        map.serialize_entry("reasoning", &self.reasoning)?;
        map.serialize_entry("store", &false)?;
        map.serialize_entry("stream", &true)?;
        if include {
            map.serialize_entry("include", &["reasoning.encrypted_content"])?;
        }
        if include_cache_key {
            map.serialize_entry("prompt_cache_key", &self.request.cache_key)?;
        }
        if include_controls {
            map.serialize_entry("tool_choice", &self.tool_choice)?;
            map.serialize_entry("parallel_tool_calls", &false)?;
        }
        if include_text {
            map.serialize_entry("text", &json!({"verbosity":"low"}))?;
        }
        if include_client_metadata {
            map.serialize_entry("client_metadata", &client_metadata(self.request))?;
        }
        if include_cap {
            map.serialize_entry("max_output_tokens", &self.request.generation_tokens)?;
        }
        map.serialize_entry("input", &self.input)?;
        map.end()
    }
}
