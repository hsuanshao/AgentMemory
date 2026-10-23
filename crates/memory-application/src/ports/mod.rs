//! Ports: the interfaces use cases depend on. Infrastructure adapters
//! implement them; tests use in-memory fakes.

pub mod audit;
pub mod coordination;
pub mod memory;
pub mod meta;
pub mod pipeline;
pub mod skill;
pub mod system;
