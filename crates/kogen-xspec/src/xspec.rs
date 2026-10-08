mod approve;
mod digest;
mod gate;
mod intent;
mod orchestration;
mod queue;
mod rebase;
mod recovery;
mod session;
mod setup_cache;
mod status;
mod temp;

pub(super) use session::SessionReplay;

use kogen_core::queue::QueueScheduler;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use temp::TempProject;

pub struct Adapter {
    slice: Slice,
    state: Value,
    approval_aliases: BTreeMap<String, ApprovalAlias>,
    intent_hash_aliases: BTreeMap<String, String>,
    project: TempProject,
    project_resets: usize,
    queue: QueueScheduler,
    stream: kogen_core::provider::http::retry::RetryReplay,
    session: SessionReplay,
    setup_cache: setup_cache::Replay,
    status: kogen_core::status::StatusReplay,
    recovery: kogen_core::recovery::RecoveryModel,
    landing: kogen_core::git::landing::LandingModel,
    orchestration: kogen_core::run::orchestration::replay::OrchestrationReplay,
    gate: kogen_core::run::orchestration::GateReplay,
}

#[derive(Clone)]
struct ApprovalAlias {
    sha: String,
    actual_sha: String,
    commit: String,
    base: String,
    actual_commit: String,
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
    Stream,
    Session,
    Orchestration,
    Gate,
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
            "stream" => Slice::Stream,
            "session" => Slice::Session,
            "orchestration" => Slice::Orchestration,
            "gate" => Slice::Gate,
            _ => return Err(format!("unknown private slice `{name}`")),
        };
        let project = TempProject::new()?;
        let setup_cache = setup_cache::Replay::new()?;
        let state = initial_state(slice);
        Ok(Self {
            slice,
            state,
            approval_aliases: BTreeMap::new(),
            intent_hash_aliases: BTreeMap::new(),
            project,
            project_resets: 0,
            queue: QueueScheduler::new(),
            stream: kogen_core::provider::http::retry::RetryReplay::default(),
            session: SessionReplay::default(),
            setup_cache,
            status: kogen_core::status::StatusReplay::new(),
            recovery: kogen_core::recovery::RecoveryModel::new(),
            landing: kogen_core::git::landing::LandingModel::new(),
            orchestration: kogen_core::run::orchestration::replay::OrchestrationReplay::new(),
            gate: kogen_core::run::orchestration::GateReplay::new(),
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
                        Slice::Stream => {
                            self.stream
                                .apply(string(&event, "tag")?, event.get("value"));
                            self.observation()
                        }
                        Slice::Session => {
                            self.session
                                .apply(string(&event, "tag")?, event.get("value"))?;
                            self.observation()
                        }
                        Slice::SetupCache => self.setup_cache.apply(&event),
                        Slice::Status => status::apply(&mut self.status, &event),
                        Slice::Recovery => recovery::apply(&mut self.recovery, &event),
                        Slice::Rebase => rebase::apply(&mut self.landing, &event),
                        Slice::Orchestration => {
                            orchestration::apply(&mut self.orchestration, &event)
                        }
                        Slice::Gate => gate::apply(&mut self.gate, &event),
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
        if matches!(self.slice, Slice::Approve) && self.project_resets == 40 {
            self.project = TempProject::new()?;
            self.project_resets = 0;
        } else if matches!(self.slice, Slice::Intent | Slice::Approve) {
            self.project.reset()?;
            self.project_resets += 1;
        }
        if matches!(self.slice, Slice::Stream) {
            self.stream = kogen_core::provider::http::retry::RetryReplay::default();
        }
        if matches!(self.slice, Slice::Session) {
            self.session.reset()?;
        }
        self.approval_aliases.clear();
        self.intent_hash_aliases.clear();
        self.state = initial_state(self.slice);
        self.queue = QueueScheduler::new();
        self.status = kogen_core::status::StatusReplay::new();
        self.recovery = kogen_core::recovery::RecoveryModel::new();
        self.landing = kogen_core::git::landing::LandingModel::new();
        self.orchestration = kogen_core::run::orchestration::replay::OrchestrationReplay::new();
        self.gate = kogen_core::run::orchestration::GateReplay::new();
        self.observation()
    }

    fn observation(&self) -> Result<Value, String> {
        match self.slice {
            Slice::Intent => intent::observe(self),
            Slice::Approve => approve::observe(self),
            Slice::Queue => Ok(serde_json::to_value(self.queue.observe())
                .expect("queue observations are serializable")),
            Slice::Stream => {
                Ok(serde_json::to_value(&self.stream)
                    .expect("stream observations are serializable"))
            }
            Slice::Session => {
                Ok(serde_json::to_value(&self.session)
                    .expect("session observations are serializable"))
            }
            Slice::SetupCache => Ok(self.setup_cache.observe()),
            Slice::Status => Ok(serde_json::to_value(self.status.observe())
                .expect("status observations are serializable")),
            Slice::Recovery => Ok(serde_json::to_value(self.recovery.observe())
                .expect("recovery observations are serializable")),
            Slice::Rebase => Ok(serde_json::to_value(self.landing.observe())
                .expect("landing observations are serializable")),
            Slice::Orchestration => Ok(orchestration::observe(&self.orchestration)),
            Slice::Gate => Ok(gate::observe(&self.gate)),
        }
    }
}

fn initial_state(slice: Slice) -> Value {
    match slice {
        Slice::Intent => json!({
            "last": "ok", "exit": 0, "did": "", "shown": "", "casTries": 0,
            "life": {}, "refs": {},
        }),
        Slice::Approve => json!({
            "last": "ok", "exit": 0, "sha8": "", "approver": "", "feas": "",
            "bwarn": false, "lwarn": false, "ran": false, "checkRuns": 0,
            "cache": ["", ""], "approvals": {},
        }),
        Slice::Queue => Value::Null,
        Slice::SetupCache => serde_json::json!({
            "entryCount": 0,
            "work": "",
            "present": false,
            "reused": false,
            "setupRuns": 0,
            "last": "ok",
        }),
        Slice::Status
        | Slice::Recovery
        | Slice::Stream
        | Slice::Session
        | Slice::Orchestration
        | Slice::Gate => Value::Null,
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

#[cfg(test)]
mod tests {
    use super::Adapter;
    use serde_json::json;

    #[test]
    fn approval_replay_replaces_accumulated_git_fixture_without_changing_reset_state() {
        let mut adapter = Adapter::new("approve").unwrap();
        let first_root = adapter.project.root().to_path_buf();
        for _ in 0..41 {
            let observation = adapter.handle(json!({"op":"reset"})).unwrap();
            assert_eq!(observation["approvals"], json!({}));
            assert_eq!(observation["last"], "ok");
        }
        assert!(!first_root.exists());
        assert!(adapter.project.root().exists());
        assert_ne!(adapter.project.root(), first_root);
    }
}
