use kogen_core::provider::auth::{InjectedCredential, RequestCredential};
use kogen_core::provider::http::{RequestContext, ResponseMode, WireConfig, build_wire_request};
use kogen_core::provider::session::{ConversationBinding, ConversationHistory};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReplay {
    pub version: String,
    pub stage: String,
    pub attempt: String,
    pub rung: String,
    pub epoch: String,
    pub epoch_class: String,
    pub model: String,
    pub run_name: String,
    pub affinity_changed: bool,
    pub shared_affinity: bool,
    pub prefixes: BTreeMap<String, String>,
    pub previous: bool,
    pub key_changed: bool,
    pub lite: String,
    pub last: String,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    run_dir: PathBuf,
    #[serde(skip)]
    affinity_dir: PathBuf,
    #[serde(skip)]
    run_number: u64,
    #[serde(skip)]
    expected_history_items: usize,
    #[serde(skip)]
    binding: Option<ConversationBinding>,
    #[serde(skip)]
    history: ConversationHistory,
    #[serde(skip)]
    cache_key: Option<String>,
    #[serde(skip)]
    thread_id: Option<String>,
    #[serde(skip)]
    lite_session_id: Option<String>,
}

impl Default for SessionReplay {
    fn default() -> Self {
        let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("kogen-xspec-session-{}-{id}", std::process::id()));
        Self {
            version: String::new(),
            stage: String::new(),
            attempt: String::new(),
            rung: String::new(),
            epoch: String::new(),
            epoch_class: String::new(),
            model: String::new(),
            run_name: "run-1".to_owned(),
            affinity_changed: false,
            shared_affinity: false,
            prefixes: BTreeMap::new(),
            previous: false,
            key_changed: false,
            lite: String::new(),
            last: "ok".to_owned(),
            root,
            run_dir: PathBuf::new(),
            affinity_dir: PathBuf::new(),
            run_number: 0,
            expected_history_items: 0,
            binding: None,
            history: ConversationHistory::default(),
            cache_key: None,
            thread_id: None,
            lite_session_id: None,
        }
    }
}

impl SessionReplay {
    pub fn reset(&mut self) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        self.run_number = 1;
        self.run_dir = self.root.join(format!("directory-{}", self.run_number));
        fs::create_dir_all(&self.run_dir).map_err(|error| error.to_string())?;
        self.affinity_dir = self.run_dir.clone();
        self.expected_history_items = 0;
        self.version.clear();
        self.stage.clear();
        self.attempt.clear();
        self.rung.clear();
        self.epoch.clear();
        self.epoch_class.clear();
        self.model.clear();
        self.run_name = "run-1".to_owned();
        self.affinity_changed = false;
        self.shared_affinity = false;
        self.prefixes.clear();
        self.previous = false;
        self.key_changed = false;
        self.lite.clear();
        self.last = "ok".to_owned();
        self.binding = None;
        self.history = ConversationHistory::default();
        self.cache_key = Some(
            kogen_core::provider::session::derive_cache_key(&self.run_dir)
                .map_err(|error| error.to_string())?,
        );
        self.thread_id = None;
        self.lite_session_id = Some(
            kogen_core::provider::session::derive_lite_session_id(&self.run_dir)
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }

    pub fn apply(&mut self, tag: &str, value: Option<&Value>) -> Result<(), String> {
        match tag {
            "Init" => self.reset(),
            "Bind" => self.bind(value),
            "Turn" => self.touch("turn"),
            "Repair" => self.touch("repair"),
            "Model" => self.set_model(value),
            "Stage" => self.set_stage(value),
            "Attempt" => self.set_attempt(value),
            "Rung" => self.set_rung(value),
            "Epoch" => self.set_epoch(value),
            "Accept" => self.accept(value),
            "Previous" => self.previous_response(),
            "Lite" => self.use_lite(),
            "NewRun" => self.new_run(value),
            "AffinityScope" => self.set_affinity_scope(value),
            "Prefix" => self.observe_prefix(value),
            _ => Err(format!("unknown session event {tag}")),
        }
    }

