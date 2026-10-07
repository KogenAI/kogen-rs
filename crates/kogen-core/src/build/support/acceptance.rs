use super::super::approval::ApprovedBuild;
use super::super::config::BuildOptions;
use super::controller_error;
use crate::gate::{CheckBaseline, CheckFinding, CheckStatus};
use crate::run::ChildEnvironment;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn candidate_path(options: &BuildOptions, slug: &str) -> PathBuf {
    PathBuf::from(&options.acceptance_candidate_dir)
        .join(format!("{slug}{}", options.acceptance_extension))
}

pub(super) fn acceptance_request(
    options: &BuildOptions,
    approved: &ApprovedBuild,
    candidate_path: &Path,
    workspace: &Path,
    run_dir: &Path,
    environment: ChildEnvironment,
    report: &str,
) -> crate::gate::CommandAcceptanceRequest {
    let command = if options.adapter == "exunit" {
        crate::gate::adapters::exunit::write_formatter(run_dir)
            .and_then(|formatter| crate::gate::adapters::exunit::runner_command(&formatter, true))
            .unwrap_or_else(|_| options.acceptance_run.clone())
    } else if options.adapter == "rails" {
        crate::gate::adapters::rails::runner_command()
    } else {
        options.acceptance_run.clone()
    };
    crate::gate::CommandAcceptanceRequest {
        slug: approved.slug.clone(),
        command,
        candidate_path: candidate_path.to_path_buf(),
        workdir: workspace.to_path_buf(),
        run_dir: run_dir.to_path_buf(),
        report_path: run_dir.join("reports").join(report),
        env: environment,
        timeout: options.acceptance_timeout,
        expected_items: approved
            .intent
            .verify
            .iter()
            .map(|item| item.id.clone())
            .collect(),
        adapter_unavailable: false,
    }
}

pub(super) fn baseline(
    options: &BuildOptions,
    approved: &ApprovedBuild,
) -> Result<Vec<CheckBaseline>, crate::error::CoreError> {
    let rows = approved
        .approval
        .get("check_baseline")
        .and_then(Value::as_array)
        .ok_or_else(|| controller_error("approval_invalid", "check baseline is missing"))?;
    let mut result = Vec::with_capacity(options.checks.len());
    for check in &options.checks {
        let Some(row) = rows
            .iter()
            .find(|row| row.get("name").and_then(Value::as_str) == Some(&check.name))
        else {
            continue;
        };
        let status = match row.get("status").and_then(Value::as_str).unwrap_or("red") {
            "green" => CheckStatus::Green,
            "unavailable" => CheckStatus::Unavailable,
            "timeout" => CheckStatus::Timeout,
            "mutating" => CheckStatus::Mutating,
            _ => CheckStatus::Red,
        };
        let findings = row
            .get("findings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|finding| CheckFinding {
                path: finding
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                rule: finding
                    .get("rule")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                symbol: finding
                    .get("symbol")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                message: finding
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                line: None,
                column: None,
            })
            .collect();
        result.push(CheckBaseline {
            name: check.name.clone(),
            status,
            exit_status: row
                .get("exit_status")
                .and_then(Value::as_i64)
                .map(|value| value as i32),
            findings,
        });
    }
    Ok(result)
}
