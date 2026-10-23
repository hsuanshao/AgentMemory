//! `memory-server reembed`: adds embeddings to atoms stored without one,
//! e.g. after the embedding service was down. memory-core also does this in
//! the background (`memory.embedding.backfillIntervalSecs`); this runs it
//! now and reports. Safe while memory-core runs: an atom is only given an
//! embedding it does not have, and is not rewritten.

use anyhow::{bail, Result};
use memory_application::memory::backfill::{BackfillReport, EmbeddingBackfill};
use memory_domain::scope::DepartmentId;

use crate::backends::build_storage;
use crate::config::{expand_home, Config};
use crate::ops_cli::Args;
use crate::wiring;

const USAGE: &str = "\
memory-server reembed [--department ID] [--limit N] [--json]

Adds embeddings to atoms stored without one (the embedding call failed or
timed out), oldest first, in every department or one. --limit caps the atoms
embedded per department (default: all). Stops at a department whose
embedding calls fail.";

/// Atoms read per round; rounds repeat until none are left.
const ROUND: usize = 200;

pub async fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let mut args = Args::new(args);
    if args.flag("-h") || args.flag("--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let department = args.value("--department")?;
    let limit = args.number("--limit")?.map(|n| n as usize);
    let json = args.flag("--json");
    args.finish()?;

    let config = Config::load()?;
    let embedder = wiring::embedder(&config);
    if embedder.dimensions().is_none() {
        bail!("embeddings are off (memory.embedding): nothing to do");
    }
    let data_dir = expand_home(&config.data.base_dir);
    let storage = build_storage(&config, &data_dir, embedder.dimensions()).await?;
    let backfill = EmbeddingBackfill::new(storage.atoms.clone(), embedder);
    let departments = match department {
        Some(d) => vec![DepartmentId::parse("--department", &d)?],
        None => storage.atoms.departments().await?,
    };

    let mut rows = Vec::new();
    for department in departments {
        let report = until_done(&backfill, &department, limit).await?;
        rows.push((department.to_string(), report));
    }
    if json {
        let rows: Vec<_> = rows
            .iter()
            .map(|(d, r)| serde_json::json!({"department": d, "embedded": r.embedded, "skipped": r.skipped}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for (department, r) in &rows {
            match r.skipped {
                0 => println!("{department}: {} embedded", r.embedded),
                n => println!(
                    "{department}: {} embedded, {n} not (the embedding service failed or they changed)",
                    r.embedded
                ),
            }
        }
    }
    Ok(())
}

/// Rounds until no atom is missing an embedding, a round embeds none (the
/// service fails), or `limit` is reached.
async fn until_done(
    backfill: &EmbeddingBackfill,
    department: &DepartmentId,
    limit: Option<usize>,
) -> Result<BackfillReport> {
    let mut total = BackfillReport::default();
    loop {
        let round = limit.map_or(ROUND, |l| ROUND.min(l - total.embedded));
        let report = backfill.department(department, round).await?;
        total.embedded += report.embedded;
        // What the last round could not embed; earlier rounds' leftovers
        // were retried.
        total.skipped = report.skipped;
        let reached = limit.is_some_and(|l| total.embedded >= l);
        if report.missing == 0 || report.embedded == 0 || reached {
            return Ok(total);
        }
    }
}
