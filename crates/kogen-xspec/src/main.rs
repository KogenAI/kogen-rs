mod xspec;

use kogen_core::provider::http::retry::RetryReplay;
use kogen_core::provider::session::replay::SessionReplay;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, BufRead, Write};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(slice) = args.next() else {
        legacy_fail("expected a slice name");
    };
    if args.next().is_some() {
        if matches!(slice.as_str(), "intent" | "approve" | "queue") {
            fail("expected exactly one private slice name");
        }
        legacy_fail("expected exactly one slice name");
    }

    match slice.as_str() {
        "stream" => run_replay(RetryReplay::default()),
        "session" => run_replay(SessionReplay::default()),
        "intent" | "approve" | "queue" => run_private(&slice),
        _ => legacy_fail(&format!("unsupported slice {slice:?}")),
    }
}

fn run_replay<S: Transition>(state: S) {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    if let Err(error) = replay(stdin.lock(), &mut stdout, state) {
        legacy_fail(&error);
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

fn run_private(slice: &str) {
    let mut adapter = match xspec::Adapter::new(slice) {
        Ok(adapter) => adapter,
        Err(error) => fail(&error),
    };
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => fail(&format!("read protocol line: {error}")),
        };
        let request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => fail(&format!("invalid JSON request: {error}")),
        };
        let observation = match adapter.handle(request) {
            Ok(observation) => observation,
            Err(error) => fail(&error),
        };
        if serde_json::to_writer(&mut stdout, &observation).is_err()
            || stdout.write_all(b"\n").is_err()
            || stdout.flush().is_err()
        {
            fail("write protocol observation");
        }
    }
}

fn legacy_fail(message: &str) -> ! {
    eprintln!("kogen-xspec: {message}");
    std::process::exit(1);
}

fn fail(message: &str) -> ! {
    eprintln!("kogen-xspec: {message}");
    std::process::exit(kogen_core::ExitCode::Bug.as_i32());
}
