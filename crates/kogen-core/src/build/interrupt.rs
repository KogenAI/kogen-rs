use crate::error::CoreError;
use crate::run::{RunEvent, RunStore};
use serde_json::json;
use signal_hook::consts::{SIGINT, SIGTERM};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) struct InterruptMonitor {
    done: Arc<AtomicBool>,
    term: signal_hook::SigId,
    interrupt: signal_hook::SigId,
    worker: Option<JoinHandle<()>>,
}

impl InterruptMonitor {
    pub fn install(store: RunStore) -> Result<Self, CoreError> {
        let term_flag = Arc::new(AtomicBool::new(false));
        let interrupt_flag = Arc::new(AtomicBool::new(false));
        let term = signal_hook::flag::register(SIGTERM, Arc::clone(&term_flag))
            .map_err(|error| interrupt_error(error.to_string()))?;
        let interrupt = match signal_hook::flag::register(SIGINT, Arc::clone(&interrupt_flag)) {
            Ok(id) => id,
            Err(error) => {
                signal_hook::low_level::unregister(term);
                return Err(interrupt_error(error.to_string()));
            }
        };
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&done);
        let worker = thread::Builder::new()
            .name("kogen-build-interrupt".to_owned())
            .spawn(move || {
                while !worker_done.load(Ordering::Relaxed) {
                    if term_flag.swap(false, Ordering::Relaxed) {
                        record_interrupt(&store, "sigterm", 143);
                    }
                    if interrupt_flag.swap(false, Ordering::Relaxed) {
                        record_interrupt(&store, "sigint", 130);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            })
            .map_err(|error| {
                signal_hook::low_level::unregister(term);
                signal_hook::low_level::unregister(interrupt);
                interrupt_error(error.to_string())
            })?;
        Ok(Self {
            done,
            term,
            interrupt,
            worker: Some(worker),
        })
    }
}

impl Drop for InterruptMonitor {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        signal_hook::low_level::unregister(self.term);
        signal_hook::low_level::unregister(self.interrupt);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn record_interrupt(store: &RunStore, reason: &str, code: i32) -> ! {
    if let Ok(snapshot) = store.read_snapshot() {
        let event = RunEvent::new("interrupted", now_ms()).with("reason", json!(reason));
        let _ = store.record(&event, &snapshot);
    }
    std::process::exit(code)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn interrupt_error(detail: impl Into<String>) -> CoreError {
    CoreError::new(
        crate::error::ErrorClass::Environment,
        "signal_handler_unavailable",
        detail,
        crate::ExitCode::Environment,
    )
}
