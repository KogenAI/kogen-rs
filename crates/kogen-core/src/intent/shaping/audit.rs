//! Requirement ledger, coverage and auditor reply decoding.

use super::ShapeWarning;
use crate::intent::Intent;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize)]
pub(super) struct LedgerRow {
    pub constraint: String,
    pub maps_to: String,
}

#[derive(Clone, Debug)]
pub(super) struct AuditItem {
    pub id: String,
    pub verdict: String,
    pub citation: String,
    pub reason: String,
}

pub(super) fn coverage_gaps(request: &[u8], rows: &[LedgerRow], intent: &Intent) -> Vec<String> {
    let request = String::from_utf8_lossy(request);
    let constraints = rows
        .iter()
        .map(|row| row.constraint.as_str())
        .collect::<Vec<_>>();
    let mut gaps = Vec::new();
    for value in request_literals(&request) {
        if !constraints
            .iter()
            .any(|constraint| constraint.contains(&value))
        {
            gaps.push(value);
        }
    }
    let valid_ids = intent
        .acceptance
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    for row in rows {
        if !row.maps_to.starts_with("untestable: ") && !valid_ids.contains(row.maps_to.as_str()) {
            gaps.push(format!(
                "{} maps to missing item {}",
                row.constraint, row.maps_to
            ));
        }
    }
    gaps.sort();
    gaps.dedup();
    gaps
}

pub(super) fn parse_ledger(text: &str) -> Result<Vec<LedgerRow>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| "requirement auditor returned invalid JSON".to_owned())?;
    let rows = value
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "requirement auditor reply must contain rows".to_owned())?;
    rows.iter()
        .map(|row| {
            let constraint = row
                .get("constraint")
                .and_then(Value::as_str)
                .ok_or_else(|| "ledger row is missing constraint".to_owned())?;
            let maps_to = row
                .get("maps_to")
                .and_then(Value::as_str)
                .ok_or_else(|| "ledger row is missing maps_to".to_owned())?;
            Ok(LedgerRow {
                constraint: constraint.to_owned(),
                maps_to: maps_to.to_owned(),
            })
        })
        .collect()
}

pub(super) fn encode_ledger(approval_sha: &str, rows: &[LedgerRow]) -> Value {
    json!({"approval_sha256":approval_sha,"rows":rows})
}

pub(super) fn audit_items(text: &str) -> Result<Vec<AuditItem>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| "acceptance test auditor returned invalid JSON".to_owned())?;
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "acceptance test auditor reply must contain items".to_owned())?;
    items
        .iter()
        .map(|item| {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "audit item is missing id".to_owned())?;
            let verdict = item
                .get("verdict")
                .and_then(Value::as_str)
                .ok_or_else(|| "audit item is missing verdict".to_owned())?;
            Ok(AuditItem {
                id: id.to_owned(),
                verdict: verdict.to_owned(),
                citation: item
                    .get("citation")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                reason: item
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect()
}

pub(super) fn warnings_value(approval_sha: &str, warnings: &[ShapeWarning]) -> Value {
    json!({"approval_sha256":approval_sha,"warnings":warnings})
}

pub(super) fn valid_citation(citation: &str, request: &[u8]) -> bool {
    !citation.is_empty() && String::from_utf8_lossy(request).contains(citation)
}

fn request_literals(request: &str) -> Vec<String> {
    let bytes = request.as_bytes();
    let mut values = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            values.push(request[start..index].to_owned());
            continue;
        }
        if matches!(bytes[index], b'`' | b'\'' | b'"') {
            let delimiter = bytes[index];
            if let Some(relative_end) = bytes[index + 1..]
                .iter()
                .position(|byte| *byte == delimiter)
            {
                let start = index + 1;
                let end = start + relative_end;
                if start < end {
                    values.push(request[start..end].to_owned());
                }
                index = end + 1;
                continue;
            }
        }
        index += 1;
    }
    values.sort();
    values.dedup();
    values
}
