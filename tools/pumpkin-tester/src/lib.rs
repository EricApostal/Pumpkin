//! Shared harness for the `pumpkin-tester` binary and its integration tests.
//!
//! The binary drives the existing bot/GameTest suites; integration tests (such as the
//! client-visible packet smoke test) link this library to boot a server, join a headless
//! bot, and inspect the packets the bot actually received.

pub mod bot;
pub mod client_trace;
pub mod harness;
pub mod mctrace;
pub mod packet_capture;
pub mod recorder;
pub mod scenario;
