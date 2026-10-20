//! PostgreSQL + pgvector adapters: one schema per department, `tsvector`
//! keyword search and HNSW vector search. Observable behaviour (ordering,
//! pagination, counts, cascades, versioning) matches the SQLite adapters.

pub mod db;
pub mod lifecycle;
pub mod memory_store;
pub mod meta_store;
pub mod pipeline_store;
pub mod schema;
pub mod skill_store;
pub mod sql;
