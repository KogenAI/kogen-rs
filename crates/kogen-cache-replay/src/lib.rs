//! Private, offline-first prompt-cache replay planner and single-attempt runner.
//!
//! Fixture input is a sanitized excerpt manifest. This crate never imports
//! credential files; live authentication is delegated to `kogen-core`.

pub mod execute;

use base64::Engine as _;
use kogen_core::provider::auth::{Credential, RequestCredential};
use kogen_core::provider::http::{
    RequestContext, ResponseMode, WireConfig, WireRequest, build_wire_request,
};
use rand::{Rng as _, RngCore as _, SeedableRng as _, seq::SliceRandom as _};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use url::Url;

pub const MODEL: &str = "gpt-6-luna";
pub const EFFORT: &str = "medium";
pub const OUTPUT_TOKEN_CAP: u64 = 256;
pub const MAX_POSTS: u64 = 360;
pub const MAX_TOTAL_TOKENS: u64 = 2_000_000;
pub const PLANNED_TOKEN_RESERVATION: u64 = 1_941_504;
pub const INPUT_OVERHEAD_TOKENS: u64 = 512;
pub const LOCAL_TOKENIZER_ID: &str = "whitespace-v1";

pub const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/responses";
pub const CHATGPT_BACKEND_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";

pub const ROUTING_HEADERS: [&str; 6] = [
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-window-id",
    "x-codex-turn-metadata",
    "x-codex-turn-state",
];

const CONTINUATION: &str =
    "Continue the frozen replay fixture with one brief neutral response. Reply OK.";
