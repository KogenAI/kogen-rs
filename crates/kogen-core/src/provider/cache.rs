//! Block-aware offline cache measurement. Production reuse is a diagnostic;
//! only designated requests in a feasible, frozen smoke replay have a threshold.
use super::ModelUsage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReplayDefinition {
    pub spec: String,
    pub adapter: String,
    pub prompt: String,
    pub replay: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub tokenizer: String,
    pub namespace: String,
    pub affinity: String,
    pub retention: String,
    pub minimum: u64,
    pub block: u64,
    pub appended_budget: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReplayRequest {
    pub conversation_id: String,
    pub prefix_tokens: Option<u64>,
    pub eligibility_known: bool,
    pub total_input: Option<u64>,
    pub cached_input: Option<u64>,
    pub designated_warm: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct RequestMeasurement {
    pub conversation_id: String,
    pub total_input: Option<u64>,
    pub cached_input: Option<u64>,
    pub eligible_input: Option<u64>,
    pub raw_hit_rate: Option<f64>,
    pub eligible_prefix_reuse: Option<f64>,
    pub excess_cached_input: Option<u64>,
    pub zero_eligible: bool,
    pub measurement: &'static str,
    pub qualification: &'static str,
}
#[derive(Clone, Debug, Serialize)]
pub struct ReplayMeasurement {
    pub versions: ReplayDefinition,
    pub requests: Vec<RequestMeasurement>,
    pub weighted_hit_rate: Option<f64>,
    pub partial: bool,
    pub qualification: &'static str,
    pub live_release_qualified: bool,
}

/// Validate the workload before attempting any replay. Unknown eligibility is
/// incomplete telemetry, so it cannot establish the theoretical smoke ratio.
pub fn measure(
    definition: &ReplayDefinition,
    requests: &[ReplayRequest],
) -> Result<ReplayMeasurement, String> {
    if definition.block == 0
        || [
            &definition.spec,
            &definition.adapter,
            &definition.prompt,
            &definition.replay,
            &definition.provider,
            &definition.model,
            &definition.endpoint,
            &definition.tokenizer,
            &definition.namespace,
            &definition.affinity,
            &definition.retention,
        ]
        .iter()
        .any(|value| value.is_empty())
    {
        return Err("freeze all replay identities and eligibility rules".to_owned());
    }
    let eligible = |request: &ReplayRequest| {
        request
            .prefix_tokens
            .filter(|_| request.eligibility_known)
            .map(|prefix| {
                if prefix < definition.minimum {
                    0
                } else {
                    prefix / definition.block * definition.block
                }
            })
    };
    for (index, request) in requests.iter().enumerate() {
        if request.designated_warm {
            let feasible = index >= 2
                && eligible(request)
                    .zip(request.total_input)
                    .is_some_and(|(e, t)| t > 0 && e <= t && e as f64 / t as f64 >= 0.95)
                && request
                    .total_input
                    .zip(request.prefix_tokens)
                    .is_some_and(|(total, prefix)| {
                        total >= prefix && total - prefix <= definition.appended_budget
                    });
            if !feasible {
                return Err("infeasible frozen warm request or appended-token budget".to_owned());
            }
        }
    }
    let mut total = 0_u64;
    let mut cached = 0_u64;
    let mut partial = false;
    let rows: Vec<_> = requests
        .iter()
        .map(|request| {
            let e = eligible(request);
            let counts = request
                .total_input
                .zip(request.cached_input)
                .filter(|(t, c)| c <= t);
            if let Some((t, c)) = counts {
                total += t;
                cached += c;
            } else {
                partial = true;
            }
            let raw = counts.and_then(|(t, c)| (t > 0).then_some(c as f64 / t as f64));
            let complete = counts.is_some() && e.is_some();
            let measurement = if !complete {
                "incomplete"
            } else if request.cached_input == Some(0) && e != Some(0) {
                "miss"
            } else {
                "measured"
            };
            let qualification = if !request.designated_warm {
                "inapplicable"
            } else if !complete {
                "incomplete"
            } else if raw.is_some_and(|ratio| ratio >= 0.95) {
                "pass"
            } else {
                "fail"
            };
            RequestMeasurement {
                conversation_id: request.conversation_id.clone(),
                total_input: request.total_input,
                cached_input: request.cached_input,
                eligible_input: e,
                raw_hit_rate: raw,
                eligible_prefix_reuse: counts
                    .zip(e)
                    .and_then(|((_, c), e)| (e > 0).then_some(c.min(e) as f64 / e as f64)),
                excess_cached_input: counts.zip(e).map(|((_, c), e)| c.saturating_sub(e)),
                zero_eligible: e == Some(0),
                measurement,
                qualification,
            }
        })
        .collect();
    let qualification = if rows.iter().any(|row| row.qualification == "incomplete") {
        "incomplete"
    } else if rows.iter().any(|row| row.qualification == "fail") {
        "fail"
    } else if rows.iter().any(|row| row.qualification == "pass") {
        "pass"
    } else {
        "inapplicable"
    };
    Ok(ReplayMeasurement {
        versions: definition.clone(),
        requests: rows,
        weighted_hit_rate: (total > 0).then_some(cached as f64 / total as f64),
        partial,
        qualification,
        live_release_qualified: false,
    })
}

/// Summarize known attempts, explicitly identifying partial totals. Missing
/// counts never become observed misses, and this has no production gate.
pub fn usage_summary(usages: &[ModelUsage]) -> serde_json::Value {
    let (mut total, mut cached, mut measured) = (0_u64, 0_u64, 0_usize);
    for usage in usages {
        if let Some((input, hit)) = usage.input.zip(usage.cached_input) {
            total += input + hit;
            cached += hit;
            measured += 1;
        }
    }
    serde_json::json!({"weighted_hit_rate":(total>0).then_some(cached as f64/total as f64),"partial":measured!=usages.len(),"attempts":usages.len(),"measured_attempts":measured,"known_total_input":total,"known_cached_input":cached})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition() -> ReplayDefinition {
        serde_json::from_value(serde_json::json!({"spec":"v1.3-draft","adapter":"fixture-v1","prompt":"p1","replay":"r1","provider":"fake","model":"m1","endpoint":"offline","tokenizer":"fixture","namespace":"isolated","affinity":"run","retention":"fixture","minimum":1024,"block":1024,"appended_budget":1000})).unwrap()
    }
    fn request(prefix: u64, total: u64, cached: Option<u64>, warm: bool) -> ReplayRequest {
        ReplayRequest {
            conversation_id: "independent".to_owned(),
            prefix_tokens: Some(prefix),
            eligibility_known: true,
            total_input: Some(total),
            cached_input: cached,
            designated_warm: warm,
        }
    }
    #[test]
    fn production_ratio_is_not_a_smoke_gate_and_infeasible_smoke_is_refused() {
        let r = request(4096, 5096, Some(4096), false);
        let measurement = measure(&definition(), std::slice::from_ref(&r)).unwrap();
        assert!((measurement.weighted_hit_rate.unwrap() - 0.80376766).abs() < 0.00001);
        assert_eq!(measurement.qualification, "inapplicable");
        let mut warm = r.clone();
        warm.designated_warm = true;
        assert!(measure(&definition(), &[r.clone(), r, warm]).is_err());
    }
    #[test]
    fn feasible_smoke_distinguishes_a_miss_from_incomplete_usage_and_rounds_blocks() {
        let cold = request(4200, 4300, Some(4096), false);
        for (cached, expected) in [
            (Some(4096), "pass"),
            (Some(0), "fail"),
            (None, "incomplete"),
        ] {
            let mut warm = cold.clone();
            warm.designated_warm = true;
            warm.cached_input = cached;
            let m = measure(&definition(), &[cold.clone(), cold.clone(), warm]).unwrap();
            assert_eq!(m.qualification, expected);
            assert_eq!(m.requests[2].eligible_input, Some(4096));
            assert!(!m.live_release_qualified);
        }
        let zero = measure(&definition(), &[request(1023, 1023, Some(0), false)]).unwrap();
        assert!(zero.requests[0].zero_eligible);
        assert_eq!(zero.requests[0].eligible_prefix_reuse, None);
    }
}
