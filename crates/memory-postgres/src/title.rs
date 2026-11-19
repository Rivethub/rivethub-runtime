use kernel::filing::Filable;
use kernel::memory::summary_opening::OpeningTitle;
use kernel::memory::{ConversationHead, names_session, needs_title, summary_title};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::summary_store::HeadRow;
use crate::{Error, PostgresMemory};

const OPENINGS: &str = "SELECT c.id, c.session_key, c.agent, c.channel, c.title,
                               c.title_from_capture, o.content, o.title_armed,
                               EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key) AS trashed
                          FROM rh_conversations c
                          JOIN rh_summary_openings o ON o.conversation_id = c.id
                         WHERE $1::uuid IS NULL OR c.id = $1 ORDER BY c.id";

const SET_SQL: &str = "UPDATE rh_conversations SET title = $1
                       WHERE id = $2 AND title IS NOT DISTINCT FROM $3 AND NOT title_from_capture";

pub type Untitled = OpeningTitle;

#[derive(sqlx::FromRow)]
struct OpeningRow {
    #[sqlx(flatten)]
    head: HeadRow,
    content: String,
    title_armed: bool,
    title_from_capture: bool,
}

impl From<OpeningRow> for OpeningTitle {
    fn from(row: OpeningRow) -> Self {
        Self {
            head: row.head.into(),
            content: row.content,
            armed: row.title_armed,
            supplied: row.title_from_capture,
        }
    }
}

impl PostgresMemory {
    pub async fn backfill_titles(&self, dry_run: bool) -> Result<u64, Error> {
        let mut tx = self.pool.begin().await.map_err(failure)?;
        let rows = sqlx::query_as::<_, OpeningRow>(OPENINGS)
            .bind(Option::<Uuid>::None)
            .fetch_all(&mut *tx)
            .await
            .map_err(failure)?;
        let mut titled = 0;
        for opening in rows.into_iter().map(OpeningTitle::from) {
            if opening.head.trashed
                || opening.supplied
                || opening.head.title.as_deref().is_some_and(names_session)
            {
                continue;
            }
            if let Some(title) = summary_title(&opening.content)
                && (dry_run || set_title(&mut tx, &opening.head, &title).await?)
            {
                titled += 1;
            }
        }
        tx.commit().await.map_err(failure)?;
        Ok(titled)
    }

    pub async fn untitled(&self) -> Result<Vec<Untitled>, Error> {
        let rows = sqlx::query_as::<_, OpeningRow>(OPENINGS)
            .bind(Option::<Uuid>::None)
            .fetch_all(&self.pool)
            .await
            .map_err(failure)?;
        Ok(rows
            .into_iter()
            .map(OpeningTitle::from)
            .filter(OpeningTitle::wants_model)
            .collect())
    }

    pub async fn apply_opening_title(
        &self,
        opening: &OpeningTitle,
        title: Option<&str>,
    ) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await.map_err(failure)?;
        sqlx::query("SELECT id FROM rh_conversations WHERE id = $1 FOR UPDATE")
            .bind(opening.head.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(failure)?;
        let current = load(&mut tx, opening.head.id).await?;
        if current.as_ref() != Some(opening) {
            return Ok(false);
        }
        if opening.armed {
            sqlx::query(
                "UPDATE rh_summary_openings SET title_armed = false WHERE conversation_id = $1",
            )
            .bind(opening.head.id)
            .execute(&mut *tx)
            .await
            .map_err(failure)?;
        }
        let changed = if let Some(title) = title.filter(|_| opening.wants_model()) {
            set_title(&mut tx, &opening.head, title).await?
        } else {
            false
        };
        tx.commit().await.map_err(failure)?;
        Ok(changed)
    }
}

pub(crate) async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Option<OpeningTitle>, Error> {
    sqlx::query_as::<_, OpeningRow>(OPENINGS)
        .bind(id)
        .fetch_optional(conn)
        .await
        .map(|row| row.map(OpeningTitle::from))
        .map_err(failure)
}

pub(crate) async fn head(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(ConversationHead, bool), Error> {
    #[derive(sqlx::FromRow)]
    struct Current {
        #[sqlx(flatten)]
        head: HeadRow,
        title_from_capture: bool,
    }
    let row = sqlx::query_as::<_, Current>(
        "SELECT id, session_key, agent, channel, title, title_from_capture,
                EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key) AS trashed
           FROM rh_conversations c WHERE id = $1",
    )
    .bind(id)
    .fetch_one(conn)
    .await
    .map_err(failure)?;
    Ok((row.head.into(), row.title_from_capture))
}

pub(crate) async fn arm_historical(conn: &mut PgConnection, id: Uuid) -> Result<(), Error> {
    let present = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM rh_conversations WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failure)?;
    if present.is_none() {
        return Ok(());
    }
    let (head, supplied) = head(conn, id).await?;
    if Filable::of(&head) == Filable::Never {
        return Ok(());
    }
    let title = !supplied && needs_title(head.title.as_deref());
    sqlx::query(
        "UPDATE rh_summary_openings SET historical_pending = false, title_armed = $2,
             filing_armed = NOT EXISTS (SELECT 1 FROM rh_conversation_placements WHERE conversation_id = $1)
                AND NOT EXISTS (SELECT 1 FROM rh_session_placements WHERE session_id = $3 OR session_id = $4)
         WHERE conversation_id = $1 AND historical_pending",
    )
    .bind(id)
    .bind(title)
    .bind(&head.session_key)
    .bind(kernel::filing::session_key(Some(&head.channel), &head.session_key))
    .execute(conn)
    .await
    .map_err(failure)?;
    Ok(())
}

pub(crate) async fn seed(
    conn: &mut PgConnection,
    head: &ConversationHead,
    old: Option<&OpeningTitle>,
    content: &str,
) -> Result<(), Error> {
    let replace = match old {
        Some(old) => old.is_seed(),
        None => !head.title.as_deref().is_some_and(names_session),
    };
    if !head.trashed
        && replace
        && let Some(title) = summary_title(content)
    {
        set_title(conn, head, &title).await?;
    }
    Ok(())
}

pub(crate) async fn name_conversation(
    conn: &mut PgConnection,
    id: Uuid,
    title: &str,
) -> Result<bool, Error> {
    let row = sqlx::query_as::<_, HeadRow>(
        "SELECT id, session_key, agent, channel, title FROM rh_conversations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failure)?;
    let Some(head) = row.map(ConversationHead::from) else {
        return Ok(false);
    };
    if !needs_title(head.title.as_deref()) {
        return Ok(false);
    }
    set_title(conn, &head, title).await
}

async fn set_title(
    conn: &mut PgConnection,
    head: &ConversationHead,
    title: &str,
) -> Result<bool, Error> {
    let row = sqlx::query(SET_SQL)
        .bind(title)
        .bind(head.id)
        .bind(&head.title)
        .execute(conn)
        .await
        .map_err(failure)?;
    Ok(row.rows_affected() == 1)
}

fn failure(source: sqlx::Error) -> Error {
    Error::operation("conversation opening title", source)
}