const FIXED_INSTRUCTIONS: &str = "Follow the frozen replay instructions. Reply with exactly OK.";
const INPUT_EXCERPT_LIMIT: usize = 384;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct FixtureManifest {
    pub schema_version: u32,
    pub fixtures: Vec<ReplayFixture>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ReplayFixture {
    pub id: String,
    pub source_run: String,
    pub source_request_ordinal: u32,
    pub source_adapter: String,
    pub source_body_sha256: String,
    pub provenance: FixtureProvenance,
    pub sanitized: bool,
    pub source_model: String,
    pub source_effort: String,
    pub instructions: String,
    pub input_excerpt: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FixtureProvenance {
    Captured,
    Reconstructed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    OpenAiResponses,
    ChatgptBackend,
}

impl Endpoint {
    #[must_use]
    pub const fn url(self) -> &'static str {
        match self {
            Self::OpenAiResponses => OPENAI_ENDPOINT,
            Self::ChatgptBackend => CHATGPT_BACKEND_ENDPOINT,
        }
    }

    #[must_use]
    pub const fn wire_mode(self) -> ResponseMode {
        match self {
            Self::OpenAiResponses => ResponseMode::Owned,
            Self::ChatgptBackend => ResponseMode::OwnedBackend,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Admission {
    pub admitted: bool,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ReplayPlan {
    pub schema_version: u32,
    pub seed: String,
    pub prng: String,
    pub source_revision: String,
    pub binary_sha256: String,
    pub adapter_source_sha256: String,
    pub fixture_manifest_sha256: String,
    pub model: String,
    pub effort: String,
    pub body_profile: String,
    pub wire_adapter: String,
    pub local_tokenizer: String,
    pub output_token_cap: u64,
    pub maximum_posts: u64,
    pub maximum_total_tokens: u64,
    pub scheduled_posts: u64,
    pub scheduled_input_reservation: u64,
    pub scheduled_output_reservation: u64,
    pub scheduled_total_reservation: u64,
    pub admission: Admission,
    pub attempts: Vec<PlannedAttempt>,
    pub plan_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct PlannedAttempt {
    pub attempt_id: String,
    pub episode_id: String,
    pub pair_h_id: String,
    pub pair_c_id: String,
    pub pair_endpoint_id: String,
    pub panel: String,
    pub endpoint: Endpoint,
    pub body_profile: String,
    pub adapter_profile: String,
    pub fixture_id: String,
    pub source_run: String,
    pub source_request_ordinal: u32,
    pub source_adapter: String,
    pub source_body_sha256: String,
    pub source_provenance: FixtureProvenance,
    pub source_model: String,
    pub source_effort: String,
    pub phase: String,
    pub schedule_index: u32,
    pub schedule_orientation: String,
    pub cache_condition: String,
    pub affinity: String,
    pub omitted_headers: Vec<String>,
    pub scope: String,
    pub prefix_tokens: u64,
    pub gap_seconds: u64,
    pub gap_after_previous_ms: Option<u64>,
    pub echo_turn_state: bool,
    pub cache_key: String,
    pub thread_id: String,
    pub nonce: String,
    pub base_body_sha256: String,
    pub body: String,
    pub body_sha256: String,
    pub body_bytes: u64,
    pub planned_header_names: Vec<String>,
    pub conditional_header_names: Vec<String>,
    pub input_reservation_tokens: u64,
    pub output_reservation_tokens: u64,
}

#[derive(Clone, Debug)]
struct EpisodeDraft {
    episode_id: String,
    pair_h_id: String,
    pair_c_id: String,
    pair_endpoint_id: String,
    panel: String,
    endpoint: Endpoint,
    fixture_id: String,
    cache_condition: String,
    affinity: String,
    omitted_headers: Vec<String>,
    scope: String,
    prefix_tokens: u64,
    gap_seconds: u64,
    primer_nonce: String,
    probe_nonce: String,
    primer_cache_key: String,
    primer_thread_id: String,
    probe_cache_key: String,
    probe_thread_id: String,
    primer_text: String,
    probe_text: String,
    echo_turn_state: bool,
    order_orientation: String,
}

/// Build the complete deterministic 360-attempt manifest from two sanitized
/// fixture descriptions and a reproducible seed.
pub fn build_plan(
    manifest: &FixtureManifest,
    manifest_bytes: &[u8],
    seed: &str,
) -> Result<ReplayPlan, String> {
    validate_manifest(manifest)?;
    if seed.is_empty() {
        return Err("--seed must not be empty".to_owned());
    }

    let seed_bytes: [u8; 32] = Sha256::digest(seed.as_bytes()).into();
    let mut rng = ChaCha20Rng::from_seed(seed_bytes);
    let mut used_nonces = HashSet::new();
    let mut schedule_blocks = Vec::<Vec<EpisodeDraft>>::new();

    build_core_blocks(
        manifest,
        seed,
        &mut rng,
        &mut used_nonces,
        &mut schedule_blocks,
    );
    build_header_blocks(
        manifest,
        seed,
        &mut rng,
        &mut used_nonces,
        &mut schedule_blocks,
    );
    build_scope_blocks(
        manifest,
        seed,
        &mut rng,
        &mut used_nonces,
        &mut schedule_blocks,
    );
    schedule_blocks.shuffle(&mut rng);

    let fixture_by_id = manifest
        .fixtures
        .iter()
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut attempts = Vec::with_capacity(MAX_POSTS as usize);
    for block in schedule_blocks {
        for draft in block {
            let fixture = fixture_by_id
                .get(draft.fixture_id.as_str())
                .ok_or_else(|| "plan references an unknown fixture".to_owned())?;
            attempts.push(build_episode_attempts(&draft, fixture, seed)?);
        }
    }
    let attempts = attempts
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, mut attempt)| {
            attempt.schedule_index = index as u32;
            attempt
        })
        .collect::<Vec<_>>();
    if attempts.len() != MAX_POSTS as usize {
        return Err(format!(
            "planner produced {} requests; expected {MAX_POSTS}",
            attempts.len()
        ));
    }

    let scheduled_input_reservation = attempts
        .iter()
        .map(|attempt| attempt.input_reservation_tokens)
        .sum::<u64>();
    let scheduled_output_reservation = attempts
        .iter()
        .map(|attempt| attempt.output_reservation_tokens)
        .sum::<u64>();
    let scheduled_total_reservation =
        scheduled_input_reservation.saturating_add(scheduled_output_reservation);
    let blockers = vec![format!(
        "{} has no versioned Kogen generation-cap contract for max_output_tokens={OUTPUT_TOKEN_CAP}; the cap is present in the frozen body, but endpoint enforcement is unverified, so execute is not admitted",
        CHATGPT_BACKEND_ENDPOINT
    )];
    let mut plan = ReplayPlan {
        schema_version: 1,
        seed: seed.to_owned(),
        prng: "ChaCha20Rng rand_chacha-0.3.1".to_owned(),
        source_revision: "unresolved".to_owned(),
        binary_sha256: String::new(),
        adapter_source_sha256: adapter_source_sha256(),
        fixture_manifest_sha256: sha256_hex(manifest_bytes),
        model: MODEL.to_owned(),
        effort: EFFORT.to_owned(),
        body_profile: "owned_responses_common_v1".to_owned(),
        wire_adapter: "kogen-core::provider::http::wire/owned-common-v1".to_owned(),
        local_tokenizer: LOCAL_TOKENIZER_ID.to_owned(),
        output_token_cap: OUTPUT_TOKEN_CAP,
        maximum_posts: MAX_POSTS,
        maximum_total_tokens: MAX_TOTAL_TOKENS,
        scheduled_posts: attempts.len() as u64,
        scheduled_input_reservation,
        scheduled_output_reservation,
        scheduled_total_reservation,
        admission: Admission {
            admitted: blockers.is_empty(),
            blockers,
        },
        attempts,
        plan_sha256: String::new(),
    };
    plan.plan_sha256 = plan_digest(&plan)?;
    Ok(plan)
}

pub fn validate_manifest(manifest: &FixtureManifest) -> Result<(), String> {
    if manifest.schema_version != 1 {
        return Err("fixture manifest schema_version must be 1".to_owned());
    }
    if manifest.fixtures.len() != 2 {
        return Err("fixture manifest must contain exactly builder and shaper fixtures".to_owned());
    }
    let ids = manifest
        .fixtures
        .iter()
        .map(|fixture| fixture.id.as_str())
        .collect::<HashSet<_>>();
    if !ids.contains("builder") || !ids.contains("shaper") {
        return Err("fixture IDs must be exactly builder and shaper".to_owned());
    }
    for fixture in &manifest.fixtures {
        if fixture.source_run.trim().is_empty()
            || fixture.source_request_ordinal == 0
            || fixture.source_adapter.trim().is_empty()
            || fixture.source_model.trim().is_empty()
            || fixture.source_effort.trim().is_empty()
        {
            return Err(format!(
                "fixture {} is missing source provenance",
                fixture.id
            ));
        }
        if !is_sha256(&fixture.source_body_sha256) {
            return Err(format!(
                "fixture {} source_body_sha256 must be a 64-character SHA-256 hex digest",
                fixture.id
            ));
        }
        if !fixture.sanitized {
            return Err(format!(
                "fixture {} is not marked sanitized; raw request material is not accepted",
                fixture.id
            ));
        }
        if fixture.instructions.trim().is_empty() {
            return Err(format!(
                "fixture {} has no sanitized instructions",
                fixture.id
            ));
        }
    }
    Ok(())
}

pub fn verify_plan(plan: &ReplayPlan) -> Result<(), String> {
    if plan.schema_version != 1 {
        return Err("unsupported replay plan schema_version".to_owned());
    }
    if plan.model != MODEL || plan.effort != EFFORT {
        return Err("plan model or reasoning effort does not match the frozen design".to_owned());
    }
    if plan.body_profile != "owned_responses_common_v1"
        || plan.wire_adapter != "kogen-core::provider::http::wire/owned-common-v1"
    {
        return Err("plan body or wire adapter profile is unsupported".to_owned());
    }
    if plan.adapter_source_sha256 != adapter_source_sha256() {
        return Err("wire adapter source hash differs from the frozen plan".to_owned());
    }
    if plan.output_token_cap != OUTPUT_TOKEN_CAP {
        return Err("plan output cap does not match the frozen 256-token cap".to_owned());
    }
    if plan.maximum_posts != MAX_POSTS || plan.scheduled_posts != plan.attempts.len() as u64 {
        return Err("plan request allocation is invalid".to_owned());
    }
    if plan.maximum_total_tokens != MAX_TOTAL_TOKENS {
        return Err("plan total-token cap is invalid".to_owned());
    }
    if plan_digest(plan)? != plan.plan_sha256 {
        return Err("replay plan SHA-256 does not match its contents".to_owned());
    }
    for (index, attempt) in plan.attempts.iter().enumerate() {
        if attempt.schedule_index as usize != index {
            return Err("plan attempt order is not contiguous".to_owned());
        }
        if sha256_hex(attempt.body.as_bytes()) != attempt.body_sha256
            || attempt.body.len() as u64 != attempt.body_bytes
        {
            return Err(format!(
                "request body integrity check failed for {}",
                attempt.attempt_id
            ));
        }
        let body: Value = serde_json::from_str(&attempt.body)
            .map_err(|_| format!("request body is invalid JSON for {}", attempt.attempt_id))?;
        if body.get("model").and_then(Value::as_str) != Some(MODEL)
            || body.get("max_output_tokens").and_then(Value::as_u64) != Some(OUTPUT_TOKEN_CAP)
            || body.get("stream").and_then(Value::as_bool) != Some(true)
            || body.get("store").and_then(Value::as_bool) != Some(false)
            || body.get("tool_choice").and_then(Value::as_str) != Some("none")
        {
            return Err(format!(
                "request body controls are invalid for {}",
                attempt.attempt_id
            ));
        }
        if !matches!(
            attempt.endpoint,
            Endpoint::OpenAiResponses | Endpoint::ChatgptBackend
        ) {
            return Err("plan contains an endpoint outside the allowlist".to_owned());
        }
        if attempt.body_profile != plan.body_profile
            || !is_sha256(&attempt.source_body_sha256)
            || attempt.source_run.trim().is_empty()
            || attempt.source_request_ordinal == 0
            || attempt.source_adapter.trim().is_empty()
        {
            return Err(format!(
                "fixture provenance or body profile is invalid for {}",
                attempt.attempt_id
            ));
        }
    }
    Ok(())
}

#[must_use]
pub fn adapter_source_sha256() -> String {
    let mut digest = Sha256::new();
    digest.update(b"kogen-cache-replay-adapter-source-v1\0");
    digest.update(include_bytes!("../../kogen-core/src/provider/http/wire.rs"));
    digest.update(include_bytes!(
        "../../kogen-core/src/provider/http/wire/body.rs"
    ));
    digest.update(include_bytes!("execute.rs"));
    digest.update(include_bytes!("lib.rs"));
    hex_digest(&digest.finalize())
}

pub fn plan_digest(plan: &ReplayPlan) -> Result<String, String> {
    let mut unhashed = plan.clone();
    unhashed.plan_sha256.clear();
    let bytes = serde_json::to_vec(&unhashed).map_err(|error| error.to_string())?;
    Ok(sha256_hex(&bytes))
}

#[must_use]
pub fn dry_run_rows(plan: &ReplayPlan) -> Vec<Value> {
    plan.attempts
        .iter()
        .map(|attempt| {
            json!({
                "attempt_id": attempt.attempt_id,
                "episode_id": attempt.episode_id,
                "schedule_index": attempt.schedule_index,
                "panel": attempt.panel,
                "endpoint": attempt.endpoint.url(),
                "body_profile": attempt.body_profile,
                "adapter_profile": attempt.adapter_profile,
                "fixture_id": attempt.fixture_id,
                "phase": attempt.phase,
                "cache_condition": attempt.cache_condition,
                "affinity": attempt.affinity,
                "omitted_headers": attempt.omitted_headers,
                "scope": attempt.scope,
                "prefix_tokens": attempt.prefix_tokens,
                "gap_seconds": attempt.gap_seconds,
                "body_sha256": attempt.body_sha256,
                "body_bytes": attempt.body_bytes,
                "header_names": attempt.planned_header_names,
                "conditional_header_names": attempt.conditional_header_names,
                "input_reservation_tokens": attempt.input_reservation_tokens,
                "output_reservation_tokens": attempt.output_reservation_tokens
            })
        })
        .collect()
}

pub(crate) fn request_context(attempt: &PlannedAttempt) -> Result<RequestContext, String> {
    let body: Value = serde_json::from_str(&attempt.body)
        .map_err(|_| format!("invalid frozen body for {}", attempt.attempt_id))?;
    let instructions = body
        .get("instructions")
        .and_then(Value::as_str)
        .ok_or_else(|| "frozen request is missing instructions".to_owned())?;
    let input = body
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| "frozen request is missing input items".to_owned())?
        .clone();
    let client_metadata = body
        .get("client_metadata")
        .ok_or_else(|| "frozen request is missing Kogen client metadata".to_owned())?;
    let cache_key = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .ok_or_else(|| "frozen request is missing prompt_cache_key".to_owned())?;
    let thread_id = client_metadata
        .get("thread_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "frozen request is missing thread_id metadata".to_owned())?;
    let context = RequestContext {
        model: MODEL.to_owned(),
        effort: EFFORT.to_owned(),
        instructions: instructions.to_owned(),
        shared_instructions: String::new(),
        role_instructions: String::new(),
        input,
        tools: Vec::new(),
        callable_tools: Vec::new(),
        tool_choice: "none".to_owned(),
        development_request: false,
        generation_tokens: None,
        cache_key: cache_key.to_owned(),
        thread_id: thread_id.to_owned(),
        lite_session_id: cache_key.to_owned(),
        sticky_routing_token: None,
    };
    Ok(context)
}

pub(crate) fn build_wire_for_attempt(
    attempt: &PlannedAttempt,
    auth: &RequestCredential,
    sticky_routing_token: Option<&str>,
) -> Result<WireRequest, String> {
    let mut context = request_context(attempt)?;
    context.sticky_routing_token = sticky_routing_token.map(str::to_owned);
    let endpoint = Url::parse(attempt.endpoint.url()).map_err(|error| error.to_string())?;
    let config = WireConfig {
        endpoint_override: Some(endpoint),
        mode: attempt.endpoint.wire_mode(),
        supports_generation_cap: false,
        user_agent_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let mut wire =
        build_wire_request(&context, auth, &config).map_err(|failure| failure.message)?;
    if sha256_hex(&wire.body) != attempt.base_body_sha256 {
        return Err(format!(
            "Kogen wire body no longer matches frozen plan for {}",
            attempt.attempt_id
        ));
    }
    wire.body = attempt.body.as_bytes().to_vec();
    wire.headers.retain(|(name, _)| {
        !attempt
            .omitted_headers
            .iter()
            .any(|omitted| omitted.eq_ignore_ascii_case(name))
    });
    Ok(wire)
}

pub(crate) fn planning_credential() -> RequestCredential {
    let claims = json!({"https://api.openai.com/auth":{"chatgpt_account_id":"planning-account"}});
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).expect("planning claims serialize"));
    RequestCredential::Owned(Credential {
        client_id: String::new(),
        access_token: String::new(),
        refresh_token: String::new(),
        id_token: format!("header.{payload}.signature"),
        expires_at: i64::MAX,
        scopes: Vec::new(),
        subject: String::new(),
        email: None,
        host_id: String::new(),
    })
}

fn build_core_blocks(
    manifest: &FixtureManifest,
    seed: &str,
    rng: &mut ChaCha20Rng,
    used_nonces: &mut HashSet<String>,
    blocks: &mut Vec<Vec<EpisodeDraft>>,
) {
    let mut fixtures = manifest.fixtures.iter().collect::<Vec<_>>();
    fixtures.sort_by(|left, right| left.id.cmp(&right.id));
    for prefix in [512_u64, 2_048, 11_008] {
        for gap in [0_u64, 5, 30] {
            let endpoint_first_is_api = rng.gen_bool(0.5);
            let orientations = [
                (
                    Endpoint::OpenAiResponses,
                    rng.gen_bool(0.5),
                    rng.gen_bool(0.5),
                ),
                (
                    Endpoint::ChatgptBackend,
                    rng.gen_bool(0.5),
                    rng.gen_bool(0.5),
                ),
            ];
            for (fixture_index, fixture) in fixtures.iter().enumerate() {
                let mut block = Vec::with_capacity(8);
                let warm_twins = twin_nonces(rng, used_nonces);
                let cold_sham_twins = twin_nonces(rng, used_nonces);
                let cold_probe_twins = twin_nonces(rng, used_nonces);
                let endpoint_first = endpoint_first_is_api ^ (fixture_index == 1);
                let endpoints = if endpoint_first {
                    [Endpoint::OpenAiResponses, Endpoint::ChatgptBackend]
                } else {
                    [Endpoint::ChatgptBackend, Endpoint::OpenAiResponses]
                };
                for endpoint in endpoints {
                    let (_, base_h_on_first, base_c_warm_first) = orientations
                        .iter()
                        .find(|(candidate, _, _)| *candidate == endpoint)
                        .expect("endpoint orientation exists");
                    let h_on_first = *base_h_on_first ^ (fixture_index == 1);
                    let c_warm_first = *base_c_warm_first ^ (fixture_index == 1);
                    let h_order = if h_on_first {
                        [true, false]
                    } else {
                        [false, true]
                    };
                    let c_order = if c_warm_first {
                        ["warm", "cold"]
                    } else {
                        ["cold", "warm"]
                    };
                    for h_on in h_order {
                        for condition in c_order {
                            let pair_h = format!(
                                "core-h-{}-l{prefix}-g{gap}-{condition}-{}",
                                endpoint_key(endpoint),
                                fixture.id
                            );
                            let pair_c = format!(
                                "core-c-{}-l{prefix}-g{gap}-h{}-{}",
                                endpoint_key(endpoint),
                                if h_on { "on" } else { "off" },
                                fixture.id
                            );
                            let pair_endpoint = format!(
                                "core-e-l{prefix}-g{gap}-{}-h{}-{condition}",
                                fixture.id,
                                if h_on { "on" } else { "off" }
                            );
                            let mask = if h_on {
                                Vec::new()
                            } else {
                                ROUTING_HEADERS
                                    .iter()
                                    .map(|name| (*name).to_owned())
                                    .collect()
                            };
                            let arm_index = usize::from(h_on);
                            let (nonce0, nonce1) = if condition == "warm" {
                                let nonce = warm_twins[arm_index].clone();
                                (nonce.clone(), nonce)
                            } else {
                                (
                                    cold_sham_twins[arm_index].clone(),
                                    cold_probe_twins[arm_index].clone(),
                                )
                            };
                            let episode_id = format!(
                                "core-{}-l{prefix}-g{gap}-{condition}-h{}-{}",
                                endpoint_key(endpoint),
                                if h_on { "on" } else { "off" },
                                fixture.id
                            );
                            let id = identity(seed, &pair_endpoint);
                            let thread_id = identity(seed, &format!("thread:{pair_endpoint}"));
                            let base = bounded_excerpt(&fixture.input_excerpt);
                            block.push(EpisodeDraft {
                                episode_id,
                                pair_h_id: pair_h,
                                pair_c_id: pair_c,
                                pair_endpoint_id: pair_endpoint,
                                panel: "core".to_owned(),
                                endpoint,
                                fixture_id: fixture.id.clone(),
                                cache_condition: condition.to_owned(),
                                affinity: if h_on { "all_on" } else { "all_off" }.to_owned(),
                                omitted_headers: mask,
                                scope: "same_conversation".to_owned(),
                                prefix_tokens: prefix,
                                gap_seconds: gap,
                                primer_nonce: nonce0,
                                probe_nonce: nonce1,
                                primer_cache_key: id.clone(),
                                primer_thread_id: thread_id.clone(),
                                probe_cache_key: id.clone(),
                                probe_thread_id: thread_id,
                                primer_text: base.clone(),
                                probe_text: base,
                                echo_turn_state: h_on,
                                order_orientation: format!(
                                    "h_{}first_c_{}first",
                                    if h_on_first { "on" } else { "off" },
                                    if c_warm_first { "warm" } else { "cold" }
                                ),
                            });
                        }
                    }
                }
                blocks.push(block);
            }
        }
    }
}

fn build_header_blocks(
    manifest: &FixtureManifest,
    seed: &str,
    rng: &mut ChaCha20Rng,
    used_nonces: &mut HashSet<String>,
    blocks: &mut Vec<Vec<EpisodeDraft>>,
) {
    let mut fixtures = manifest.fixtures.iter().collect::<Vec<_>>();
    fixtures.sort_by(|left, right| left.id.cmp(&right.id));
    for endpoint in [Endpoint::OpenAiResponses, Endpoint::ChatgptBackend] {
        let mut headers = ROUTING_HEADERS.to_vec();
        headers.shuffle(rng);
        let mut order = [true, true, true, false, false, false];
        order.shuffle(rng);
        for (index, header) in headers.into_iter().enumerate() {
            let fixture = fixtures[index % fixtures.len()];
            let pair_id = format!("header-{}-{header}", endpoint_key(endpoint));
            let arm_order = if order[index] {
                ["all_on", "leave_one_out"]
            } else {
                ["leave_one_out", "all_on"]
            };
            let twins = [new_nonce(rng, used_nonces), new_nonce(rng, used_nonces)];
            let swap = rng.gen_bool(0.5);
            let arm_nonce = if swap {
                [twins[1].clone(), twins[0].clone()]
            } else {
                twins
            };
            let mut block = Vec::with_capacity(2);
            for (sequence, arm) in arm_order.into_iter().enumerate() {
                let omit = if arm == "leave_one_out" {
                    vec![header.to_owned()]
                } else {
                    Vec::new()
                };
                let episode_id = format!("{pair_id}-{arm}");
                let id = identity(seed, &episode_id);
                let base = bounded_excerpt(&fixture.input_excerpt);
                let echo_turn_state = !omit.iter().any(|header| header == "x-codex-turn-state");
                block.push(EpisodeDraft {
                    episode_id: episode_id.clone(),
                    pair_h_id: pair_id.clone(),
                    pair_c_id: String::new(),
                    pair_endpoint_id: String::new(),
                    panel: "individual_header".to_owned(),
                    endpoint,
                    fixture_id: fixture.id.clone(),
                    cache_condition: "warm".to_owned(),
                    affinity: if arm == "all_on" {
                        "all_on".to_owned()
                    } else {
                        format!("omit:{header}")
                    },
                    omitted_headers: omit,
                    scope: "same_conversation".to_owned(),
                    prefix_tokens: 2_048,
                    gap_seconds: 5,
                    primer_nonce: arm_nonce[sequence].clone(),
                    probe_nonce: arm_nonce[sequence].clone(),
                    primer_cache_key: id.clone(),
                    primer_thread_id: identity(seed, &format!("thread:{episode_id}")),
                    probe_cache_key: id.clone(),
                    probe_thread_id: identity(seed, &format!("thread:{episode_id}")),
                    primer_text: base.clone(),
                    probe_text: base,
                    echo_turn_state,
                    order_orientation: format!("{}_then_{}", arm_order[0], arm_order[1]),
                });
            }
            blocks.push(block);
        }
    }
}

fn build_scope_blocks(
    manifest: &FixtureManifest,
    seed: &str,
    rng: &mut ChaCha20Rng,
    used_nonces: &mut HashSet<String>,
    blocks: &mut Vec<Vec<EpisodeDraft>>,
) {
    let mut fixtures = manifest.fixtures.iter().collect::<Vec<_>>();
    fixtures.sort_by(|left, right| left.id.cmp(&right.id));
    for scope in [
        "S0_same_context",
        "S1_new_thread_shared_key",
        "S2_new_thread_new_key",
    ] {
        let api_first_base = rng.gen_bool(0.5);
        for (fixture_index, fixture) in fixtures.iter().enumerate() {
            let pair_id = format!("scope-{scope}-{}", fixture.id);
            let nonce = new_nonce(rng, used_nonces);
            let shared_key = identity(seed, &format!("{pair_id}:cache"));
            let thread_one = identity(seed, &format!("{pair_id}:thread-one"));
            let thread_two = if scope == "S0_same_context" {
                thread_one.clone()
            } else {
                identity(seed, &format!("{pair_id}:thread-two"))
            };
            let new_key = identity(seed, &format!("{pair_id}:cache-two"));
            let (probe_key, probe_thread) = match scope {
                "S0_same_context" => (shared_key.clone(), thread_one.clone()),
                "S1_new_thread_shared_key" => (shared_key.clone(), thread_two.clone()),
                _ => (new_key, thread_two),
            };
            let base = bounded_excerpt(&fixture.input_excerpt);
            let api_first = api_first_base ^ (fixture_index == 1);
            let endpoints = if api_first {
                [Endpoint::OpenAiResponses, Endpoint::ChatgptBackend]
            } else {
                [Endpoint::ChatgptBackend, Endpoint::OpenAiResponses]
            };
            let mut block = Vec::with_capacity(2);
            for endpoint in endpoints {
                let episode_id = format!("{pair_id}-{}", endpoint_key(endpoint));
                block.push(EpisodeDraft {
                    episode_id: episode_id.clone(),
                    pair_h_id: String::new(),
                    pair_c_id: String::new(),
                    pair_endpoint_id: pair_id.clone(),
                    panel: "conversation_scope".to_owned(),
                    endpoint,
                    fixture_id: fixture.id.clone(),
                    cache_condition: "warm".to_owned(),
                    affinity: "all_on".to_owned(),
                    omitted_headers: Vec::new(),
                    scope: scope.to_owned(),
                    prefix_tokens: 11_008,
                    gap_seconds: 5,
                    primer_nonce: nonce.clone(),
                    probe_nonce: nonce.clone(),
                    primer_cache_key: shared_key.clone(),
                    primer_thread_id: thread_one.clone(),
                    probe_cache_key: probe_key.clone(),
                    probe_thread_id: probe_thread.clone(),
                    primer_text: base.clone(),
                    probe_text: base.clone(),
                    echo_turn_state: scope == "S0_same_context",
                    order_orientation: format!("{} endpoint first", endpoint_key(endpoints[0])),
                });
            }
            blocks.push(block);
        }
    }
}

fn twin_nonces(rng: &mut ChaCha20Rng, used_nonces: &mut HashSet<String>) -> [String; 2] {
    let mut values = [new_nonce(rng, used_nonces), new_nonce(rng, used_nonces)];
    values.shuffle(rng);
    values
}

fn build_episode_attempts(
    draft: &EpisodeDraft,
    fixture: &ReplayFixture,
    _seed: &str,
) -> Result<[PlannedAttempt; 2], String> {
    let prefix_primer = make_prefix(
        draft.prefix_tokens,
        &draft.primer_nonce,
        &fixture.instructions,
    )?;
    let prefix_probe = make_prefix(
        draft.prefix_tokens,
        &draft.probe_nonce,
        &fixture.instructions,
    )?;
    let first = build_one_attempt(
        draft,
        fixture,
        "primer",
        0,
        &draft.primer_nonce,
        &draft.primer_cache_key,
        &draft.primer_thread_id,
        &prefix_primer,
        &draft.primer_text,
        None,
    )?;
    let second = build_one_attempt(
        draft,
        fixture,
        "probe",
        1,
        &draft.probe_nonce,
        &draft.probe_cache_key,
        &draft.probe_thread_id,
        &prefix_probe,
        &draft.probe_text,
        Some(draft.gap_seconds * 1_000),
    )?;
    Ok([first, second])
}

#[allow(clippy::too_many_arguments)]
fn build_one_attempt(
    draft: &EpisodeDraft,
    fixture: &ReplayFixture,
    phase: &str,
    phase_index: usize,
    nonce: &str,
    cache_key: &str,
    thread_id: &str,
    instructions: &str,
    input_text: &str,
    gap_after_previous_ms: Option<u64>,
) -> Result<PlannedAttempt, String> {
    let context = RequestContext {
        model: MODEL.to_owned(),
        effort: EFFORT.to_owned(),
        instructions: instructions.to_owned(),
        shared_instructions: String::new(),
        role_instructions: String::new(),
        input: replay_input_items(
            input_text,
            phase == "probe" || draft.cache_condition == "cold",
        ),
        tools: Vec::new(),
        callable_tools: Vec::new(),
        tool_choice: "none".to_owned(),
        development_request: false,
        generation_tokens: None,
        cache_key: cache_key.to_owned(),
        thread_id: thread_id.to_owned(),
        lite_session_id: cache_key.to_owned(),
        sticky_routing_token: None,
    };
    let config = WireConfig {
        endpoint_override: Some(
            Url::parse(draft.endpoint.url()).map_err(|error| error.to_string())?,
        ),
        mode: draft.endpoint.wire_mode(),
        supports_generation_cap: false,
        user_agent_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let mut base_wire = build_wire_request(&context, &planning_credential(), &config)
        .map_err(|failure| failure.message)?;
    let base_body_sha256 = sha256_hex(&base_wire.body);
    let frozen_body = add_output_cap(&base_wire.body)?;
    let body_sha256 = sha256_hex(&frozen_body);
    base_wire.body = frozen_body.clone();
    base_wire.headers.retain(|(name, _)| {
        !draft
            .omitted_headers
            .iter()
            .any(|omitted| omitted.eq_ignore_ascii_case(name))
    });
    let mut planned_header_names = header_names(&base_wire);
    let mut conditional_header_names = Vec::new();
    if phase == "probe"
        && draft.echo_turn_state
        && !draft
            .omitted_headers
            .iter()
            .any(|h| h == "x-codex-turn-state")
    {
        let mut with_state_context = context.clone();
        with_state_context.sticky_routing_token = Some("planning-only-state".to_owned());
        let mut with_state =
            build_wire_request(&with_state_context, &planning_credential(), &config)
                .map_err(|failure| failure.message)?;
        with_state.headers.retain(|(name, _)| {
            !draft
                .omitted_headers
                .iter()
                .any(|omitted| omitted.eq_ignore_ascii_case(name))
        });
        let names = header_names(&with_state);
        conditional_header_names = names
            .iter()
            .filter(|name| !planned_header_names.contains(name))
            .cloned()
            .collect();
        planned_header_names.extend(conditional_header_names.iter().cloned());
        planned_header_names.sort();
    }
    let input_reservation_tokens = draft.prefix_tokens + INPUT_OVERHEAD_TOKENS;
    Ok(PlannedAttempt {
        attempt_id: format!("{}-{phase}", draft.episode_id),
        episode_id: draft.episode_id.clone(),
        pair_h_id: draft.pair_h_id.clone(),
        pair_c_id: draft.pair_c_id.clone(),
        pair_endpoint_id: draft.pair_endpoint_id.clone(),
        panel: draft.panel.clone(),
        endpoint: draft.endpoint,
        body_profile: "owned_responses_common_v1".to_owned(),
        adapter_profile: match draft.endpoint {
            Endpoint::OpenAiResponses => "owned_headers_v1".to_owned(),
            Endpoint::ChatgptBackend => "owned_backend_compat_headers_v1".to_owned(),
        },
        fixture_id: fixture.id.clone(),
        source_run: fixture.source_run.clone(),
        source_request_ordinal: fixture.source_request_ordinal,
        source_adapter: fixture.source_adapter.clone(),
        source_body_sha256: fixture.source_body_sha256.clone(),
        source_provenance: fixture.provenance,
        source_model: fixture.source_model.clone(),
        source_effort: fixture.source_effort.clone(),
        phase: phase.to_owned(),
        schedule_index: 0,
        schedule_orientation: draft.order_orientation.clone(),
        cache_condition: draft.cache_condition.clone(),
        affinity: draft.affinity.clone(),
        omitted_headers: draft.omitted_headers.clone(),
        scope: draft.scope.clone(),
        prefix_tokens: draft.prefix_tokens,
        gap_seconds: draft.gap_seconds,
        gap_after_previous_ms,
        echo_turn_state: phase_index == 1 && draft.echo_turn_state,
        cache_key: cache_key.to_owned(),
        thread_id: thread_id.to_owned(),
        nonce: nonce.to_owned(),
        base_body_sha256,
        body: String::from_utf8(frozen_body).map_err(|error| error.to_string())?,
        body_sha256,
        body_bytes: base_wire.body.len() as u64,
        planned_header_names,
        conditional_header_names,
        input_reservation_tokens,
        output_reservation_tokens: OUTPUT_TOKEN_CAP,
    })
}

fn add_output_cap(base_body: &[u8]) -> Result<Vec<u8>, String> {
    let mut body: Value = serde_json::from_slice(base_body)
        .map_err(|error| format!("Kogen produced invalid JSON request body: {error}"))?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| "Kogen request body is not a JSON object".to_owned())?;
    object.insert("max_output_tokens".to_owned(), json!(OUTPUT_TOKEN_CAP));
    serde_json::to_vec(&body).map_err(|error| error.to_string())
}

fn make_prefix(target: u64, nonce: &str, instructions: &str) -> Result<String, String> {
    let target = usize::try_from(target).map_err(|_| "prefix token target is too large")?;
    let mut tokens = vec![format!("nonce={nonce}")];
    tokens.extend(FIXED_INSTRUCTIONS.split_whitespace().map(str::to_owned));
    let available = target.saturating_sub(tokens.len());
    tokens.extend(
        instructions
            .split_whitespace()
            .take(available)
            .map(str::to_owned),
    );
    if tokens.len() > target {
        tokens.truncate(target);
    }
    tokens.resize(target, "pad".to_owned());
    let result = tokens.join(" ");
    if result.split_whitespace().count() != target {
        return Err("local prefix tokenizer did not reach the target".to_owned());
    }
    Ok(result)
}

fn bounded_excerpt(input: &str) -> String {
    input
        .split_whitespace()
        .take(INPUT_EXCERPT_LIMIT)
        .collect::<Vec<_>>()
        .join(" ")
}

fn replay_input_items(input: &str, include_continuation: bool) -> Vec<Value> {
    let mut items = vec![user_message(input)];
    if include_continuation {
        items.push(user_message(CONTINUATION));
    }
    items
}

fn user_message(text: &str) -> Value {
    json!({
        "role": "user",
        "content": [{"type": "input_text", "text": text}]
    })
}

fn header_names(wire: &WireRequest) -> Vec<String> {
    let mut names = wire
        .headers
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn endpoint_key(endpoint: Endpoint) -> &'static str {
    match endpoint {
        Endpoint::OpenAiResponses => "openai",
        Endpoint::ChatgptBackend => "backend",
    }
}

fn identity(seed: &str, label: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kogen-cache-replay-id-v1\0");
    digest.update(seed.as_bytes());
    digest.update([0]);
    digest.update(label.as_bytes());
    hex_digest(digest.finalize().as_slice())
}

fn new_nonce(rng: &mut ChaCha20Rng, used: &mut HashSet<String>) -> String {
    loop {
        let mut bytes = [0_u8; 16];
        rng.fill_bytes(&mut bytes);
        let nonce = hex_digest(&bytes);
        if used.insert(nonce.clone()) {
            return nonce;
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_manifest() -> (FixtureManifest, Vec<u8>) {
        let manifest = FixtureManifest {
            schema_version: 1,
            fixtures: vec![
                fixture("builder", "Instructions for the frozen builder fixture."),
                fixture("shaper", "Instructions for the frozen shaper fixture."),
            ],
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        (manifest, bytes)
    }

    fn fixture(id: &str, instructions: &str) -> ReplayFixture {
        ReplayFixture {
            id: id.to_owned(),
            source_run: format!("source-{id}"),
            source_request_ordinal: 1,
            source_adapter: "owned".to_owned(),
            source_body_sha256: "a".repeat(64),
            provenance: FixtureProvenance::Captured,
            sanitized: true,
            source_model: "gpt-6-luna".to_owned(),
            source_effort: "medium".to_owned(),
            instructions: instructions.to_owned(),
            input_excerpt: "A short sanitized fixture excerpt.".to_owned(),
        }
    }

    #[test]
    fn plan_is_deterministic_and_keeps_the_exact_allocation() {
        let (manifest, bytes) = fixture_manifest();
        let first = build_plan(&manifest, &bytes, "repeatable-seed").unwrap();
        let second = build_plan(&manifest, &bytes, "repeatable-seed").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.plan_sha256, plan_digest(&first).unwrap());
        assert_eq!(first.scheduled_posts, 360);
        assert_eq!(first.scheduled_input_reservation, 1_849_344);
        assert_eq!(first.scheduled_output_reservation, 92_160);
        assert_eq!(first.scheduled_total_reservation, PLANNED_TOKEN_RESERVATION);
        assert!(!first.admission.admitted);
        assert!(first.admission.blockers[0].contains("enforcement is unverified"));
        verify_plan(&first).unwrap();

        for primer in first
            .attempts
            .iter()
            .filter(|attempt| attempt.panel == "core" && attempt.phase == "primer")
        {
            let probe = first
                .attempts
                .iter()
                .find(|attempt| attempt.episode_id == primer.episode_id && attempt.phase == "probe")
                .expect("each core primer has a probe");
            let primer_body: Value = serde_json::from_str(&primer.body).unwrap();
            let probe_body: Value = serde_json::from_str(&probe.body).unwrap();
            let primer_items = primer_body["input"].as_array().unwrap();
            let probe_items = probe_body["input"].as_array().unwrap();
            assert_eq!(primer_items[0], probe_items[0]);
            if primer.cache_condition == "warm" {
                assert_eq!(probe_items.len(), primer_items.len() + 1);
                assert_eq!(probe_items[1]["content"][0]["text"], CONTINUATION);
            } else {
                assert_eq!(primer_items, probe_items);
                assert_ne!(primer.nonce, probe.nonce);
            }
        }

        for attempt in first
            .attempts
            .iter()
            .filter(|attempt| attempt.panel == "core")
        {
            let endpoint_twin = first
                .attempts
                .iter()
                .find(|candidate| {
                    candidate.panel == "core"
                        && candidate.pair_endpoint_id == attempt.pair_endpoint_id
                        && candidate.phase == attempt.phase
                        && candidate.endpoint != attempt.endpoint
                })
                .expect("each core endpoint arm has a twin");
            assert_eq!(attempt.body, endpoint_twin.body);
            assert_eq!(attempt.body_sha256, endpoint_twin.body_sha256);
            assert_eq!(attempt.base_body_sha256, endpoint_twin.base_body_sha256);
        }
    }

    #[test]
    fn header_mask_changes_only_the_six_routing_headers() {
        let mut request = RequestContext {
            model: MODEL.to_owned(),
            effort: EFFORT.to_owned(),
            instructions: "nonce=fixture Reply OK".to_owned(),
            shared_instructions: String::new(),
            role_instructions: String::new(),
            input: vec![user_message("fixture")],
            tools: Vec::new(),
            callable_tools: Vec::new(),
            tool_choice: "none".to_owned(),
            development_request: false,
            generation_tokens: None,
            cache_key: "same-cache-key".to_owned(),
            thread_id: "same-thread-id".to_owned(),
            lite_session_id: "same-cache-key".to_owned(),
            sticky_routing_token: Some("state-token".to_owned()),
        };
        let auth = planning_credential();
        let config = WireConfig {
            endpoint_override: Some(Url::parse(OPENAI_ENDPOINT).unwrap()),
            mode: ResponseMode::Owned,
            supports_generation_cap: false,
            user_agent_version: "0.1.0".to_owned(),
        };
        let on = build_wire_request(&request, &auth, &config).unwrap();
        let mut off = on.clone();
        off.headers.retain(|(name, _)| {
            !ROUTING_HEADERS
                .iter()
                .any(|header| header.eq_ignore_ascii_case(name))
        });
        assert_eq!(on.body, off.body);
        assert_eq!(on.headers.len() - off.headers.len(), ROUTING_HEADERS.len());
        for header in ROUTING_HEADERS {
            assert!(on.header(header).is_some(), "expected {header}");
            assert!(off.header(header).is_none(), "unexpected {header}");
        }
        assert!(off.header("authorization").is_some());
        assert!(off.header("content-type").is_some());
        request.sticky_routing_token = None;

        let (manifest, bytes) = fixture_manifest();
        let plan = build_plan(&manifest, &bytes, "header-factor-seed").unwrap();
        let on_attempt = plan
            .attempts
            .iter()
            .find(|attempt| {
                attempt.panel == "core" && attempt.phase == "probe" && attempt.affinity == "all_on"
            })
            .unwrap();
        let off_attempt = plan
            .attempts
            .iter()
            .find(|attempt| {
                attempt.pair_h_id == on_attempt.pair_h_id
                    && attempt.phase == "probe"
                    && attempt.affinity == "all_off"
            })
            .unwrap();
        assert!(on_attempt.omitted_headers.is_empty());
        assert_eq!(
            off_attempt.omitted_headers,
            ROUTING_HEADERS
                .iter()
                .map(|header| (*header).to_owned())
                .collect::<Vec<_>>()
        );
        let auth = planning_credential();
        let on_wire = build_wire_for_attempt(on_attempt, &auth, Some("test-state")).unwrap();
        let off_wire = build_wire_for_attempt(off_attempt, &auth, Some("test-state")).unwrap();
        let on_names = on_wire
            .headers
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let off_names = off_wire
            .headers
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        assert_eq!(
            on_names
                .difference(&off_names)
                .cloned()
                .collect::<HashSet<_>>(),
            ROUTING_HEADERS
                .iter()
                .map(|header| (*header).to_owned())
                .collect::<HashSet<_>>()
        );
        assert!(off_names.difference(&on_names).next().is_none());
        assert!(off_wire.header("authorization").is_some());
        assert!(off_wire.header("content-type").is_some());
    }

    #[test]
    fn owned_backend_profile_uses_same_body_and_adds_required_compatibility_headers() {
        let context = RequestContext {
            model: MODEL.to_owned(),
            effort: EFFORT.to_owned(),
            instructions: "nonce=fixture Reply OK".to_owned(),
            shared_instructions: String::new(),
            role_instructions: String::new(),
            input: vec![user_message("fixture")],
            tools: Vec::new(),
            callable_tools: Vec::new(),
            tool_choice: "none".to_owned(),
            development_request: false,
            generation_tokens: None,
            cache_key: "same-cache-key".to_owned(),
            thread_id: "same-thread-id".to_owned(),
            lite_session_id: "same-cache-key".to_owned(),
            sticky_routing_token: None,
        };
        let auth = planning_credential();
        let api_config = WireConfig {
            endpoint_override: Some(Url::parse(OPENAI_ENDPOINT).unwrap()),
            mode: ResponseMode::Owned,
            supports_generation_cap: false,
            user_agent_version: "0.1.0".to_owned(),
        };
        let backend_config = WireConfig {
            endpoint_override: Some(Url::parse(CHATGPT_BACKEND_ENDPOINT).unwrap()),
            mode: ResponseMode::OwnedBackend,
            supports_generation_cap: false,
            user_agent_version: "0.1.0".to_owned(),
        };
        let api = build_wire_request(&context, &auth, &api_config).unwrap();
        let backend = build_wire_request(&context, &auth, &backend_config).unwrap();
        assert_eq!(api.body, backend.body);
        assert!(backend.header("chatgpt-account-id").is_some());
        assert!(backend.header("openai-beta").is_some());
        assert!(backend.header("originator").is_some());
        assert_eq!(api.header("user-agent"), backend.header("user-agent"));
    }
}
