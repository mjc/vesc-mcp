//! Core types and MCP tool wiring for the vesc-mcp server.

pub mod error;
pub mod server;

pub use error::{CoreError, CoreResult};
pub use server::VescMcpService;