    fn bind(&mut self, value: Option<&Value>) -> Result<(), String> {
        let Some(value) = value else {
            self.last = "bad_bind".to_owned();
            return Ok(());
        };
        let stage = string_field(value, "stage").unwrap_or_default();
        let raw_attempt = string_field(value, "attempt").unwrap_or_default();
        let raw_rung = string_field(value, "rung").unwrap_or_default();
        if !known_stage(stage) || !known_attempt(raw_attempt) || !known_rung(raw_rung) {
            self.last = "bad_bind".to_owned();
            return Ok(());
        }
        let attempt = if raw_attempt.is_empty() {
            "builder"
        } else {
            raw_attempt
        };
        let rung = if raw_rung.is_empty() {
            attempt
        } else {
            raw_rung
        };
        let previous = self.thread_id.clone();
        let was_bound = self.binding.is_some();
        self.history = ConversationHistory::default();
        self.expected_history_items = 0;
        let mut binding = ConversationBinding::new(&self.run_dir, stage);
        binding.attempt = attempt.to_owned();
        binding.rung = rung.to_owned();
        self.binding = Some(binding);
        self.version = "v2".to_owned();
        self.stage = stage.to_owned();
        self.attempt = attempt.to_owned();
        self.rung = rung.to_owned();
        self.epoch = "initial".to_owned();
        self.epoch_class = "initial".to_owned();
        self.update_identity(previous, was_bound)
    }

    fn touch(&mut self, text: &str) -> Result<(), String> {
        if self.binding.is_none() {
            self.last = "not_bound".to_owned();
            return Ok(());
        }
        let prefix = self.history.items().to_vec();
        self.history.append_user(text);
        self.expected_history_items = self.expected_history_items.saturating_add(1);
        if self.history.items().get(..prefix.len()) != Some(prefix.as_slice()) {
            return Err("production session history rewrote an existing request item".to_owned());
        }
        self.key_changed = false;
        self.affinity_changed = false;
        self.last = "ok".to_owned();
        self.validate_wire(false)
    }

    fn set_model(&mut self, value: Option<&Value>) -> Result<(), String> {
        if self.binding.is_none() {
            self.last = "not_bound".to_owned();
            return Ok(());
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some(name @ ("luna" | "sol")) => {
                self.model = name.to_owned();
                self.key_changed = false;
                self.affinity_changed = false;
                self.last = "ok".to_owned();
                self.validate_wire(false)
            }
            _ => {
                self.last = "bad_model".to_owned();
                Ok(())
            }
        }
    }

    fn set_stage(&mut self, value: Option<&Value>) -> Result<(), String> {
        if self.binding.is_none() {
            self.last = "not_bound".to_owned();
            return Ok(());
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some(name) if known_stage(name) => {
                let previous = self.thread_id.clone();
                self.stage = name.to_owned();
                self.binding.as_mut().expect("binding was checked").stage = name.to_owned();
                self.update_identity(previous, true)
            }
            _ => {
                self.last = "bad_bind".to_owned();
                Ok(())
            }
        }
    }

    fn set_attempt(&mut self, value: Option<&Value>) -> Result<(), String> {
        if self.binding.is_none() {
            self.last = "not_bound".to_owned();
            return Ok(());
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some(name) if known_real_attempt(name) => {
                let previous = self.thread_id.clone();
                self.attempt = name.to_owned();
                self.binding.as_mut().expect("binding was checked").attempt = name.to_owned();
                self.update_identity(previous, true)
            }
            _ => {
                self.last = "bad_bind".to_owned();
                Ok(())
            }
        }
    }

