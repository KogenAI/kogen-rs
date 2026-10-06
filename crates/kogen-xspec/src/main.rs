use kogen_core::provider::http::retry::RetryReplay;
use kogen_core::provider::session::replay::SessionReplay;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, BufRead, Write};

fn main() {
    if let Err(error) = run() {
        eprintln!("kogen-xspec: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let slice = args
        .next()
        .ok_or_else(|| "expected a slice name".to_owned())?;
    if args.next().is_some() {
        return Err("expected exactly one slice name".to_owned());
    }
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    match slice.as_str() {
        "stream" => replay(stdin.lock(), &mut stdout, RetryReplay::default()),
        "session" => replay(stdin.lock(), &mut stdout, SessionReplay::default()),
        _ => Err(format!("unsupported slice {slice:?}")),
    }
}

trait Transition: Default + Serialize {
    fn apply(&mut self, tag: &str, value: Option<&Value>);
    fn reset(&mut self) {
        *self = Self::default();
    }
    fn known_tag(tag: &str) -> bool;
}

impl Transition for RetryReplay {
    fn apply(&mut self, tag: &str, value: Option<&Value>) {
        RetryReplay::apply(self, tag, value);
    }

    fn known_tag(tag: &str) -> bool {
        matches!(tag, "Init" | "Open" | "Result" | "Checkpoint" | "SetWaited")
    }
}

impl Transition for SessionReplay {
    fn apply(&mut self, tag: &str, value: Option<&Value>) {
        SessionReplay::apply(self, tag, value);
    }

    fn known_tag(tag: &str) -> bool {
        matches!(
            tag,
            "Init"
                | "Bind"
                | "Turn"
                | "Repair"
                | "Model"
                | "Stage"
                | "Attempt"
                | "Rung"
                | "Epoch"
                | "Accept"
                | "Previous"
                | "Lite"
                | "NewRun"
        )
    }
}

fn replay<R, W, S>(reader: R, writer: &mut W, mut state: S) -> Result<(), String>
where
    R: BufRead,
    W: Write,
    S: Transition,
{
    for (line_number, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| format!("read line {}: {error}", line_number + 1))?;
        let request: Value = serde_json::from_str(&line)
            .map_err(|error| format!("invalid JSON on line {}: {error}", line_number + 1))?;
        match request.get("op").and_then(Value::as_str) {
            Some("reset") => state.reset(),
            Some("apply") => {
                let event = request
                    .get("event")
                    .ok_or_else(|| format!("missing event on line {}", line_number + 1))?;
                let tag = event
                    .get("tag")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("missing event tag on line {}", line_number + 1))?;
                if !S::known_tag(tag) {
                    return Err(format!(
                        "unknown event tag {tag:?} on line {}",
                        line_number + 1
                    ));
                }
                state.apply(tag, event.get("value"));
            }
            Some(operation) => {
                return Err(format!(
                    "unsupported operation {operation:?} on line {}",
                    line_number + 1
                ));
            }
            None => return Err(format!("missing operation on line {}", line_number + 1)),
        }
        serde_json::to_writer(&mut *writer, &state)
            .map_err(|error| format!("encode observation: {error}"))?;
        writer.write_all(b"\n").map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}
