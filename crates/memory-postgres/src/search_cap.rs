use chrono::{Duration, Utc};
use serde::Deserialize;
use sqlx::{PgConnection, Postgres, QueryBuilder, types::Json};
use uuid::Uuid;

use crate::error::Error;
use crate::search_fts::{push_fts_match, push_fts_where};
use crate::search_sql::{ArmContext, SearchTable};

const RANK_CAP: usize = 5_000;
const PROBE_FROM: f64 = 100.0;
const SLICE_DAYS: [i64; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];

#[derive(Deserialize)]
struct Explained {
    #[serde(rename = "Plan")]
    plan: PlannedRows,
}

#[derive(Deserialize)]
struct PlannedRows {
    #[serde(rename = "Plan Rows")]
    rows: f64,
}

pub(crate) async fn capped_candidates(
    tx: &mut PgConnection,
    context: &ArmContext<'_>,
    tables: &[SearchTable],
    long: bool,
) -> Result<Option<Vec<Uuid>>, Error> {
    let mut explain = QueryBuilder::new("EXPLAIN (FORMAT JSON) ");
    for (index, &table) in tables.iter().enumerate() {
        if index > 0 {
            explain.push(" UNION ALL ");
        }
        explain.push(format!("SELECT 1 FROM {}", table.from()));
        push_fts_match(&mut explain, context, table, false);
    }
    let plan: Json<Vec<Explained>> = explain
        .build_query_scalar()
        .fetch_one(&mut *tx)
        .await
        .map_err(|source| Error::operation("search estimate", source))?;
    let estimate = plan.0.first().map_or(0.0, |explained| explained.plan.rows);
    if estimate < PROBE_FROM {
        return Ok(None);
    }
    if estimate < f64::from(u32::try_from(RANK_CAP)?)
        && !matches_beyond_cap(tx, context, tables, long).await?
    {
        return Ok(None);
    }

    let now = Utc::now();
    let mut candidates: Vec<Uuid> = Vec::with_capacity(RANK_CAP);
    let mut upper = None;
    let bounds = SLICE_DAYS
        .iter()
        .map(|days| Some(now - Duration::days(*days)))
        .chain(std::iter::once(None));
    for lower in bounds {
        let remaining = RANK_CAP - candidates.len();
        let mut slice = QueryBuilder::new("SELECT id FROM (");
        for (index, &table) in tables.iter().enumerate() {
            if index > 0 {
                slice.push(" UNION ALL ");
            }
            let newest = table.newest();
            slice.push(format!(
                "(SELECT {}.id, {newest} AS newest FROM {}",
                table.alias(),
                table.from()
            ));
            push_fts_where(&mut slice, context, table, long);
            if let Some(upper) = upper {
                slice
                    .push(format!(" AND {newest} < "))
                    .push_bind(crate::time::BindTime::bind_time(upper));
            }
            if let Some(lower) = lower {
                slice
                    .push(format!(" AND {newest} >= "))
                    .push_bind(crate::time::BindTime::bind_time(lower));
            }
            slice.push(format!(" ORDER BY {newest} DESC LIMIT {remaining})"));
        }
        slice.push(format!(
            ") candidates ORDER BY newest DESC LIMIT {remaining}"
        ));
        let found: Vec<Uuid> = slice
            .build_query_scalar()
            .fetch_all(&mut *tx)
            .await
            .map_err(|source| Error::operation("search candidates", source))?;
        candidates.extend(found);
        if candidates.len() >= RANK_CAP || lower.is_none() {
            break;
        }
        upper = lower;
    }
    Ok(Some(candidates))
}

async fn matches_beyond_cap(
    tx: &mut PgConnection,
    context: &ArmContext<'_>,
    tables: &[SearchTable],
    long: bool,
) -> Result<bool, Error> {
    let mut probe: QueryBuilder<Postgres> = QueryBuilder::new("SELECT count(*) FROM (");
    for (index, &table) in tables.iter().enumerate() {
        if index > 0 {
            probe.push(" UNION ALL ");
        }
        probe.push(format!("SELECT 1 FROM {}", table.from()));
        push_fts_where(&mut probe, context, table, long);
    }
    probe.push(format!(" LIMIT {}) probe", RANK_CAP + 1));
    let matched: i64 = probe
        .build_query_scalar()
        .fetch_one(tx)
        .await
        .map_err(|source| Error::operation("search estimate", source))?;
    Ok(usize::try_from(matched)? > RANK_CAP)
}
