//! `amem`: the Agent Memory client. One binary serves every host:
//! Claude Code and Antigravity call it from hooks, Hermes from its memory
//! provider, and any MCP-capable agent through `amem mcp`.

pub mod api;
pub mod capture;
pub mod config;
pub mod hooks;
pub mod mcp;
pub mod queue;
pub use memory_redact as redact;
pub mod transcript;
