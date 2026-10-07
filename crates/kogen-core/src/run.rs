//! Durable run journal and process custody.
//!
//! `ProcessPort` is the shared production seam for approval checks, gate
//! checks, acceptance adapters, and tools. Callers provide the exact argv and
//! exact child environment; this module never invokes a shell implicitly.

mod diagnostics;
mod environment;
pub mod orchestration;
mod persistence;
mod process;
mod sandbox;
mod script;
pub mod setup_cache;
mod watchdog;

pub(crate) use diagnostics::{failure_detail, prepare_private_run_dir};
pub use environment::{
    EnvironmentError, EnvironmentMap, EnvironmentRequest, build_child_environment, host_environment,
};
pub use persistence::{LandingRecord, RunEvent, RunPersistenceError, RunSnapshot, RunStore};
pub(crate) use process::run_bounded_command;
pub use process::{
    ChildEnvironment, DEFAULT_PROCESS_TIMEOUT, OUTPUT_TAIL_BYTES, ProcessError, ProcessPort,
    ProcessRequest, ProcessResult, ProcessSupervisor, SandboxObservation, SandboxStatus,
    StdinSource,
};
pub use sandbox::{SandboxIntegrityPort, SandboxPolicy, SandboxedProcessPort};
pub use script::{DEFAULT_SHELL_TIMEOUT, ScriptError, run_private_script};
