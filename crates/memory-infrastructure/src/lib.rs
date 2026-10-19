//! Frameworks & drivers: SQLite, PostgreSQL, filesystem, HTTP clients and background
//! workers implementing the application ports.

pub mod cli_llm;
pub mod fs;
pub mod lifecycle;
pub mod local_lock;
pub mod openai;
pub mod postgres;
pub mod redis;
pub mod sqlite;
pub mod sqlite_to_postgres;
pub mod system;
pub mod text;
pub mod worker;
