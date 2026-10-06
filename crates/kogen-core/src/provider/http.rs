//! ChatGPT Responses request construction and retry policy.

mod client;
pub mod retry;
mod wire;

pub use client::{
    ClockPort, HttpAttempt, HttpPort, ProviderCall, ProviderCallFailure, RequestDeadlines,
    RequestEvent, RequestPolicy, ReqwestPort, SystemClock, respond,
};
pub use wire::{
    ApiMode, RequestContext, ResponseMode, WireConfig, WireRequest, build_wire_request,
    validate_generation_cap,
};
