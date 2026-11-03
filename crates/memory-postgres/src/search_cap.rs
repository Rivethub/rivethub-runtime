use chrono::Duration;
use chrono::Utc;
use serde::Deserialize;
use sqlx::QueryBuilder;
use sqlx::types::Json;
use uuid::Uuid;

use crate::error::Error;
use crate::search_sql::ArmContext;
use crate::search_sql::push_fts_where;

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

pub(crate) const fn newest_column(is_message: bool) -> &'static str {
    if is_message {
        "m.created_at"
    } else {
        "s.latest_at"
    }
}

const fn source(is_message: bool) -> (&'static str, &'static str) {
    if is_message {
        ("m", "rh_messages m")
    } else {
        ("s", "rh_summaries s")
    }
}

impl crate::PostgresMemory {
    pub(crate) async fn capped_candidates(
        &self,
        context: &ArmContext<'_>,
        is_message: bool,
        long: bool,
    ) -> Result<Option<Vec<Uuid>>, Error> {
        let (alias, from) = source(is_message);
        let mut explain = QueryBuilder::new(format!("EXPLAIN (FORMAT JSON) SELECT 1 FROM {from}"));
        push_fts_where(&mut explain, context, is_message, false);
        let plan: Json<Vec<Explained>> =
            explain
                .build_query_scalar()
                .fetch_one(&self.pool)
                .await
                .map_err(|source| Error::operation("search estimate", source))?;
        let estimate = plan.0.first().map_or(0.0, |explained| explained.plan.rows);
        if estimate < PROBE_FROM {
            return Ok(None);
        }
        if estimate < f64::from(u32::try_from(RANK_CAP)?)
            && !self.matches_beyond_cap(context, is_message, long).await?
        {
            return Ok(None);
        }
        let newest = newest_column(is_message);
        let now = Utc::now();
        let mut candidates: Vec<Uuid> = Vec::with_capacity(RANK_CAP);
        let mut upper = None;
        let bounds = SLICE_DAYS
            .iter()
            .map(|days| Some(now - Duration::days(*days)))
            .chain(std::iter::once(None));
        for lower in bounds {
            let mut slice = QueryBuilder::new(format!("SELECT {alias}.id FROM {from}"));
            push_fts_where(&mut slice, context, is_message, long);
            if let Some(upper) = upper {
                slice.push(format!(" AND {newest} < ")).push_bind(upper);
            }
            if let Some(lower) = lower {
                slice.push(format!(" AND {newest} >= ")).push_bind(lower);
            }
            slice.push(format!(
                " ORDER BY {newest} DESC LIMIT {}",
                RANK_CAP - candidates.len()
            ));
            let found: Vec<Uuid> = slice
                .build_query_scalar()
                .fetch_all(&self.pool)
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
        &self,
        context: &ArmContext<'_>,
        is_message: bool,
        long: bool,
    ) -> Result<bool, Error> {
        let (_, from) = source(is_message);
        let mut probe = QueryBuilder::new(format!("SELECT count(*) FROM (SELECT 1 FROM {from}"));
        push_fts_where(&mut probe, context, is_message, long);
        probe.push(format!(" LIMIT {}) probe", RANK_CAP + 1));
        let matched: i64 = probe
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("search estimate", source))?;
        Ok(usize::try_from(matched)? > RANK_CAP)
    }
}
