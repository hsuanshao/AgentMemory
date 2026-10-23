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
timed out), in every department or one: one pass over each department's
atoms without an embedding, oldest first. An atom the service refuses is
skipped and counted; when several in a row are refused, the service is taken
as down and the department is left for later. --limit caps the atoms
embedded per department (default: all).";

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
            .map(|(d, r)| serde_json::json!({"department": d, "report": r}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for (department, r) in &rows {
            let mut line = format!("{department}: {} embedded", r.embedded);
            if r.skipped > 0 {
                line += &format!(
                    ", {} skipped (refused by the service, or changed)",
                    r.skipped
                );
            }
            if r.service_down {
                line += "; stopped: the embedding service keeps failing";
            }
            if r.misconfigured {
                line += "; stopped: the embedding service refuses every input, check memory.embedding model and dimensions";
            }
            println!("{line}");
        }
    }
    Ok(())
}

/// Rounds until the department's queue has been walked once, the embedding
/// service is down, or `limit` atoms are embedded.
async fn until_done(
    backfill: &EmbeddingBackfill,
    department: &DepartmentId,
    limit: Option<usize>,
) -> Result<BackfillReport> {
    let mut total = BackfillReport::default();
    loop {
        let round = limit.map_or(ROUND, |l| ROUND.min(l - total.embedded));
        let report = backfill.department(department, round).await?;
        total.missing += report.missing;
        total.embedded += report.embedded;
        total.skipped += report.skipped;
        total.service_down = report.service_down;
        total.misconfigured = report.misconfigured;
        total.complete = report.complete;
        let reached = limit.is_some_and(|l| total.embedded >= l);
        if report.complete || report.service_down || report.misconfigured || reached {
            return Ok(total);
        }
    }
}
