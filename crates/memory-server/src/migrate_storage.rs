//! `memory-server migrate-storage`: copies the SQLite data of every
//! instance into the Postgres database named by `AGENT_MEMORY_POSTGRES_URL`.
//! Run it with memory-core stopped, then switch
//! `AGENT_MEMORY_STORAGE_BACKEND` to `postgres`. `migrate-to-postgres.sh`
//! does all of that; docs/MIGRATE-TO-POSTGRES.md has the manual steps.

use anyhow::{bail, Context, Result};
use memory_infrastructure::postgres::db::PgDb;
use memory_infrastructure::sqlite_to_postgres::{self, MigrationReport, Outcome};

use crate::config::{expand_home, Config};
use crate::wiring;

const USAGE: &str = "\
usage: memory-server migrate-storage [--dry-run] [--json]

Copies every SQLite instance under the data directory into the Postgres
database at AGENT_MEMORY_POSTGRES_URL. Stop memory-core first.

  --dry-run  report what would be copied; write nothing
  --json     print the report as JSON";

pub async fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let (mut dry_run, mut json) = (false, false);
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("unknown option `{other}`\n\n{USAGE}"),
        }
    }

    let config = Config::load()?;
    let url = &config.storage.postgres.url;
    if url.is_empty() {
        bail!("set AGENT_MEMORY_POSTGRES_URL (storage.postgres.url) to the target database");
    }
    let data_dir = expand_home(&config.data.base_dir);
    // The same dimension the server will use, so the embedding column
    // matches and stored vectors carry over.
    let dims = wiring::vector_dims(&config);
    let pg = PgDb::connect(url, config.storage.postgres.max_connections, dims)
        .await
        .context("connecting to the target database")?;
    let report = sqlite_to_postgres::migrate(&data_dir, &pg, dry_run)
        .await
        .context(
        "migration stopped; instances copied before the error stay in Postgres and a re-run skips them",
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_summary(&report, &data_dir.display().to_string());
    }
    Ok(())
}

fn print_summary(report: &MigrationReport, data_dir: &str) {
    if report.instances.is_empty() {
        println!("No SQLite instances under {data_dir}/instances; nothing to copy.");
        return;
    }
    for i in &report.instances {
        let state = match (i.outcome, report.dry_run) {
            (Outcome::AlreadyMigrated, _) => "already migrated, skipped",
            (Outcome::Copied, true) => "would be copied",
            (Outcome::Copied, false) => "copied and verified",
        };
        println!("instance {}: {state}", i.instance);
        for t in i.tables.iter().filter(|t| t.source > 0) {
            println!("  {:<24} {:>9}", t.table, t.source);
        }
        if i.embeddings_copied + i.embeddings_dropped > 0 {
            println!(
                "  embeddings: {} kept, {} dropped (embeddings off or another dimension)",
                i.embeddings_copied, i.embeddings_dropped
            );
        }
    }
    if report.dry_run {
        println!("Dry run: nothing was written.");
    } else {
        println!("Done. Set AGENT_MEMORY_STORAGE_BACKEND=postgres and start memory-core.");
    }
}
