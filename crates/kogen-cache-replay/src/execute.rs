//! Single-attempt execution and attempt-level receipts.

use crate::{
    Admission, AdmissionPolicy, MAX_POSTS, MAX_TOTAL_TOKENS, OUTPUT_CANCEL_THRESHOLD_TOKENS,
    PlannedAttempt, REASONING_CANCEL_THRESHOLD_TOKENS, ReplayPlan, build_wire_for_attempt,
    sha256_hex, verify_plan,
};
use kogen_core::provider::ProviderErrorKind;
use kogen_core::provider::auth::RequestCredential;
use kogen_core::provider::http::{
    HttpPort, RequestDeadlines, StreamLimitExceeded, StreamOutputLimits,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ReceiptLedger {
    pub schema_version: u32,
    pub plan_sha256: String,
    pub source_revision: String,
    pub binary_sha256: String,
    pub adapter_source_sha256: String,
    pub admission: Admission,
    pub max_posts: u64,
    pub max_total_tokens: u64,
    pub posts_sent: u64,
    pub tokens_charged_or_reserved: u64,
    pub observed_framing_margin_tokens: u64,
    pub auth_source: String,
    pub aborted_reason: Option<String>,
    pub attempts: Vec<AttemptReceipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AttemptReceipt {
    pub attempt_id: String,
    pub episode_id: String,
    pub panel: String,
    pub endpoint: String,
    pub body_profile: String,
    pub adapter_profile: String,
    pub fixture_id: String,
    pub source_run: String,
    pub source_request_ordinal: u32,
    pub source_adapter: String,
    pub source_body_sha256: String,
    pub source_provenance: crate::FixtureProvenance,
    pub source_model: String,
    pub source_effort: String,
    pub phase: String,
    pub schedule_index: u32,
    pub cache_condition: String,
    pub affinity: String,
    pub omitted_headers: Vec<String>,
    pub scope: String,
    pub prefix_tokens: u64,
    pub assigned_gap_ms: Option<u64>,
    pub actual_gap_ms: Option<u64>,
    pub gap_within_tolerance: Option<bool>,
    pub request_body_sha256: String,
    pub request_body_bytes: u64,
    pub output_limit_field: String,
    pub requested_output_limit_tokens: u64,
    pub admission_policy_name: String,
    pub output_cancel_threshold_tokens: u64,
    pub reasoning_cancel_threshold_tokens: u64,
    pub cache_key_fingerprint: String,
    pub thread_id_fingerprint: String,
    pub sent_header_names: Vec<String>,
    pub turn_state_received: bool,
    pub turn_state_echoed: bool,
    pub dispatched: bool,
    pub unexecuted_reason: Option<String>,
    pub dispatch_unix_ms: Option<u128>,
    pub terminal_unix_ms: Option<u128>,
    pub first_byte_ms: Option<u64>,
    pub terminal_elapsed_ms: Option<u64>,
    pub status_code: Option<u16>,
    pub response_model: Option<String>,
    pub raw_usage: Option<Value>,
    pub input_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub usage_valid: Option<bool>,
    pub charged_tokens: Option<u64>,
    pub reservation_tokens: u64,
    pub error_kind: Option<String>,
    pub output_limit_compliant: Option<bool>,
    pub truncated: bool,
    pub truncation_reason: Option<String>,
}

/// Run a plan through Kogen's HTTP port exactly once per dispatched attempt.
/// The credential callback is invoked once per POST so Kogen's owned login
/// layer can refresh under its normal lock before each dispatch.
pub fn execute_plan<F, J>(
    plan: &ReplayPlan,
    max_posts: u64,
    max_total_tokens: u64,
    auth_source: &str,
    http: &dyn HttpPort,
    credential_for_post: F,
    journal: J,
) -> Result<ReceiptLedger, String>
where
    F: FnMut() -> Result<RequestCredential, String>,
    J: FnMut(&AttemptReceipt) -> Result<(), String>,
{
    execute_plan_inner(
        plan,
        max_posts,
        max_total_tokens,
        auth_source,
        http,
        false,
        credential_for_post,
        journal,
    )
}

/// Create an attempt ledger for an unadmitted plan without loading credentials
/// or touching the HTTP port. Every scheduled slot is present with a null
/// response/usage record and an explicit unexecuted reason.
pub fn unadmitted_ledger(
    plan: &ReplayPlan,
    max_posts: u64,
    max_total_tokens: u64,
    auth_source: &str,
    reason: &str,
) -> Result<ReceiptLedger, String> {
    verify_plan(plan)?;
    if plan.admission.admitted {
        return Err("cannot create an unadmitted ledger for an admitted plan".to_owned());
    }
    let mut ledger = ReceiptLedger {
        schema_version: 2,
        plan_sha256: plan.plan_sha256.clone(),
        source_revision: plan.source_revision.clone(),
        binary_sha256: plan.binary_sha256.clone(),
        adapter_source_sha256: plan.adapter_source_sha256.clone(),
        admission: plan.admission.clone(),
        max_posts,
        max_total_tokens,
        posts_sent: 0,
        tokens_charged_or_reserved: 0,
        observed_framing_margin_tokens: 0,
        auth_source: auth_source.to_owned(),
        aborted_reason: Some(reason.to_owned()),
        attempts: Vec::with_capacity(plan.attempts.len()),
    };
    for planned in &plan.attempts {
        let mut receipt = receipt_for(planned, 0, &plan.admission.policy);
        receipt.unexecuted_reason = Some(reason.to_owned());
        ledger.attempts.push(receipt);
    }
    Ok(ledger)
}

#[allow(clippy::too_many_arguments)]
fn execute_plan_inner<F, J>(
    plan: &ReplayPlan,
    max_posts: u64,
    max_total_tokens: u64,
    auth_source: &str,
    http: &dyn HttpPort,
    test_capability_override: bool,
    mut credential_for_post: F,
    mut journal: J,
) -> Result<ReceiptLedger, String>
where
    F: FnMut() -> Result<RequestCredential, String>,
    J: FnMut(&AttemptReceipt) -> Result<(), String>,
{
    verify_plan(plan)?;
    if !test_capability_override && !plan.admission.admitted {
        return Err(format!(
            "plan is not admitted: {}",
            plan.admission.blockers.join("; ")
        ));
    }
    if max_posts > plan.maximum_posts || max_posts > MAX_POSTS {
        return Err(format!("--max-posts must be at most {MAX_POSTS}"));
    }
    if max_total_tokens > plan.maximum_total_tokens || max_total_tokens > MAX_TOTAL_TOKENS {
        return Err(format!(
            "--max-total-tokens must be at most {MAX_TOTAL_TOKENS}"
        ));
    }

    let mut ledger = ReceiptLedger {
        schema_version: 2,
        plan_sha256: plan.plan_sha256.clone(),
        source_revision: plan.source_revision.clone(),
        binary_sha256: plan.binary_sha256.clone(),
        adapter_source_sha256: plan.adapter_source_sha256.clone(),
        admission: plan.admission.clone(),
        max_posts,
        max_total_tokens,
        posts_sent: 0,
        tokens_charged_or_reserved: 0,
        observed_framing_margin_tokens: 0,
        auth_source: auth_source.to_owned(),
        aborted_reason: None,
        attempts: Vec::with_capacity(plan.attempts.len()),
    };
    let mut stop_reason: Option<String> = None;
    let mut failed_primers = std::collections::HashSet::new();
    let mut states_by_episode = HashMap::<String, String>::new();
    let mut previous_terminal: Option<Instant> = None;

    for planned in &plan.attempts {
        let mut receipt = receipt_for(
            planned,
            ledger.observed_framing_margin_tokens,
            &plan.admission.policy,
        );
        if let Some(reason) = stop_reason.as_ref() {
            receipt.unexecuted_reason = Some(reason.clone());
            journal(&receipt)?;
            ledger.attempts.push(receipt);
            continue;
        }
        if planned.phase == "probe" && failed_primers.contains(&planned.episode_id) {
            receipt.unexecuted_reason = Some("primer_failed".to_owned());
            journal(&receipt)?;
            ledger.attempts.push(receipt);
            continue;
        }
        if ledger.posts_sent >= max_posts {
            stop_reason = Some("hard post cap reached before dispatch".to_owned());
            receipt.unexecuted_reason = stop_reason.clone();
            journal(&receipt)?;
            ledger.attempts.push(receipt);
            continue;
        }

        let reservation = planned
            .input_reservation_tokens
            .saturating_add(ledger.observed_framing_margin_tokens)
            .saturating_add(planned.output_reservation_tokens);
        receipt.reservation_tokens = reservation;
        if ledger
            .tokens_charged_or_reserved
            .saturating_add(reservation)
            > max_total_tokens
        {
            stop_reason = Some("hard total-token cap would be exceeded before dispatch".to_owned());
            receipt.unexecuted_reason = stop_reason.clone();
            journal(&receipt)?;
            ledger.attempts.push(receipt);
            continue;
        }
        let input_budget_allowance = planned
            .input_reservation_tokens
            .saturating_add(ledger.observed_framing_margin_tokens);
        let hard_budget_output_tokens = max_total_tokens
            .saturating_sub(ledger.tokens_charged_or_reserved)
            .saturating_sub(input_budget_allowance);

        let auth = match credential_for_post() {
            Ok(credential) => credential,
            Err(_) => {
                stop_reason = Some(
                    "Kogen login unavailable; sign in with `kogen provider login chatgpt`"
                        .to_owned(),
                );
                receipt.unexecuted_reason = Some("kogen_auth_failed".to_owned());
                journal(&receipt)?;
                ledger.attempts.push(receipt);
                continue;
            }
        };
        let sticky_state = if planned.phase == "probe" && planned.echo_turn_state {
            states_by_episode
                .get(&planned.episode_id)
                .map(String::as_str)
        } else {
            None
        };
        let wire = match build_wire_for_attempt(planned, &auth, sticky_state) {
            Ok(wire) => wire,
            Err(reason) => {
                stop_reason = Some(format!(
                    "Kogen wire validation failed before dispatch: {reason}"
                ));
                receipt.unexecuted_reason = stop_reason.clone();
                journal(&receipt)?;
                ledger.attempts.push(receipt);
                continue;
            }
        };

        if let Some(target_gap_ms) = planned.gap_after_previous_ms
            && let Some(terminal) = previous_terminal
        {
            let elapsed = terminal.elapsed();
            let target = Duration::from_millis(target_gap_ms);
            if elapsed < target {
                std::thread::sleep(target - elapsed);
            }
            receipt.actual_gap_ms =
                Some(terminal.elapsed().as_millis().min(u64::MAX as u128) as u64);
            let tolerance = 250;
            receipt.gap_within_tolerance = Some(
                receipt
                    .actual_gap_ms
                    .is_some_and(|actual| actual.abs_diff(target_gap_ms) <= tolerance),
            );
        }
        receipt.sent_header_names = wire
            .headers
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        receipt.sent_header_names.sort();
        receipt.sent_header_names.dedup();
        receipt.turn_state_echoed = wire.header("x-codex-turn-state").is_some();
        receipt.dispatched = true;
        receipt.dispatch_unix_ms = Some(unix_millis());
        ledger.posts_sent = ledger.posts_sent.saturating_add(1);
        let dispatch = Instant::now();
        let limited = http.execute_with_output_limits(
            &wire,
            RequestDeadlines::from_environment(),
            StreamOutputLimits {
                output_tokens: OUTPUT_CANCEL_THRESHOLD_TOKENS,
                reasoning_tokens: REASONING_CANCEL_THRESHOLD_TOKENS,
                hard_budget_output_tokens,
            },
        );
        let limit_exceeded = limited.limit_exceeded;
        let attempt = limited.attempt;
        let terminal = Instant::now();
        let terminal_ms = dispatch.elapsed().as_millis().min(u64::MAX as u128) as u64;
        previous_terminal = Some(terminal);
        receipt.terminal_unix_ms = Some(unix_millis());
        receipt.first_byte_ms = attempt.first_byte_ms;
        receipt.terminal_elapsed_ms = Some(terminal_ms.max(attempt.elapsed_ms));
        receipt.status_code = attempt.status_code;
        receipt.response_model = attempt.response_model.clone();
        receipt.raw_usage = attempt.raw_usage.clone();
        receipt.turn_state_received = attempt
            .sticky_routing_token
            .as_deref()
            .is_some_and(|token| !token.is_empty());
        if planned.phase == "primer"
            && attempt.response.is_ok()
            && let Some(token) = attempt
                .sticky_routing_token
                .as_ref()
                .filter(|token| !token.is_empty())
        {
            states_by_episode.insert(planned.episode_id.clone(), token.clone());
        }

        let counters = raw_counters(attempt.raw_usage.as_ref());
        receipt.input_tokens = counters.input;
        receipt.uncached_input_tokens = counters.uncached;
        receipt.cached_input_tokens = counters.cached;
        receipt.output_tokens = counters.output;
        receipt.reasoning_tokens = counters.reasoning;
        receipt.usage_valid = counters.valid;
        receipt.error_kind = attempt
            .response
            .as_ref()
            .err()
            .map(|failure| failure.kind.as_str().to_owned());
        let threshold_exceeded = limit_exceeded.or_else(|| stream_limit_exceeded(&counters));
        receipt.truncated = threshold_exceeded.is_some();
        receipt.truncation_reason = threshold_exceeded.map(|reason| reason.as_str().to_owned());
        receipt.output_limit_compliant = counters
            .output
            .map(|output| output <= planned.output_limit_tokens);

        let accounted_input = counters.input.unwrap_or_else(|| {
            planned
                .input_reservation_tokens
                .saturating_add(ledger.observed_framing_margin_tokens)
        });
        let accounted_output = counters.output.unwrap_or_else(|| {
            planned
                .output_reservation_tokens
                .max(counters.reasoning.unwrap_or_default())
        });
        let accounted_tokens = accounted_input.saturating_add(accounted_output);
        receipt.charged_tokens = Some(if counters.input.is_some() && counters.output.is_some() {
            accounted_tokens
        } else {
            reservation.max(accounted_tokens)
        });
        ledger.tokens_charged_or_reserved = ledger
            .tokens_charged_or_reserved
            .saturating_add(receipt.charged_tokens.unwrap_or(reservation));
        let input_overrun = counters.input.is_some_and(|input| {
            input
                > planned
                    .input_reservation_tokens
                    .saturating_add(ledger.observed_framing_margin_tokens)
        });
        if let Some(input) = counters.input {
            ledger.observed_framing_margin_tokens = ledger
                .observed_framing_margin_tokens
                .max(input.saturating_sub(planned.input_reservation_tokens));
        }

        if limit_exceeded == Some(StreamLimitExceeded::GlobalBudget) {
            stop_reason = Some(
                "hard total-token cap reached during response streaming; no further request dispatched"
                    .to_owned(),
            );
        } else if counters.valid == Some(false) {
            stop_reason = Some("provider usage counters failed integrity checks".to_owned());
        } else if input_overrun {
            stop_reason =
                Some("provider input exceeded the reserved per-request input allowance".to_owned());
        } else if attempt
            .response_model
            .as_deref()
            .is_some_and(|model| model != plan.model)
        {
            stop_reason =
                Some("provider returned a model different from the frozen plan".to_owned());
        } else if ledger.tokens_charged_or_reserved > max_total_tokens {
            stop_reason = Some("observed usage exceeded the hard total-token cap".to_owned());
        } else if attempt.response.as_ref().err().is_some_and(|failure| {
            matches!(
                failure.kind,
                ProviderErrorKind::Login
                    | ProviderErrorKind::UsageLimit
                    | ProviderErrorKind::Unsupported
            )
        }) {
            stop_reason =
                Some("provider rejected login, usage allowance, or request capability".to_owned());
        }

        if planned.phase == "primer" && (attempt.response.is_err() || receipt.truncated) {
            failed_primers.insert(planned.episode_id.clone());
        }
        journal(&receipt)?;
        ledger.attempts.push(receipt);
    }
    ledger.aborted_reason = stop_reason;
    Ok(ledger)
}

fn receipt_for(
    planned: &PlannedAttempt,
    framing_margin: u64,
    policy: &AdmissionPolicy,
) -> AttemptReceipt {
    AttemptReceipt {
        attempt_id: planned.attempt_id.clone(),
        episode_id: planned.episode_id.clone(),
        panel: planned.panel.clone(),
        endpoint: planned.endpoint.url().to_owned(),
        body_profile: planned.body_profile.clone(),
        adapter_profile: planned.adapter_profile.clone(),
        fixture_id: planned.fixture_id.clone(),
        source_run: planned.source_run.clone(),
        source_request_ordinal: planned.source_request_ordinal,
        source_adapter: planned.source_adapter.clone(),
        source_body_sha256: planned.source_body_sha256.clone(),
        source_provenance: planned.source_provenance,
        source_model: planned.source_model.clone(),
        source_effort: planned.source_effort.clone(),
        phase: planned.phase.clone(),
        schedule_index: planned.schedule_index,
        cache_condition: planned.cache_condition.clone(),
        affinity: planned.affinity.clone(),
        omitted_headers: planned.omitted_headers.clone(),
        scope: planned.scope.clone(),
        prefix_tokens: planned.prefix_tokens,
        assigned_gap_ms: planned.gap_after_previous_ms,
        actual_gap_ms: None,
        gap_within_tolerance: None,
        request_body_sha256: planned.body_sha256.clone(),
        request_body_bytes: planned.body_bytes,
        output_limit_field: planned.output_limit_field.clone(),
        requested_output_limit_tokens: planned.output_limit_tokens,
        admission_policy_name: policy.name.clone(),
        output_cancel_threshold_tokens: policy.output_cancel_threshold_tokens,
        reasoning_cancel_threshold_tokens: policy.reasoning_cancel_threshold_tokens,
        cache_key_fingerprint: sha256_hex(planned.cache_key.as_bytes()),
        thread_id_fingerprint: sha256_hex(planned.thread_id.as_bytes()),
        sent_header_names: Vec::new(),
        turn_state_received: false,
        turn_state_echoed: false,
        dispatched: false,
        unexecuted_reason: None,
        dispatch_unix_ms: None,
        terminal_unix_ms: None,
        first_byte_ms: None,
        terminal_elapsed_ms: None,
        status_code: None,
        response_model: None,
        raw_usage: None,
        input_tokens: None,
        uncached_input_tokens: None,
        cached_input_tokens: None,
        output_tokens: None,
        reasoning_tokens: None,
        usage_valid: None,
        charged_tokens: None,
        reservation_tokens: planned
            .input_reservation_tokens
            .saturating_add(framing_margin)
            .saturating_add(planned.output_reservation_tokens),
        error_kind: None,
        output_limit_compliant: None,
        truncated: false,
        truncation_reason: None,
    }
}

fn stream_limit_exceeded(counters: &RawCounters) -> Option<StreamLimitExceeded> {
    if counters
        .output
        .is_some_and(|output| output > OUTPUT_CANCEL_THRESHOLD_TOKENS)
    {
        Some(StreamLimitExceeded::OutputTokens)
    } else if counters
        .reasoning
        .is_some_and(|reasoning| reasoning > REASONING_CANCEL_THRESHOLD_TOKENS)
    {
        Some(StreamLimitExceeded::ReasoningTokens)
    } else {
        None
    }
}

struct RawCounters {
    input: Option<u64>,
    uncached: Option<u64>,
    cached: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    valid: Option<bool>,
}

fn raw_counters(raw: Option<&Value>) -> RawCounters {
    let Some(raw) = raw else {
        return RawCounters {
            input: None,
            uncached: None,
            cached: None,
            output: None,
            reasoning: None,
            valid: None,
        };
    };
    let input = raw.get("input_tokens").and_then(Value::as_u64);
    let cached_value = raw
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"));
    let cached = cached_value.and_then(Value::as_u64);
    let output_value = raw.get("output_tokens");
    let output = output_value.and_then(Value::as_u64);
    let reasoning_value = raw
        .get("output_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"));
    let reasoning = reasoning_value.and_then(Value::as_u64);
    let malformed_present_counter = [
        raw.get("input_tokens"),
        cached_value,
        output_value,
        reasoning_value,
    ]
    .into_iter()
    .flatten()
    .any(|value| !value.is_null() && value.as_u64().is_none());
    let valid = if malformed_present_counter {
        Some(false)
    } else if let (Some(input), Some(cached)) = (input, cached) {
        Some(cached <= input)
    } else {
        Some(true)
    };
    RawCounters {
        input,
        uncached: input
            .zip(cached)
            .and_then(|(input, cached)| (cached <= input).then_some(input - cached)),
        cached,
        output,
        reasoning,
        valid,
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FixtureManifest, FixtureProvenance, ReplayFixture, build_plan, plan_digest,
        planning_credential,
    };
    use kogen_core::provider::ModelResponse;
    use kogen_core::provider::ModelUsage;
    use kogen_core::provider::http::WireRequest;
    use kogen_core::provider::http::{HttpAttempt, HttpPort};
    use serde_json::json;
    use std::sync::Mutex;

    struct FakeServer {
        requests: Mutex<Vec<WireRequest>>,
        raw_usage: Value,
    }

    impl HttpPort for FakeServer {
        fn execute(&self, request: &WireRequest, _deadlines: RequestDeadlines) -> HttpAttempt {
            self.requests.lock().unwrap().push(request.clone());
            HttpAttempt {
                response: Ok(ModelResponse {
                    id: "ephemeral-response-id".to_owned(),
                    text: "ephemeral output is never forwarded".to_owned(),
                    tool_calls: Vec::new(),
                    usage: ModelUsage::default(),
                    raw_items: Vec::new(),
                }),
                received_items: Vec::new(),
                first_byte_ms: Some(1),
                elapsed_ms: 2,
                body_bytes_received: 16,
                status_code: Some(200),
                raw_usage: Some(self.raw_usage.clone()),
                response_model: Some("gpt-6-luna".to_owned()),
                sticky_routing_token: Some("ephemeral-turn-state".to_owned()),
            }
        }
    }

    fn test_plan() -> ReplayPlan {
        let manifest = FixtureManifest {
            schema_version: 2,
            fixtures: vec![fixture("builder"), fixture("shaper")],
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let mut plan = build_plan(&manifest, &bytes, "fake-server-seed").unwrap();
        plan.admission.admitted = true;
        plan.admission.blockers.clear();
        plan.plan_sha256 = plan_digest(&plan).unwrap();
        plan
    }

    fn fixture(id: &str) -> ReplayFixture {
        ReplayFixture {
            id: id.to_owned(),
            source_run: format!("fixture-{id}"),
            source_request_ordinal: 1,
            source_adapter: "owned".to_owned(),
            source_body_sha256: "a".repeat(64),
            provenance: FixtureProvenance::Captured,
            sanitized: true,
            source_model: "gpt-6-luna".to_owned(),
            source_effort: "medium".to_owned(),
            instructions: "Sanitized local fixture instructions.".to_owned(),
            tool_schemas: tool_schemas_for(id),
            input_excerpt: "Safe synthetic excerpt.".to_owned(),
            continuation: crate::CONTINUATION.to_owned(),
        }
    }

    fn tool_schemas_for(id: &str) -> Vec<Value> {
        let allowed: &[&str] = if id == "builder" {
            &["finish", "shell", "tool_output"]
        } else {
            &["read", "search", "write"]
        };
        kogen_core::provider::tools::canonical_tool_schemas()
            .into_iter()
            .filter(|schema| {
                schema
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| allowed.contains(&name))
            })
            .collect()
    }

    #[test]
    fn execute_cancels_and_records_output_overflow_against_a_fake_server() {
        let plan = test_plan();
        let fake = FakeServer {
            requests: Mutex::new(Vec::new()),
            raw_usage: json!({
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 20},
                "output_tokens": 513,
                "output_tokens_details": {"reasoning_tokens": 4}
            }),
        };
        let mut journaled = Vec::new();
        let ledger = execute_plan_inner(
            &plan,
            1,
            MAX_TOTAL_TOKENS,
            "test_injected",
            &fake,
            true,
            || Ok(planning_credential()),
            |row| {
                journaled.push(row.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(ledger.posts_sent, 1);
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        assert_eq!(ledger.attempts.len(), 360);
        assert_eq!(journaled.len(), 360);
        assert_eq!(
            ledger.attempts[0].raw_usage.as_ref().unwrap()["output_tokens"],
            513
        );
        assert_eq!(ledger.attempts[0].output_limit_compliant, Some(false));
        assert_eq!(
            ledger.attempts[0].admission_policy_name,
            "coordinator_cancel_on_overflow_v1"
        );
        assert_eq!(ledger.attempts[0].output_cancel_threshold_tokens, 512);
        assert_eq!(ledger.attempts[0].reasoning_cancel_threshold_tokens, 1_024);
        assert!(ledger.attempts[0].truncated);
        assert_eq!(
            ledger.attempts[0].truncation_reason.as_deref(),
            Some("output_tokens_exceeded_512")
        );
        assert!(!ledger.attempts[1].dispatched);
        assert!(ledger.attempts[1].unexecuted_reason.is_some());
    }

    #[test]
    fn execute_cancels_and_records_reasoning_overflow_against_a_fake_server() {
        let plan = test_plan();
        let fake = FakeServer {
            requests: Mutex::new(Vec::new()),
            raw_usage: json!({
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 20},
                "output_tokens": 80,
                "output_tokens_details": {"reasoning_tokens": 1_025}
            }),
        };
        let ledger = execute_plan_inner(
            &plan,
            1,
            MAX_TOTAL_TOKENS,
            "test_owned",
            &fake,
            true,
            || Ok(planning_credential()),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(ledger.posts_sent, 1);
        assert!(ledger.attempts[0].truncated);
        assert_eq!(
            ledger.attempts[0].truncation_reason.as_deref(),
            Some("reasoning_tokens_exceeded_1024")
        );
    }

    #[test]
    fn execute_stops_before_post_when_reserved_tokens_would_breach_the_cap() {
        let plan = test_plan();
        let fake = FakeServer {
            requests: Mutex::new(Vec::new()),
            raw_usage: json!({"input_tokens": 1, "output_tokens": 1}),
        };
        let ledger = execute_plan_inner(
            &plan,
            360,
            1,
            "test_owned",
            &fake,
            true,
            || Ok(planning_credential()),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(ledger.posts_sent, 0);
        assert!(fake.requests.lock().unwrap().is_empty());
        assert!(
            ledger
                .aborted_reason
                .as_deref()
                .unwrap()
                .contains("would be exceeded before dispatch")
        );
    }

    #[test]
    fn execute_charges_provider_totals_then_stops_before_the_next_worst_case_reservation() {
        let plan = test_plan();
        let first = &plan.attempts[0];
        let second = &plan.attempts[1];
        let first_reservation = first.input_reservation_tokens + first.output_reservation_tokens;
        let second_reservation = second.input_reservation_tokens + second.output_reservation_tokens;
        let max_total_tokens = first_reservation + second_reservation - 1;
        let fake = FakeServer {
            requests: Mutex::new(Vec::new()),
            raw_usage: json!({
                "input_tokens": first.input_reservation_tokens,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": first.output_reservation_tokens
            }),
        };
        let ledger = execute_plan_inner(
            &plan,
            360,
            max_total_tokens,
            "test_owned",
            &fake,
            true,
            || Ok(planning_credential()),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(ledger.posts_sent, 1);
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        assert_eq!(
            ledger.tokens_charged_or_reserved,
            first.input_reservation_tokens + first.output_reservation_tokens
        );
        assert_eq!(
            ledger.attempts[1].unexecuted_reason.as_deref(),
            Some("hard total-token cap would be exceeded before dispatch")
        );
    }

    #[test]
    fn execute_stops_after_a_fake_server_reports_input_above_its_reservation() {
        let plan = test_plan();
        let first_allowance = plan.attempts[0].input_reservation_tokens;
        let fake = FakeServer {
            requests: Mutex::new(Vec::new()),
            raw_usage: json!({
                "input_tokens": first_allowance + 1,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 1
            }),
        };

        let ledger = execute_plan_inner(
            &plan,
            360,
            MAX_TOTAL_TOKENS,
            "test_owned",
            &fake,
            true,
            || Ok(planning_credential()),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(ledger.posts_sent, 1);
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        assert_eq!(ledger.attempts[0].input_tokens, Some(first_allowance + 1));
        assert_eq!(ledger.attempts[0].charged_tokens, Some(first_allowance + 2));
        assert!(
            ledger
                .aborted_reason
                .as_deref()
                .unwrap()
                .contains("input exceeded the reserved per-request input allowance")
        );
        assert!(!ledger.attempts[1].dispatched);
    }

    #[test]
    fn admitted_policy_does_not_require_a_verified_hard_request_cap() {
        let plan = test_plan();
        assert!(plan.admission.admitted);
        assert!(!plan.admission.policy.per_request_output_cap_required);
        let fake = FakeServer {
            requests: Mutex::new(Vec::new()),
            raw_usage: json!({"input_tokens": 1, "output_tokens": 1}),
        };
        let mut loaded_credentials = false;
        let ledger = execute_plan(
            &plan,
            1,
            MAX_TOTAL_TOKENS,
            "kogen_owned_account_selection",
            &fake,
            || {
                loaded_credentials = true;
                Ok(planning_credential())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert!(loaded_credentials);
        assert_eq!(ledger.posts_sent, 1);
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        assert_eq!(
            ledger.admission.policy.name,
            "coordinator_cancel_on_overflow_v1"
        );
    }

    #[test]
    fn unadmitted_ledger_records_every_slot_without_usage_or_dispatch() {
        let manifest = FixtureManifest {
            schema_version: 2,
            fixtures: vec![fixture("builder"), fixture("shaper")],
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let mut plan = build_plan(&manifest, &bytes, "blocked-ledger-seed").unwrap();
        plan.admission.admitted = false;
        plan.admission.blockers = vec!["test admission blocker".to_owned()];
        plan.plan_sha256 = crate::plan_digest(&plan).unwrap();
        let reason = plan.admission.blockers.join("; ");
        let ledger =
            unadmitted_ledger(&plan, 360, MAX_TOTAL_TOKENS, "kogen_owned", &reason).unwrap();

        assert_eq!(ledger.posts_sent, 0);
        assert_eq!(ledger.tokens_charged_or_reserved, 0);
        assert_eq!(ledger.attempts.len(), 360);
        assert!(ledger.attempts.iter().all(|receipt| {
            !receipt.dispatched
                && receipt.raw_usage.is_none()
                && receipt.unexecuted_reason.as_deref() == Some(reason.as_str())
        }));
    }
}
