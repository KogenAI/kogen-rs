//! Ordered candidate checks for a completed shaping pass.

use super::super::audit;
use super::super::validation::{self, ValidationFailure};
use super::checkout_lock::CheckoutLock;
use super::execute::{PassResult, RunState};
use super::files::write_json;
use crate::intent;
use std::collections::BTreeMap;
use std::fs;

pub(super) fn validate_pass(
    state: &mut RunState,
    pass: usize,
    role: &str,
    final_text: &str,
    style_repairs: usize,
) -> Result<PassResult, crate::error::CoreError> {
    let _lock = CheckoutLock::acquire(&state.options.home, &state.checkout)?;
    add_concerns(state, final_text);
    let generated = fs::read(&state.intent_path)
        .map_err(|error| super::files::io_error("shape_output_unavailable", error))?;
    let test = fs::read(&state.acceptance_path)
        .map_err(|error| super::files::io_error("shape_output_unavailable", error))?;
    let normalized = match validation::normalize(&generated, &state.request) {
        Ok(bytes) => bytes,
        Err(failure) => return Ok(PassResult::Failure(failure)),
    };
    let intent_rel = state
        .intent_path
        .strip_prefix(&state.checkout)
        .map_err(|error| super::files::io_error("shape_output_unavailable", error))?;
    crate::safe_fs::validate_write(&state.checkout, intent_rel)
        .and_then(|()| crate::safe_fs::write_file(&state.checkout, intent_rel, &normalized))
        .map_err(|error| super::files::io_error("shape_output_unavailable", error))?;
    let mut parsed = match validation::parse_and_lint(&state.options.slug, &normalized) {
        Ok(parsed) => parsed,
        Err(failure) => return Ok(PassResult::Failure(failure)),
    };
    if !parsed.style.is_empty() && style_repairs < 2 {
        return Ok(PassResult::StyleRepair(
            parsed.style.iter().map(validation::render_issue).collect(),
        ));
    }
    for issue in &parsed.style {
        let item_ids = parsed
            .intent
            .acceptance
            .iter()
            .find(|item| Some(item.line) == issue.line)
            .map_or_else(Vec::new, |item| vec![item.id.clone()]);
        state.warnings.push(super::super::ShapeWarning {
            code: format!("lint_{}", issue.rule),
            item_ids,
            message: issue.message.clone(),
        });
    }
    if let Some(path) = validation::undeclared_gate_path(&parsed.intent, &test, &state.gate_paths) {
        return Ok(PassResult::Failure(ValidationFailure {
            reason: "undeclared_gate_path",
            detail: format!("Gate-path edit requires `changes_gate: true`; matched path {path}."),
        }));
    }
    let formatter_unavailable = match state.commands.formatter(
        &state.checkout,
        state.config.as_ref(),
        [&state.intent_rel, &state.acceptance_rel],
    ) {
        Ok(unavailable) => unavailable,
        Err(failure) => return Ok(PassResult::Failure(failure)),
    };
    if formatter_unavailable {
        state.progress.push(format!(
            "shaper pass={pass} role={role} warning formatter_unavailable"
        ));
        state.warnings.push(super::super::ShapeWarning {
            code: "formatter_unavailable".to_owned(),
            item_ids: Vec::new(),
            message: "configured formatter is unavailable".to_owned(),
        });
    }
    let formatted_intent = fs::read(&state.intent_path)
        .map_err(|error| super::files::io_error("shape_output_unavailable", error))?;
    let formatted_test = fs::read(&state.acceptance_path)
        .map_err(|error| super::files::io_error("shape_output_unavailable", error))?;
    if let Err(failure) = state.commands.staged_acceptance_checks(
        &state.checkout,
        state.config.as_ref(),
        &state.candidate_rel,
        &formatted_test,
    ) {
        return Ok(PassResult::Failure(failure));
    }
    let item_ids = parsed
        .intent
        .acceptance
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let base_results = match state.commands.base_acceptance(
        &state.checkout,
        &state.options.slug,
        &state.acceptance_rel,
        &state.run_dir.join("reports/base-acceptance.jsonl"),
        item_ids.clone(),
        pass,
    ) {
        Ok(results) => results,
        Err(failure) => return Ok(PassResult::Failure(failure)),
    };
    let results = item_ids
        .iter()
        .map(|id| (id.clone(), base_results.contains(id)))
        .collect::<BTreeMap<_, _>>();
    let (reclassified, mut reclass_warnings, any_red_change) =
        match validation::reclassify(&state.options.slug, &formatted_intent, &results) {
            Ok(result) => result,
            Err(failure) => return Ok(PassResult::Failure(failure)),
        };
    state.warnings.append(&mut reclass_warnings);
    if !any_red_change {
        return Ok(PassResult::Failure(ValidationFailure {
            reason: "all_items_keep",
            detail: "at least one test item must be fully red on the base".to_owned(),
        }));
    }
    crate::safe_fs::validate_write(&state.checkout, intent_rel)
        .and_then(|()| crate::safe_fs::write_file(&state.checkout, intent_rel, &reclassified))
        .map_err(|error| super::files::io_error("shape_output_unavailable", error))?;
    parsed = match validation::parse_and_lint(&state.options.slug, &reclassified) {
        Ok(parsed) => parsed,
        Err(failure) => return Ok(PassResult::Failure(failure)),
    };

    let ledger_text = state.drive_auditor(
        "requirement-auditor",
        super::super::prompts::REQUIREMENT_AUDITOR_SYSTEM,
        super::super::prompts::requirement_message(&state.request),
    )?;
    let ledger = match audit::parse_ledger(&ledger_text) {
        Ok(ledger) => ledger,
        Err(detail) => {
            return Ok(PassResult::Failure(ValidationFailure {
                reason: "requirement_audit_failed",
                detail,
            }));
        }
    };
    let gaps = audit::coverage_gaps(&state.request, &ledger, &parsed.intent);
    let hash = intent::approval_sha256(&reclassified, &formatted_test);
    let ledger_path = state
        .intent_path
        .parent()
        .expect("intent directory")
        .join("ledger.json");
    write_json(&ledger_path, &audit::encode_ledger(&hash, &ledger))?;
    let coverage_failure = if gaps.is_empty() {
        None
    } else if !state.coverage_repaired {
        Some(ValidationFailure {
            reason: "coverage_gap",
            detail: gaps.join("\n"),
        })
    } else {
        state.warnings.push(super::super::ShapeWarning {
            code: "coverage_gap".to_owned(),
            item_ids: ledger
                .iter()
                .filter(|row| row.maps_to.starts_with('A'))
                .map(|row| row.maps_to.clone())
                .collect(),
            message: gaps.join("; "),
        });
        None
    };

    let base_outputs = item_ids
        .iter()
        .map(|id| {
            format!(
                "{id}: {}",
                if results.get(id).copied().unwrap_or(false) {
                    "passed"
                } else {
                    "failed"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let audit_text = state.drive_auditor(
        "test-auditor",
        super::super::prompts::TEST_AUDITOR_SYSTEM,
        super::super::prompts::test_audit_message(
            &state.request,
            &reclassified,
            &formatted_test,
            &base_outputs,
        ),
    )?;
    let audit_items = match audit::audit_items(&audit_text) {
        Ok(items) => items,
        Err(detail) => {
            return Ok(PassResult::Failure(ValidationFailure {
                reason: "test_audit_failed",
                detail,
            }));
        }
    };
    let invalid = audit_items
        .iter()
        .filter(|item| item.verdict != "valid")
        .collect::<Vec<_>>();
    let repairable = invalid
        .iter()
        .filter(|item| audit::valid_citation(&item.citation, &state.request))
        .copied()
        .collect::<Vec<_>>();

    let audit_failure = if !repairable.is_empty() && !state.audit_repaired {
        state.audit_repaired = true;
        Some(
            repairable
                .iter()
                .map(|item| format!("{} {}: {}", item.id, item.verdict, item.reason))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        None
    };
    if coverage_failure.is_some() || audit_failure.is_some() {
        let mut detail = Vec::new();
        if let Some(failure) = coverage_failure {
            state.coverage_repaired = true;
            detail.push(format!("coverage_gap: {}", failure.detail));
        }
        if let Some(failure) = audit_failure {
            detail.push(format!("audit_over_strict: {failure}"));
        }
        return Ok(PassResult::Failure(ValidationFailure {
            reason: "validation_repair",
            detail: detail.join("\n"),
        }));
    }
    for item in invalid {
        state.warnings.push(super::super::ShapeWarning {
            code: format!("audit_{}", item.verdict),
            item_ids: vec![item.id.clone()],
            message: item.reason.clone(),
        });
    }
    let warnings_path = state
        .intent_path
        .parent()
        .expect("intent directory")
        .join("shape-warnings.json");
    write_json(
        &warnings_path,
        &audit::warnings_value(&hash, &state.warnings),
    )?;
    Ok(PassResult::Complete)
}

fn add_concerns(state: &mut RunState, text: &str) {
    let mut in_concerns = false;
    for line in text.lines() {
        if line.trim() == "Concerns:" {
            in_concerns = true;
            continue;
        }
        if !in_concerns {
            continue;
        }
        if let Some(concern) = line.strip_prefix("- ") {
            if state.concern_seen.insert(concern.to_owned()) {
                state.warnings.push(super::super::ShapeWarning {
                    code: "feasibility_concern".to_owned(),
                    item_ids: Vec::new(),
                    message: concern.to_owned(),
                });
            }
        } else if !line.trim().is_empty() {
            in_concerns = false;
        }
    }
}
