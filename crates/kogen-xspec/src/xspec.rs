mod approve;
mod intent;
mod queue;
mod rebase;
mod recovery;
mod setup_cache;
mod status;
mod temp;

use kogen_core::queue::QueueScheduler;
use serde_json::{Value, json};
use temp::{ApprovalSummary, SourceBytes, TempProject};

pub struct Adapter {
    slice: Slice,
    state: Value,
    project: TempProject,
    queue: QueueScheduler,
    setup_cache: setup_cache::Replay,
    status: kogen_core::status::StatusReplay,
    recovery: kogen_core::recovery::RecoveryModel,
    landing: kogen_core::git::landing::LandingModel,
}

#[derive(Clone, Copy)]
enum Slice {
    Intent,
    Approve,
    Queue,
    SetupCache,
    Status,
    Recovery,
    Rebase,
}

impl Adapter {
    pub fn new(name: &str) -> Result<Self, String> {
        let slice = match name {
            "intent" => Slice::Intent,
            "approve" => Slice::Approve,
            "queue" => Slice::Queue,
            "setup-cache" => Slice::SetupCache,
            "status" => Slice::Status,
            "recovery" => Slice::Recovery,
            "rebase" => Slice::Rebase,
            _ => return Err(format!("unknown private slice `{name}`")),
        };
        let project = TempProject::new()?;
        let setup_cache = setup_cache::Replay::new()?;
        let state = initial_state(slice);
        Ok(Self {
            slice,
            state,
            project,
            queue: QueueScheduler::new(),
            setup_cache,
            status: kogen_core::status::StatusReplay::new(),
            recovery: kogen_core::recovery::RecoveryModel::new(),
            landing: kogen_core::git::landing::LandingModel::new(),
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
                        Slice::Queue => queue::apply(&mut self.queue, &event),
                        Slice::SetupCache => self.setup_cache.apply(&event),
                        Slice::Status => status::apply(&mut self.status, &event),
                        Slice::Recovery => recovery::apply(&mut self.recovery, &event),
                        Slice::Rebase => rebase::apply(&mut self.landing, &event),
                    }
                }
            }
            _ => Err(format!("unknown protocol operation `{op}`")),
        }
    }

    fn reset(&mut self) -> Result<Value, String> {
        if matches!(self.slice, Slice::SetupCache) {
            return self.setup_cache.reset();
        }
        if matches!(self.slice, Slice::Intent | Slice::Approve) {
            self.project.reset()?;
        }
        self.state = initial_state(self.slice);
        self.queue = QueueScheduler::new();
        self.status = kogen_core::status::StatusReplay::new();
        self.recovery = kogen_core::recovery::RecoveryModel::new();
        self.landing = kogen_core::git::landing::LandingModel::new();
        Ok(self.observation())
    }

    fn observation(&self) -> Value {
        match self.slice {
            Slice::Intent => intent::observe(self),
            Slice::Approve => approve::observe(self),
            Slice::Queue => serde_json::to_value(self.queue.observe())
                .expect("queue observations are serializable"),
            Slice::SetupCache => self.setup_cache.observe(),
            Slice::Status => serde_json::to_value(self.status.observe())
                .expect("status observations are serializable"),
            Slice::Recovery => serde_json::to_value(self.recovery.observe())
                .expect("recovery observations are serializable"),
            Slice::Rebase => serde_json::to_value(self.landing.observe())
                .expect("landing observations are serializable"),
        }
    }
}

fn initial_state(slice: Slice) -> Value {
    match slice {
        Slice::Intent => kogen_core::approval::replay::intent_initial(),
        Slice::Approve => kogen_core::approval::replay::approve_initial(),
        Slice::Queue => Value::Null,
        Slice::SetupCache => serde_json::json!({
            "entryCount": 0,
            "work": "",
            "present": false,
            "reused": false,
            "setupRuns": 0,
            "last": "ok",
        }),
        Slice::Status | Slice::Recovery => Value::Null,
        Slice::Rebase => json!(kogen_core::git::landing::LandingModel::new().observe()),
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
