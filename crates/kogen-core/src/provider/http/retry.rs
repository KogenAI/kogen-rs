//! Pure retry and fallback transitions shared by requests and xspec.

use serde::{Deserialize, Serialize};
use serde_json::Value;

const PAUSE_MS: u64 = 300_000;
const PAUSE_CAP_MS: u64 = 86_400_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryReplay {
    pub phase: String,
    pub mode: String,
    pub role: String,
    pub model: String,
    pub fallback_on: bool,
    pub bounded: bool,
    pub wall: u64,
    pub attempt: u32,
    pub overloads: u32,
    pub refreshed: bool,
    pub waited: u64,
    pub decision: String,
    pub delay: u64,
    pub reason: String,
    pub continued: bool,
    pub queued: bool,
    pub exit: i32,
    pub checkpoint: String,
    pub continuations: u32,
    pub last: String,
    pub failed: bool,
}

impl Default for RetryReplay {
    fn default() -> Self {
        Self {
            phase: "idle".to_owned(),
            mode: String::new(),
            role: String::new(),
            model: String::new(),
            fallback_on: false,
            bounded: false,
            wall: 0,
            attempt: 0,
            overloads: 0,
            refreshed: false,
            waited: 0,
            decision: String::new(),
            delay: 0,
            reason: String::new(),
            continued: false,
            queued: false,
            exit: 0,
            checkpoint: String::new(),
            continuations: 0,
            last: "ok".to_owned(),
            failed: false,
        }
    }
}

impl RetryReplay {
    pub fn apply(&mut self, tag: &str, value: Option<&Value>) {
        match tag {
            "Init" => *self = Self::default(),
            "Open" => self.open(value),
            "Result" => self.result(value),
            "Checkpoint" => self.checkpoint(value),
            "SetWaited" => self.set_waited(value),
            _ => self.last = "bad_event".to_owned(),
        }
    }

    fn open(&mut self, value: Option<&Value>) {
        if self.phase == "open" {
            self.last = "bad_open".to_owned();
            return;
        }
        let Some(value) = value else {
            self.last = "bad_open".to_owned();
            return;
        };
        let role = str_field(value, "role").unwrap_or_default();
        let model = str_field(value, "model").unwrap_or_default();
        let mode = str_field(value, "mode").unwrap_or_default();
        let fallback = bool_field(value, "fallbackOn");
        let bounded = bool_field(value, "bounded");
        let Some(wall) = value.get("wall").and_then(Value::as_u64) else {
            self.last = "bad_open".to_owned();
            return;
        };
        if !matches!(role, "builder" | "planner")
            || !matches!(model, "luna" | "sol")
            || !matches!(mode, "build" | "shape")
        {
            self.last = "bad_open".to_owned();
            return;
        }
        self.phase = "open".to_owned();
        self.mode = mode.to_owned();
        self.role = role.to_owned();
        self.model = model.to_owned();
        self.fallback_on = fallback;
        self.bounded = bounded;
        self.wall = wall;
        self.attempt = 1;
        self.overloads = 0;
        self.refreshed = false;
        self.decision.clear();
        self.delay = 0;
        self.reason.clear();
        self.continued = false;
        self.checkpoint.clear();
        self.queued = true;
        self.exit = 0;
        self.last = "ok".to_owned();
        self.failed = false;
    }

    fn result(&mut self, value: Option<&Value>) {
        if self.phase != "open" {
            self.last = "not_open".to_owned();
            return;
        }
        let Some(value) = value else {
            self.last = "bad_result".to_owned();
            return;
        };
        let kind = str_field(value, "kind").unwrap_or_default();
        let items = bool_field(value, "items");
        let kind = classify_kind(kind, items);
        if kind.is_empty() {
            self.last = "bad_result".to_owned();
        } else if kind == "ok" {
            self.phase = "idle".to_owned();
            self.decision = "success".to_owned();
            self.delay = 0;
            self.reason.clear();
            self.continued = false;
            self.exit = 0;
            self.last = "ok".to_owned();
        } else if kind == "incomplete" {
            self.phase = "idle".to_owned();
            self.decision = "incomplete".to_owned();
            self.delay = 0;
            self.reason.clear();
            self.continued = false;
            self.exit = 0;
            self.last = "ok".to_owned();
        } else if matches!(kind.as_str(), "usage_limit" | "login") {
            let retry_after_ms = value.get("retryAfterMs").and_then(Value::as_u64);
            self.credential(&kind, retry_after_ms);
        } else {
            self.retry(&kind, items);
        }
    }