    fn set_rung(&mut self, value: Option<&Value>) -> Result<(), String> {
        if self.binding.is_none() {
            self.last = "not_bound".to_owned();
            return Ok(());
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some(name @ ("1" | "2" | "builder" | "fresh-1")) => {
                let previous = self.thread_id.clone();
                self.rung = name.to_owned();
                self.binding.as_mut().expect("binding was checked").rung = name.to_owned();
                self.update_identity(previous, true)
            }
            _ => {
                self.last = "bad_bind".to_owned();
                Ok(())
            }
        }
    }

    fn set_epoch(&mut self, value: Option<&Value>) -> Result<(), String> {
        if self.binding.is_none() {
            self.last = "not_bound".to_owned();
            return Ok(());
        }
        let Some(name) = string_field(value.unwrap_or(&Value::Null), "name") else {
            self.last = "bad_epoch".to_owned();
            return Ok(());
        };
        let (epoch, class) = match name {
            "mutation-advice" => ("mutation-advice", "mutation-advice"),
            "summarizer" => ("checkpoint-1", "checkpoint"),
            _ => {
                self.last = "bad_epoch".to_owned();
                return Ok(());
            }
        };
        let previous = self.thread_id.clone();
        self.epoch = epoch.to_owned();
        self.epoch_class = class.to_owned();
        self.binding.as_mut().expect("binding was checked").epoch = epoch.to_owned();
        self.update_identity(previous, true)
    }

    fn accept(&mut self, value: Option<&Value>) -> Result<(), String> {
        if self.binding.is_none() {
            self.last = "not_bound".to_owned();
            return Ok(());
        }
        if value
            .and_then(|value| value.get("ok"))
            .and_then(Value::as_bool)
            != Some(true)
        {
            self.last = "no_epoch".to_owned();
            return Ok(());
        }
        let previous = self.thread_id.clone();
        self.epoch = "digest".to_owned();
        self.epoch_class = "checkpoint".to_owned();
        self.binding.as_mut().expect("binding was checked").epoch = "digest".to_owned();
        self.update_identity(previous, true)
    }

    fn previous_response(&mut self) -> Result<(), String> {
        if self.binding.is_some() {
            self.validate_wire(false)?;
        }
        self.last = "never_sent".to_owned();
        Ok(())
    }

    fn use_lite(&mut self) -> Result<(), String> {
        let Some(binding) = self.binding.as_ref() else {
            self.last = "not_bound".to_owned();
            return Ok(());
        };
        self.lite_session_id = Some(
            binding
                .lite_session_id()
                .map_err(|error| error.to_string())?,
        );
        self.lite = "v1".to_owned();
        self.key_changed = false;
        self.affinity_changed = false;
        self.last = "ok".to_owned();
        self.validate_wire(true)
    }

    fn new_run(&mut self, value: Option<&Value>) -> Result<(), String> {
        let Some(name) = string_field(value.unwrap_or(&Value::Null), "name") else {
            self.last = "bad_run".to_owned();
            return Ok(());
        };
        if name.is_empty() || name == self.run_name {
            self.last = "bad_run".to_owned();
            return Ok(());
        }
        let previous_cache = self
            .cache_key
            .clone()
            .ok_or_else(|| "session cache affinity was not initialized".to_owned())?;
        let previous_lite = self
            .lite_session_id
            .clone()
            .ok_or_else(|| "Lite session identity was not initialized".to_owned())?;
        self.run_number = self.run_number.saturating_add(1);
        self.run_dir = self.root.join(format!("directory-{}", self.run_number));
        fs::create_dir_all(&self.run_dir).map_err(|error| error.to_string())?;
        self.affinity_dir = if self.shared_affinity {
            let shared = self.root.join("shared-affinity");
            fs::create_dir_all(&shared).map_err(|error| error.to_string())?;
            shared
        } else {
            self.run_dir.clone()
        };
        let next_cache = kogen_core::provider::session::derive_cache_key(&self.affinity_dir)
            .map_err(|error| error.to_string())?;
        let next_lite = kogen_core::provider::session::derive_lite_session_id(&self.run_dir)
            .map_err(|error| error.to_string())?;
        if next_lite == previous_lite {
            return Err("production Lite session identity did not change across runs".to_owned());
        }
        self.version.clear();
        self.stage.clear();
        self.attempt.clear();
        self.rung.clear();
        self.epoch.clear();
        self.epoch_class.clear();
        self.model.clear();
        self.run_name = name.to_owned();
        self.affinity_changed = previous_cache != next_cache;
        self.previous = false;
        self.key_changed = false;
        self.lite.clear();
        self.last = "ok".to_owned();
        self.binding = None;
        self.history = ConversationHistory::default();
        self.expected_history_items = 0;
        self.cache_key = Some(next_cache);
        self.thread_id = None;
        self.lite_session_id = Some(next_lite);
        Ok(())
    }

