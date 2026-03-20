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
//!  ┌──────────────────┐     WebSocket        ┌─────────────────────┐
//!  │   WsTransport    │◄────────────────────►│  Session SDK URL    │
//!  │  (send/recv      │   stream-json         │  (user ↔ handler)   │
//!  │   Event stream)  │                      └─────────────────────┘
//!  └──────────────────┘
//!
//!  ┌──────────────────┐
//!  │   JsonLineCodec  │   Encoder/Decoder for raw JSONL byte streams
//!  │  (EventCodec)    │   (use with Framed<T, EventCodec>)
//!  └──────────────────┘
//! ```
//!
//! # Quick start
//!
//! See the `echo-server` crate for a minimal working example.

pub mod client;
pub mod codec;
pub mod config;
pub mod sse;
pub mod transport;
pub mod types;

pub use client::BridgeClient;
pub use codec::{EventCodec, JsonLineCodec};
pub use config::BridgeConfig;
pub use sse::SseTransport;
pub use transport::WsTransport;
pub use types::Event;
