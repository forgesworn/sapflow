//! Runtime-free broadcaster primitives for native compressed media adapters.
//!
//! The caller owns scheduling, transports, destination authorisation and files.
//! No public method performs network or file I/O, or starts a background task.

pub mod media;
pub mod mpegts;
pub mod queue;
pub mod session;
pub mod srt;

pub use media::{AacConfig, CodecConfig, EncodedSample, H264Config, SampleTiming, SharedSample};

#[cfg(test)]
mod tests;
