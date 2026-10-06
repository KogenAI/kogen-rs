mod approve;
mod intent;
mod temp;

use serde_json::{Value, json};
use temp::{ApprovalSummary, SourceBytes, TempProject};

pub struct Adapter {
    slice: Slice,
    state: Value,
    project: TempProject,
}

#[derive(Clone, Copy)]
enum Slice {
    Intent,
    Approve,
}

impl Adapter {
    pub fn new(name: &str) -> Result<Self, String> {
        let slice = match name {
            "intent" => Slice::Intent,
            "approve" => Slice::Approve,
            _ => return Err(format!("unknown private slice `{name}`")),
        };
        let project = TempProject::new()?;
        let state = initial_state(slice);
        Ok(Self {
            slice,
            state,
            project,
        })
    }

    pub fn handle(&mut self, request: Value) -> Result<Value, String> {
        let op = request
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| "request requires string field `op`".to_owned())?;
        match op {
            "reset" => self.reset(),
            "apply" => {
                let event = request
                    .get("event")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| "apply request requires an event object".to_owned())?
                    .clone();
                if event.get("tag").and_then(Value::as_str) == Some("Init") {
                    self.reset()
                } else {
                    match self.slice {
                        Slice::Intent => intent::apply(self, &event),
                        Slice::Approve => approve::apply(self, &event),
                    }
                }
            }
            _ => Err(format!("unknown protocol operation `{op}`")),
        }
    }

    fn reset(&mut self) -> Result<Value, String> {
        self.project.reset()?;
        self.state = initial_state(self.slice);
        Ok(self.observation())
    }

    fn observation(&self) -> Value {
        match self.slice {
            Slice::Intent => intent::observe(self),
            Slice::Approve => approve::observe(self),
        }
    }
}

fn initial_state(slice: Slice) -> Value {
    match slice {
        Slice::Intent => kogen_core::approval::replay::intent_initial(),
        Slice::Approve => kogen_core::approval::replay::approve_initial(),
    }
}

fn object<'a>(value: &'a Value, key: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("event requires object field `{key}`"))
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("event requires string field `{key}`"))
}

fn boolean(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("event requires boolean field `{key}`"))
}

fn value_with(event: &Value, value: Value) -> Value {
    let mut event = event.clone();
    if let Some(map) = event.as_object_mut() {
        map.insert("value".to_owned(), value);
    }
    event
}

fn empty_observation() -> Value {
    json!({})
}
