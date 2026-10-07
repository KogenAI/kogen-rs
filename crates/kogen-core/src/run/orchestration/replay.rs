//! Typed event bridge used by the private xspec executable.

use super::{BuildEvent, BuildMachine, BuildObservation, step};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default)]
pub struct OrchestrationReplay {
    machine: BuildMachine,
}

impl OrchestrationReplay {
    #[must_use]
    pub fn new() -> Self {
        Self {
            machine: BuildMachine::new(),
        }
    }

    pub fn apply(&mut self, event: &Value) -> Result<(), String> {
        let tag = event
            .get("tag")
            .and_then(Value::as_str)
            .ok_or("event requires string field `tag`")?;
        let value = event.get("value").cloned().unwrap_or(Value::Null);
        let event = match tag {
            "Init" => BuildEvent::Init,
            "Begin" => BuildEvent::Begin {
                valid: boolean(&value, "valid")?,
                branch_ok: boolean(&value, "branchOk")?,
                claim_free: boolean(&value, "claimFree")?,
                witness: boolean(&value, "witness")?,
                hard: boolean(&value, "hard")?,
                max_rungs: integer(&value, "maxR")?,
            },
            "Probe" => BuildEvent::Probe {
                confined: boolean(&value, "confined")?,
            },
            "Witness" => BuildEvent::Witness {
                green: boolean(&value, "green")?,
            },
            "Plan" => BuildEvent::Plan,
            "Setup" => BuildEvent::Setup {
                ok: boolean(&value, "ok")?,
            },
            "BaseAccept" => BuildEvent::BaseAccept {
                runner: boolean(&value, "runner")?,
            },
            "Verify" => BuildEvent::Verify {
                red: boolean(&value, "red")?,
                landable: boolean(&value, "landable")?,
                counted: boolean(&value, "counted")?,
                count: unsigned(&value, "count")?,
            },
            "Repair" => BuildEvent::Repair {
                lower: boolean(&value, "lower")?,
            },
            "Pair" => BuildEvent::Pair {
                first: string(&value, "first")?.to_owned(),
                second: string(&value, "second")?.to_owned(),
            },
            "Audit" => BuildEvent::Audit {
                now_landable: boolean(&value, "nowLandable")?,
            },
            "Budget" => BuildEvent::Budget,
            "Land" => BuildEvent::Land {
                how: string(&value, "how")?.to_owned(),
            },
            "Stop" => BuildEvent::Stop {
                why: string(&value, "why")?.to_owned(),
            },
            _ => return Err(format!("unknown orchestration event `{tag}`")),
        };
        self.machine = step(&self.machine, &event).0;
        Ok(())
    }

    #[must_use]
    pub fn observe(&self) -> Value {
        observation_value(&self.machine.observation())
    }
}

fn observation_value(observation: &BuildObservation) -> Value {
    json!({
        "last": observation.last,
        "exit": observation.exit,
        "phase": observation.phase,
        "status": observation.status,
        "reason": observation.reason,
        "rung": observation.rung,
        "entry": observation.entry,
        "claim": observation.claim,
        "planned": observation.planned,
        "landable": observation.landable,
        "parkedRef": observation.parked_ref,
        "repairs": observation.repairs,
        "granted": observation.granted,
        "snapshots": observation.snapshots,
        "journal": observation.journal,
    })
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

fn unsigned(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("event requires nonnegative integer field `{key}`"))
}

fn integer(value: &Value, key: &str) -> Result<u8, String> {
    unsigned(value, key)?
        .try_into()
        .map_err(|_| format!("event integer field `{key}` is too large"))
}
