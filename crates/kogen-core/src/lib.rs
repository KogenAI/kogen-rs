//! Shared production policy for the public CLI and the private trace adapter.
//! Modules are added by the work packages in docs/work/QUEUE.txt.

pub mod approval;
pub mod build;
pub mod error;
pub mod gate;
pub mod git;
pub mod intent;
pub mod project;
pub mod provider;
pub mod queue;
pub mod recovery;
pub mod run;
pub mod status;

mod safe_fs;

/// The public CLI's exit classes, from spec/01-cli.md §1.5.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum ExitCode {
    Done = 0,
    Negative = 1,
    Usage = 2,
    Environment = 3,
    Provider = 4,
    Decision = 5,
    Bug = 70,
    Interrupted = 130,
    Terminated = 143,
}

impl ExitCode {
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}
