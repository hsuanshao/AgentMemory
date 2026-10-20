//! `memory-server migrate`: brings stored data to the version this build
//! expects, then exits. memory-core does the same at start-up; running it
//! first lets an operator see the result before the service comes back, and
//! migrates every SQLite department now rather than on first use.

use anyhow::{bail, Context, Result};
use memory_infrastructure::sqlite::db::Db;
use memory_infrastructure::sqlite::schema;

use crate::backends::{build_coordination, build_storage};
use crate::config::{expand_home, Config};
use crate::wiring;

const USAGE: &str = "\
usage: memory-server migrate

Migrates the data directory, the storage backend and the coordination
backend named by the configuration to this version. Stop every memory-core
replica of an older version first. Safe to repeat.";

pub async fn run(args: impl Iterator<Item = String>) -> Result<()> {
    if let Some(arg) = args.into_iter().next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("unknown option `{other}`\n\n{USAGE}"),
        }
    }
    let config = Config::load()?;
    let data_dir = expand_home(&config.data.base_dir);
    std::fs::create_dir_all(&data_dir)?;
    memory_infrastructure::fs::migrate_data_dir(&data_dir)?;

    build_storage(&config, &data_dir, wiring::vector_dims(&config)).await?;
    let mut departments = 0;
    if config.storage.backend == "sqlite" {
        let root = data_dir.join("departments");
        if root.is_dir() {
            for entry in std::fs::read_dir(&root)? {
                let db = entry?.path().join("memory.db");
                if db.is_file() {
                    Db::open(&db, schema::migrate_department)
                        .with_context(|| format!("migrating {}", db.display()))?;
                    departments += 1;
                }
            }
        }
    }
    build_coordination(&config, &data_dir).await?;

    println!(
        "storage `{}` and coordination `{}` are at the current version{}",
        config.storage.backend,
        config.coordination.backend,
        if departments > 0 {
            format!(" ({departments} SQLite departments)")
        } else {
            String::new()
        }
    );
    Ok(())
}