    fn set_affinity_scope(&mut self, value: Option<&Value>) -> Result<(), String> {
        if self.binding.is_some() {
            self.last = "already_bound".to_owned();
            return Ok(());
        }
        let Some(shared) = value
            .and_then(|value| value.get("shared"))
            .and_then(Value::as_bool)
        else {
            self.last = "bad_affinity".to_owned();
            return Ok(());
        };
        self.shared_affinity = shared;
        self.affinity_dir = if shared {
            let shared = self.root.join("shared-affinity");
            fs::create_dir_all(&shared).map_err(|error| error.to_string())?;
            shared
        } else {
            self.run_dir.clone()
        };
        self.cache_key = Some(
            kogen_core::provider::session::derive_cache_key(&self.affinity_dir)
                .map_err(|error| error.to_string())?,
        );
        self.affinity_changed = false;
        self.last = "ok".to_owned();
        Ok(())
    }

    fn observe_prefix(&mut self, value: Option<&Value>) -> Result<(), String> {
        let Some(value) = value else {
            self.last = "bad_prefix".to_owned();
            return Ok(());
        };
        let mut fields = Vec::with_capacity(5);
        for field in ["provider", "model", "adapter", "prompt", "bytes"] {
            let Some(text) = string_field(value, field) else {
                self.last = "bad_prefix".to_owned();
                return Ok(());
            };
            if text.is_empty() {
                self.last = "bad_prefix".to_owned();
                return Ok(());
            }
            fields.push(text);
        }
        let namespace = tuple_key(&fields[..4]);
        let observed = self.production_instructions(fields[1], fields[4])?;
        if self
            .prefixes
            .get(&namespace)
            .is_some_and(|old| old != &observed)
        {
            self.last = "static_prefix_changed".to_owned();
            return Ok(());
        }
        self.prefixes.insert(namespace, observed);
        self.last = "ok".to_owned();
        Ok(())
    }

    fn update_identity(
        &mut self,
        previous_thread: Option<String>,
        was_bound: bool,
    ) -> Result<(), String> {
        let binding = self
            .binding
            .as_ref()
            .ok_or_else(|| "session binding disappeared".to_owned())?;
        let cache_key = kogen_core::provider::session::derive_cache_key(&self.affinity_dir)
            .map_err(|error| error.to_string())?;
        let thread_id = binding.thread_id().map_err(|error| error.to_string())?;
        self.affinity_changed = self
            .cache_key
            .as_deref()
            .is_some_and(|old| old != cache_key);
        self.key_changed = was_bound && previous_thread.as_deref() != Some(thread_id.as_str());
        self.cache_key = Some(cache_key);
        self.thread_id = Some(thread_id);
        self.last = "ok".to_owned();
        self.validate_wire(false)
    }

    fn validate_wire(&self, lite: bool) -> Result<(), String> {
        let Some(binding) = self.binding.as_ref() else {
            return Ok(());
        };
        self.production_wire(binding, lite, "xspec session replay", None)
            .map(|_| ())
    }

