//! Semantic stream lifecycle and event adapters.
//!
//! This module translates event payloads only. SSE framing remains in
//! `crate::sse`, while the gateway owns transport, retries and backpressure.

mod adapters;
mod aggregator;
mod decode;
mod usage;

#[cfg(test)]
mod tests;

pub use adapters::{
    AnthropicStreamAdapter, GeminiStreamAdapter, OpenAiStreamAdapter, outbound_stream,
    supports_prefill_continuation,
};
pub use aggregator::{StreamAggregator, StreamLifecycle, StreamStatus, StreamViolation};
pub use decode::{
    DecodedFrame, DecodedSnapshot, decode_upstream_frame, responses_done_arguments,
    responses_function_call_id,
};
