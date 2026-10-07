//! Production session-identity transition used by the Quint replay adapter.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    pub shared_affinity: bool,
    pub prefixes: std::collections::BTreeMap<String, String>,
    pub affinity_changed: bool,
    pub previous: bool,
    pub key_changed: bool,
    pub lite: String,
    pub last: String,
}

impl Default for SessionReplay {
    fn default() -> Self {
        Self {
            version: String::new(),
            stage: String::new(),
            attempt: String::new(),
            rung: String::new(),
            epoch: String::new(),
            epoch_class: String::new(),
            model: String::new(),
            run_name: "run-1".to_owned(),
            shared_affinity: false,
            prefixes: Default::default(),
            affinity_changed: false,
            previous: false,
            key_changed: false,
            lite: String::new(),
            last: "ok".to_owned(),
        }
    }
}

impl SessionReplay {
    pub fn apply(&mut self, tag: &str, value: Option<&Value>) {
        match tag {
            "Init" => *self = Self::default(),
            "Bind" => self.bind(value),
            "Turn" | "Repair" => self.touch(),
            "Model" => self.set_model(value),
            "Stage" => self.set_stage(value),
            "Attempt" => self.set_attempt(value),
            "Rung" => self.set_rung(value),
            "Epoch" => self.set_epoch(value),
            "Accept" => self.accept(value),
            "Previous" => self.last = "never_sent".to_owned(),
            "Lite" => {
                self.lite = "v1".to_owned();
                self.key_changed = false;
                self.affinity_changed = false;
                self.last = "ok".to_owned();
            }
            "NewRun" => self.new_run(value),
            "AffinityScope" => {
                if self.is_bound() {
                    self.last = "already_bound".to_owned();
                } else {
                    self.shared_affinity = value
                        .and_then(|value| value.get("shared"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    self.affinity_changed = false;
                    self.last = "ok".to_owned();
                }
            }
            "Prefix" => self.prefix(value),
            _ => self.last = "bad_event".to_owned(),
        }
    }

    fn bind(&mut self, value: Option<&Value>) {
        let Some(value) = value else {
            self.last = "bad_bind".to_owned();
            return;
        };
        let stage = string_field(value, "stage").unwrap_or_default();
        let raw_attempt = string_field(value, "attempt").unwrap_or_default();
        let raw_rung = string_field(value, "rung").unwrap_or_default();
        if !known_stage(stage) || !known_attempt(raw_attempt) || !known_rung(raw_rung) {
            self.last = "bad_bind".to_owned();
            return;
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
        let was_bound = self.is_bound();
        let same = self.stage == stage
            && self.attempt == attempt
            && self.rung == rung
            && self.epoch == "initial";
        self.version = "v2".to_owned();
        self.stage = stage.to_owned();
        self.attempt = attempt.to_owned();
        self.rung = rung.to_owned();
        self.epoch = "initial".to_owned();
        self.epoch_class = "initial".to_owned();
        self.key_changed = was_bound && !same;
        self.affinity_changed = false;
        self.last = "ok".to_owned();
    }

    fn touch(&mut self) {
        if !self.is_bound() {
            self.last = "not_bound".to_owned();
        } else {
            self.key_changed = false;
            self.affinity_changed = false;
            self.last = "ok".to_owned();
        }
    }

    fn set_model(&mut self, value: Option<&Value>) {
        if !self.is_bound() {
            self.last = "not_bound".to_owned();
            return;
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some("luna" | "sol") => {
                self.model = string_field(value.unwrap(), "name").unwrap().to_owned();
                self.key_changed = false;
                self.affinity_changed = false;
                self.last = "ok".to_owned();
            }
            _ => self.last = "bad_model".to_owned(),
        }
    }

    fn set_stage(&mut self, value: Option<&Value>) {
        if !self.is_bound() {
            self.last = "not_bound".to_owned();
            return;
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some(name) if known_stage(name) => {
                self.key_changed = self.stage != name;
                self.stage = name.to_owned();
                self.affinity_changed = false;
                self.last = "ok".to_owned();
            }
            _ => self.last = "bad_bind".to_owned(),
        }
    }

    fn set_attempt(&mut self, value: Option<&Value>) {
        if !self.is_bound() {
            self.last = "not_bound".to_owned();
            return;
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some(name) if known_real_attempt(name) => {
                self.key_changed = self.attempt != name;
                self.attempt = name.to_owned();
                self.affinity_changed = false;
                self.last = "ok".to_owned();
            }
            _ => self.last = "bad_bind".to_owned(),
        }
    }

    fn set_rung(&mut self, value: Option<&Value>) {
        if !self.is_bound() {
            self.last = "not_bound".to_owned();
            return;
        }
        match string_field(value.unwrap_or(&Value::Null), "name") {
            Some(name @ ("1" | "2" | "builder" | "fresh-1")) => {
                self.key_changed = self.rung != name;
                self.rung = name.to_owned();
                self.affinity_changed = false;
                self.last = "ok".to_owned();
            }
            _ => self.last = "bad_bind".to_owned(),
        }
    }

    fn set_epoch(&mut self, value: Option<&Value>) {
        if !self.is_bound() {
            self.last = "not_bound".to_owned();
            return;
        }
        let Some(name) = string_field(value.unwrap_or(&Value::Null), "name") else {
            self.last = "bad_epoch".to_owned();
            return;
        };
        let (epoch, class) = match name {
            "mutation-advice" => ("mutation-advice", "mutation-advice"),
            "summarizer" => ("checkpoint-1", "checkpoint"),
            _ => {
                self.last = "bad_epoch".to_owned();
                return;
            }
        };
        self.key_changed = self.epoch != epoch;
        self.epoch = epoch.to_owned();
        self.epoch_class = class.to_owned();
        self.affinity_changed = false;
        self.last = "ok".to_owned();
    }

    fn accept(&mut self, value: Option<&Value>) {
        if !self.is_bound() {
            self.last = "not_bound".to_owned();
            return;
        }
        if value
            .and_then(|value| value.get("ok"))
            .and_then(Value::as_bool)
            != Some(true)
        {
            self.last = "no_epoch".to_owned();
            return;
        }
        self.key_changed = self.epoch != "digest";
        self.epoch = "digest".to_owned();
        self.epoch_class = "checkpoint".to_owned();
        self.affinity_changed = false;
        self.last = "ok".to_owned();
    }

    fn new_run(&mut self, value: Option<&Value>) {
        let Some(name) = string_field(value.unwrap_or(&Value::Null), "name") else {
            self.last = "bad_run".to_owned();
            return;
        };
        if name.is_empty() || name == self.run_name {
            self.last = "bad_run".to_owned();
            return;
        }
        *self = Self {
            run_name: name.to_owned(),
            affinity_changed: !self.shared_affinity,
            shared_affinity: self.shared_affinity,
            prefixes: self.prefixes.clone(),
            ..Self::default()
        };
    }

    fn prefix(&mut self, value: Option<&Value>) {
        let value = value.unwrap_or(&Value::Null);
        let fields = ["provider", "model", "adapter", "prompt", "bytes"]
            .map(|key| string_field(value, key).unwrap_or_default());
        if fields.iter().any(|field| field.is_empty()) {
            self.last = "bad_prefix".to_owned();
            return;
        }
        let namespace = format!(
            "['{}', '{}', '{}', '{}']",
            fields[0], fields[1], fields[2], fields[3]
        );
        if self
            .prefixes
            .get(&namespace)
            .is_some_and(|bytes| bytes != fields[4])
        {
            self.last = "static_prefix_changed".to_owned();
            return;
        }
        self.prefixes.insert(namespace, fields[4].to_owned());
        self.last = "ok".to_owned();
    }

    fn is_bound(&self) -> bool {
        self.version == "v2"
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