    fn production_instructions(&self, model: &str, instructions: &str) -> Result<String, String> {
        let binding = self
            .binding
            .clone()
            .unwrap_or_else(|| ConversationBinding::new(&self.run_dir, "develop"));
        let body = self.production_wire(&binding, false, instructions, Some(model))?;
        let serialized = body
            .get("instructions")
            .and_then(Value::as_str)
            .ok_or_else(|| "production wire body omitted request instructions".to_owned())?;
        let context =
            RequestContext::for_conversation(&binding, model, "medium", instructions, Vec::new())
                .map_err(|error| error.to_string())?;
        if serialized != context.instructions {
            return Err("production wire changed the reusable instructions".to_owned());
        }
        // The session slice names the role-specific reusable bytes; the
        // universal system prelude is verified in the serialized request.
        Ok(context.role_instructions)
    }

    fn production_wire(
        &self,
        binding: &ConversationBinding,
        lite: bool,
        instructions: &str,
        model_override: Option<&str>,
    ) -> Result<Value, String> {
        let model = if lite || self.model != "sol" {
            "gpt-6-luna"
        } else {
            "gpt-6.1-sol"
        };
        let mut context = RequestContext::for_conversation(
            binding,
            model_override.unwrap_or(model),
            "medium",
            instructions,
            self.history.items().to_vec(),
        )
        .map_err(|error| error.to_string())?;
        context.cache_key = kogen_core::provider::session::derive_cache_key(&self.affinity_dir)
            .map_err(|error| error.to_string())?;
        let credential = RequestCredential::Injected(InjectedCredential {
            access_token: "xspec-injected-token".to_owned(),
            account_id: "xspec-account".to_owned(),
            expires_at: i64::MAX,
        });
        let config = WireConfig {
            endpoint_override: None,
            mode: if lite {
                ResponseMode::Lite
            } else {
                ResponseMode::Injected
            },
            supports_generation_cap: false,
            user_agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        let wire = build_wire_request(&context, &credential, &config)
            .map_err(|failure| failure.message)?;
        if wire.header("session-id") != Some(context.cache_key.as_str())
            || wire.header("thread-id") != Some(context.thread_id.as_str())
        {
            return Err(
                "production session headers do not match derived conversation identity".to_owned(),
            );
        }
        if lite && wire.header("session_id") != Some(context.lite_session_id.as_str()) {
            return Err(
                "production Lite session header does not match the run session id".to_owned(),
            );
        }
        let body: Value = serde_json::from_slice(&wire.body)
            .map_err(|error| format!("production request body is not JSON: {error}"))?;
        let input = body
            .get("input")
            .and_then(Value::as_array)
            .ok_or_else(|| "production request body omitted its input array".to_owned())?;
        let expected_input_items = self.expected_history_items + if lite { 3 } else { 0 };
        if input.len() != expected_input_items {
            return Err(format!(
                "production request history contains {} items, expected {}",
                input.len(),
                expected_input_items
            ));
        }
        if body.get("previous_response_id").is_some() {
            return Err("production request unexpectedly included previous_response_id".to_owned());
        }
        Ok(body)
    }
}

impl Drop for SessionReplay {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn string_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

fn known_stage(name: &str) -> bool {
    matches!(name, "develop" | "plan")
}

fn known_attempt(name: &str) -> bool {
    matches!(name, "" | "builder" | "fresh-1" | "fresh-2" | "escalation")
}

fn known_real_attempt(name: &str) -> bool {
    matches!(name, "builder" | "fresh-1" | "fresh-2" | "escalation")
}

fn known_rung(name: &str) -> bool {
    matches!(name, "" | "builder" | "1" | "2" | "fresh-1")
}

fn tuple_key(parts: &[&str]) -> String {
    format!(
        "[{}]",
        parts
            .iter()
            .map(|part| format!("'{}'", part.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
