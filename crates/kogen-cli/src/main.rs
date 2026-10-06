mod arguments;
mod handlers;
mod help;
mod moved;
mod parse;
mod request;

use std::io::Write as _;
use std::path::Path;

use kogen_core::ExitCode;
use kogen_core::error::CliOutput;

use crate::parse::parse;
use crate::request::ParsedRequest;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let output = run(&args, &cwd);
    let _ = std::io::stdout().write_all(output.stdout.as_bytes());
    let _ = std::io::stderr().write_all(output.stderr.as_bytes());
    std::process::exit(output.exit_code.as_i32());
}

fn run(args: &[String], cwd: &Path) -> CliOutput {
    match parse(args, cwd) {
        ParsedRequest::Help(page) => CliOutput::success(page.contents()),
        ParsedRequest::Moved(message) => CliOutput {
            stdout: format!("kogen: moved: use {message}\n"),
            stderr: String::new(),
            exit_code: ExitCode::Usage,
        },
        ParsedRequest::Usage(error) => CliOutput {
            stdout: error.render(),
            stderr: String::new(),
            exit_code: ExitCode::Usage,
        },
        ParsedRequest::Command(command) => handlers::dispatch(command),
    }
}
