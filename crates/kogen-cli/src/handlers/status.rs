//! Public status formatting and watch polling.

mod render;
mod report;

use kogen_core::ExitCode;
use kogen_core::error::{CliOutput, CoreError, ErrorClass};
use kogen_core::project::ProjectResolution;
use kogen_core::status::{StatusKind, StatusReport, inspect};
use std::io::Write as _;
use std::thread;
use std::time::Duration;

pub(super) fn command(
    project: &ProjectResolution,
    slug: Option<&str>,
    watch: bool,
    json_output: bool,
) -> CliOutput {
    if watch {
        return watch_command(project, slug);
    }
    let report = match inspect(project) {
        Ok(report) => report,
        Err(error) => return error.into_cli_output(),
    };
    if let Some(slug) = slug {
        let Some(intent) = report
            .board
            .intents
            .iter()
            .find(|intent| intent.facts.slug == slug)
        else {
            return not_found();
        };
        if json_output {
            return CliOutput::success(format!(
                "{}\n",
                report::build_report(intent, &report, project)
            ));
        }
        return CliOutput::success(render::render_detail(intent, &report, project));
    }
    if json_output {
        return CliOutput::success(render::render_json_lines(&report));
    }
    CliOutput::success(render::render_overview(&report))
}

fn watch_command(project: &ProjectResolution, slug: Option<&str>) -> CliOutput {
    let mut previous = None;
    loop {
        let report = match inspect(project) {
            Ok(report) => report,
            Err(error) => return error.into_cli_output(),
        };
        if let Some(slug) = slug
            && !report
                .board
                .intents
                .iter()
                .any(|intent| intent.facts.slug == slug)
        {
            return not_found();
        }
        let frame = slug.map_or_else(
            || render::render_overview(&report),
            |slug| {
                let intent = report
                    .board
                    .intents
                    .iter()
                    .find(|intent| intent.facts.slug == slug)
                    .expect("checked above");
                render::render_detail(intent, &report, project)
            },
        );
        if previous.as_deref() != Some(frame.as_str()) {
            let mut stdout = std::io::stdout().lock();
            if previous.is_some() {
                let _ = stdout.write_all(b"\n");
            }
            if stdout
                .write_all(frame.as_bytes())
                .and_then(|()| stdout.flush())
                .is_err()
            {
                return internal_status_error("could not write status frame");
            }
            previous = Some(frame);
        }
        let idle = report.queue_pid.is_none()
            && !report
                .board
                .intents
                .iter()
                .any(|intent| intent.kind == StatusKind::Building)
            && !has_busy_agents(&report);
        if idle {
            if let Some(slug) = slug {
                return CliOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: if report.board.intents.iter().any(|intent| {
                        intent.facts.slug == slug && intent.kind == StatusKind::Landed
                    }) {
                        ExitCode::Done
                    } else {
                        ExitCode::Negative
                    },
                };
            }
            return CliOutput::success("");
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn has_busy_agents(report: &StatusReport) -> bool {
    report
        .agents
        .iter()
        .any(|agent| matches!(agent.status.as_str(), "running" | "waiting"))
}

fn not_found() -> CliOutput {
    CoreError::new(
        ErrorClass::Intent,
        "not_found",
        "Intent does not exist",
        ExitCode::Usage,
    )
    .into_cli_output()
}

fn internal_status_error(detail: &str) -> CliOutput {
    CoreError::new(
        ErrorClass::Controller,
        "internal_error",
        detail,
        ExitCode::Bug,
    )
    .into_cli_output()
}
