use std::time::Duration;

use kernel::deadline::Deadline;
use kernel::tenancy::Scope;
use sqlx::PgConnection;
use sqlx::postgres::PgPoolOptions;

const MAINTENANCE_ON: &str = "on";
const MAINTENANCE_OFF: &str = "off";

struct Context {
    principal: String,
    spaces: String,
    maintenance: &'static str,
}

fn context(scope: Option<Scope>) -> Context {
    match scope {
        Some(Scope::Viewer(viewer)) => Context {
            principal: viewer.principal().as_str().to_owned(),
            spaces: viewer
                .spaces()
                .iter()
                .map(kernel::tenancy::SpaceId::as_str)
                .collect::<Vec<_>>()
                .join(","),
            maintenance: MAINTENANCE_OFF,
        },
        Some(Scope::Maintenance) => Context {
            principal: String::new(),
            spaces: String::new(),
            maintenance: MAINTENANCE_ON,
        },
        Some(Scope::Closed) => Context {
            principal: String::new(),
            spaces: String::new(),
            maintenance: MAINTENANCE_OFF,
        },
        None => {
            tracing::debug!("database connection taken outside a tenancy scope; it sees no rows");
            Context {
                principal: String::new(),
                spaces: String::new(),
                maintenance: MAINTENANCE_OFF,
            }
        }
    }
}

fn statement_timeout(left: Duration) -> String {
    let millis = left.as_millis().max(1);
    format!("{millis}ms")
}

pub(crate) async fn apply(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    let context = context(Scope::current());
    sqlx::query(
        "SELECT set_config('rh.principal', $1, false), set_config('rh.spaces', $2, false), \
                set_config('rh.maintenance', $3, false), set_config('jit', 'off', false), \
                set_config('statement_timeout', coalesce($4, \
                    (SELECT s.reset_val FROM pg_settings s WHERE s.name = 'statement_timeout')), false)",
    )
    .bind(context.principal)
    .bind(context.spaces)
    .bind(context.maintenance)
    .bind(Deadline::remaining().map(statement_timeout))
    .execute(conn)
    .await
    .map(drop)
}

const NOT_VERIFIED: &str = "the memory database has not passed its schema and login checks yet";

#[derive(Debug, Default)]
pub(crate) struct Gate(std::sync::atomic::AtomicBool);

impl Gate {
    pub(crate) fn open(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    fn is_open(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[must_use]
pub(crate) fn gated(options: PgPoolOptions, gate: std::sync::Arc<Gate>) -> PgPoolOptions {
    options
        .after_connect(move |conn, _| {
            let open = gate.is_open();
            Box::pin(async move {
                if !open {
                    return Err(sqlx::Error::Protocol(NOT_VERIFIED.to_owned()));
                }
                apply(conn).await
            })
        })
        .before_acquire(|conn, _| Box::pin(async move { apply(conn).await.map(|()| true) }))
}

#[must_use]
pub fn scoped(options: PgPoolOptions) -> PgPoolOptions {
    options
        .after_connect(|conn, _| Box::pin(apply(conn)))
        .before_acquire(|conn, _| Box::pin(async move { apply(conn).await.map(|()| true) }))
}
