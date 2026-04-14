//! Bridge protocol library for Claude Code remote control.
//!
//! This crate implements the client side of the Claude Code bridge protocol,
//! allowing programs to register as bridge environments and handle remote
//! sessions.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────────┐        HTTP          ┌─────────────────────┐
//!  │   BridgeClient   │◄────────────────────►│  Anthropic Sessions │
//!  │  (register/poll/ │   POST/GET/DELETE     │        API          │
//!  │   ack/heartbeat) │                      └─────────────────────┘
//!  └────────┬─────────┘
//!           │ work item arrives
//!           ▼
//!  ┌──────────────────┐        SSE           ┌─────────────────────┐
//!  │   SseTransport   │◄────────────────────►│  Worker Events      │
//!  │  (recv events)   │   GET event stream    │  Stream             │
//!  └──────────────────┘                      └─────────────────────┘
//! ```

pub mod client;
pub mod config;
pub mod oauth;
pub mod sse;
pub mod types;

pub use client::BridgeClient;
pub use config::BridgeConfig;
pub use sse::SseTransport;
pub use types::Event;