    fn credential(&mut self, kind: &str, retry_after_ms: Option<u64>) {
        let reason = provider_reason(kind);
        if !self.refreshed {
            self.refreshed = true;
            self.decision = "refresh".to_owned();
            self.delay = 0;
            self.reason = reason;
            self.continued = false;
            self.phase = "open".to_owned();
            self.exit = 0;
            self.last = "ok".to_owned();
        } else {
            let wait = retry_after_ms.unwrap_or(PAUSE_MS);
            if self.mode == "shape" || self.waited.saturating_add(wait) > PAUSE_CAP_MS {
                self.halt(&reason, 4);
            } else {
                self.waited += wait;
                self.decision = "pause".to_owned();
                self.delay = wait;
                self.reason = reason;
                self.continued = false;
                self.phase = "idle".to_owned();
                self.last = "ok".to_owned();
            }
        }
    }

    fn retry(&mut self, kind: &str, items: bool) {
        let streak = if kind == "overload" {
            self.overloads.saturating_add(1)
        } else {
            0
        };
        let switching =
            streak >= 2 && self.fallback_on && self.role == "builder" && self.model != "sol";
        let budget_class = matches!(kind, "timeout" | "stall" | "transport")
            || (kind == "overload" && !self.fallback_on);
        let attempts_left = (self.bounded && budget_class) || self.attempt < 4;
        let delay = if switching {
            0
        } else {
            retry_ceiling(self.attempt)
        };
        let cannot_pay = self.bounded && self.wall <= delay;
        if !attempts_left || cannot_pay {
            self.halt(&provider_reason(kind), 4);
        } else if switching {
            self.phase = "open".to_owned();
            self.model = "sol".to_owned();
            self.overloads = 0;
            self.attempt = self.attempt.saturating_add(1);
            self.decision = "switch".to_owned();
            self.delay = 0;
            self.reason = provider_reason(kind);
            self.continued = false;
            self.exit = 0;
            self.last = "ok".to_owned();
        } else {
            self.phase = "open".to_owned();
            self.overloads = streak;
            self.attempt = self.attempt.saturating_add(1);
            self.decision = "retry".to_owned();
            self.delay = delay;
            self.reason = provider_reason(kind);
            self.continued = items && continuable(kind);
            self.exit = 0;
            self.last = "ok".to_owned();
        }
    }

    fn checkpoint(&mut self, value: Option<&Value>) {
        if self.phase != "open" {
            self.last = "not_open".to_owned();
            return;
        }
        match str_field(value.unwrap_or(&Value::Null), "kind") {
            Some("valid") => {
                self.phase = "idle".to_owned();
                self.decision = "checkpoint".to_owned();
                self.checkpoint = "accepted".to_owned();
                self.continuations = self.continuations.saturating_add(1);
                self.delay = 0;
                self.reason.clear();
                self.continued = false;
                self.exit = 0;
                self.last = "ok".to_owned();
            }
            Some("invalid" | "oversized") => {
                self.halt("continuation_failed", 1);
                self.checkpoint = "failed".to_owned();
            }
            _ => self.last = "bad_checkpoint".to_owned(),
        }
    }

    fn set_waited(&mut self, value: Option<&Value>) {
        let waited = value
            .and_then(|value| value.get("ms"))
            .and_then(Value::as_u64);
        if let Some(waited) =
            waited.filter(|ms| matches!(*ms, 0 | 300_000 | 86_100_000 | 86_400_000))
        {
            self.waited = waited;
            self.last = "ok".to_owned();
        } else {
            self.last = "bad_wait".to_owned();
        }
    }

    fn halt(&mut self, reason: &str, exit: i32) {
        self.phase = "stopped".to_owned();
        self.decision = "stop".to_owned();
        self.delay = 0;
        self.reason = reason.to_owned();
        self.continued = false;
        self.exit = exit;
        self.queued = true;
        self.last = "ok".to_owned();
    }
}

#[must_use]
pub const fn retry_ceiling(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 2_000,
        2 => 4_000,
        3 => 8_000,
        4 => 16_000,
        5 => 32_000,
        _ => 60_000,
    }
}

#[must_use]
pub fn jitter_delay(attempt: u32, random: u32) -> u64 {
    let ceiling = retry_ceiling(attempt);
    let floor = ceiling / 2;
    floor + u64::from(random) % (ceiling - floor + 1)
}

fn classify_kind(kind: &str, has_items: bool) -> String {
    match kind {
        "ok" | "overload" | "malformed" | "transport" | "timeout" | "stall" | "usage_limit"
        | "login" | "incomplete" => kind.to_owned(),
        "first_byte" | "total" => "timeout".to_owned(),
        "cut" if has_items => "malformed".to_owned(),
        "cut" => "transport".to_owned(),
        _ => String::new(),
    }
}

fn provider_reason(kind: &str) -> String {
    format!("provider/{kind}")
}

fn continuable(kind: &str) -> bool {
    matches!(kind, "timeout" | "stall" | "transport" | "malformed")
}

fn str_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn bool_field(value: &Value, field: &str) -> bool {
    value.get(field).and_then(Value::as_bool).unwrap_or(false)
}
