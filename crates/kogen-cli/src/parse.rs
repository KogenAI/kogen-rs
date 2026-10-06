use std::path::Path;

use crate::arguments::{
    build_command, collect_positionals, missing_positional, scan_options, unexpected_positional,
};
use crate::help::HelpPage;
use crate::moved::find_moved_form;
use crate::request::{ParsedRequest, Route, UsageError};

/// Parse the fixed CLI tree and argument grammar. Slug shape is left to the
/// command handler, as required by spec/01-cli.md §1.3.
#[must_use]
pub fn parse(args: &[String], cwd: &Path) -> ParsedRequest {
    if let Some(message) = find_moved_form(args) {
        return ParsedRequest::Moved(message);
    }
    if args.is_empty() {
        return ParsedRequest::Help(HelpPage::Top);
    }

    let (route, tail, page) = match args[0].as_str() {
        "help" => {
            if args.len() == 1 {
                return ParsedRequest::Help(HelpPage::Top);
            }
            return usage(
                format!("kogen help: unexpected argument '{}'", args[1]),
                HelpPage::Top,
            );
        }
        "status" => (Route::Status, &args[1..], HelpPage::Status),
        "version" => (Route::Version, &args[1..], HelpPage::Version),
        "intent" => match args.get(1).map(String::as_str) {
            None => return ParsedRequest::Help(HelpPage::Intent),
            Some("shape") => (Route::IntentShape, &args[2..], HelpPage::IntentShape),
            Some("approve") => (Route::IntentApprove, &args[2..], HelpPage::IntentApprove),
            Some("remove") => (Route::IntentRemove, &args[2..], HelpPage::IntentRemove),
            Some(subcommand) => {
                return usage(
                    format!("kogen intent: unknown command '{subcommand}'"),
                    HelpPage::Intent,
                );
            }
        },
        "queue" => match args.get(1).map(String::as_str) {
            None => return ParsedRequest::Help(HelpPage::Queue),
            Some("start") => (Route::QueueStart, &args[2..], HelpPage::QueueStart),
            Some("stop") => (Route::QueueStop, &args[2..], HelpPage::QueueStop),
            Some(subcommand) => {
                return usage(
                    format!("kogen queue: unknown command '{subcommand}'"),
                    HelpPage::Queue,
                );
            }
        },
        "provider" => match args.get(1).map(String::as_str) {
            None => return ParsedRequest::Help(HelpPage::Provider),
            Some("list") => (Route::ProviderList, &args[2..], HelpPage::ProviderList),
            Some("login") => (Route::ProviderLogin, &args[2..], HelpPage::ProviderLogin),
            Some("logout") => (Route::ProviderLogout, &args[2..], HelpPage::ProviderLogout),
            Some("use") => (Route::ProviderUse, &args[2..], HelpPage::ProviderUse),
            Some(subcommand) => {
                return usage(
                    format!("kogen provider: unknown command '{subcommand}'"),
                    HelpPage::Provider,
                );
            }
        },
        unknown => {
            return usage(format!("kogen: unknown command '{unknown}'"), HelpPage::Top);
        }
    };

    parse_route(route, tail, page, cwd)
}

fn parse_route(route: Route, tail: &[String], page: HelpPage, cwd: &Path) -> ParsedRequest {
    let options = scan_options(route, tail);
    if let Some(flag) = options.boolean_value.as_deref() {
        return usage(
            format!("kogen {}: {flag} takes no value", route.path()),
            page,
        );
    }
    if let Some(option) = options.unknown_option.as_deref() {
        return usage(
            format!("kogen {}: unknown option '{option}'", route.path()),
            page,
        );
    }
    if let Some(option) = options.missing_value.as_deref() {
        return usage(
            format!("kogen {}: {option} needs a value", route.path()),
            page,
        );
    }

    let positional = collect_positionals(tail);
    if let Some(name) = missing_positional(route, positional.len()) {
        return usage(format!("kogen {}: missing {name}", route.path()), page);
    }
    if let Some(unexpected) = unexpected_positional(route, &positional) {
        return usage(
            format!("kogen {}: unexpected argument '{unexpected}'", route.path()),
            page,
        );
    }
    if let Some(option) = options.disallowed_option.as_deref() {
        return usage(
            format!("kogen {}: unknown option '{option}'", route.path()),
            page,
        );
    }

    match build_command(route, positional, options, cwd) {
        Ok(command) => ParsedRequest::Command(command),
        Err(message) => usage(message, page),
    }
}

fn usage(message: String, page: HelpPage) -> ParsedRequest {
    ParsedRequest::Usage(UsageError { message, page })
}
