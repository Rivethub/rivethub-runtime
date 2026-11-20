use super::{
    ENTITY_LOCK_CLASS, EntityLeaf, EntityStoreError, EntityView, Postgres, PreparedEntity,
    SummaryConsumer, SummaryReferenceError, Transaction, compact_key, identity_keys, query_error,
    summary_replace,
};

pub(super) async fn lock_names(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedEntity,
) -> Result<(), EntityStoreError> {
    let mut keys: Vec<String> = name_keys(prepared)
        .into_iter()
        .map(|key| format!("name:{key}"))
        .collect();
    if let Some(id) = prepared.entity_ref {
        keys.push(format!("id:{id}"));
    }
    if keys.is_empty() {
        keys.push(format!("name:{}", compact_key(&prepared.name)));
    }
    lock_all(tx, &keys).await
}

fn name_keys(prepared: &PreparedEntity) -> Vec<String> {
    identity_keys(&prepared.name)
}

pub(super) async fn lock_view(
    tx: &mut Transaction<'_, Postgres>,
    view: &EntityView,
) -> Result<(), EntityStoreError> {
    lock_all(
        tx,
        &[
            format!("name:{}", compact_key(&view.name)),
            format!("id:{}", view.id),
        ],
    )
    .await
}

async fn lock_all(
    tx: &mut Transaction<'_, Postgres>,
    keys: &[String],
) -> Result<(), EntityStoreError> {
    let mut ordered = keys.to_vec();
    ordered.sort();
    ordered.dedup();
    for key in ordered {
        sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
            .bind(ENTITY_LOCK_CLASS)
            .bind(key)
            .execute(&mut **tx)
            .await
            .map_err(query_error)?;
    }
    Ok(())
}

pub(super) async fn validate_leaf(
    tx: &mut Transaction<'_, Postgres>,
    input: &summary_replace::SummaryInput,
    leaf: &EntityLeaf,
) -> Result<(), EntityStoreError> {
    if input.conversation_id != leaf.conversation_id
        || input.content != leaf.content
        || Some(input.owner_user_id.as_str()) != leaf.owner.as_deref()
        || input.input_key.as_ref() != leaf.delivery.as_ref().map(|delivery| &delivery.input_key)
    {
        return Err(SummaryReferenceError::Stale(input.id).into());
    }
    if let Some(delivery) = &leaf.delivery
        && summary_replace::pending(tx, input.id, SummaryConsumer::Entity)
            .await
            .map_err(query_error)?
            != delivery.pending
    {
        return Err(SummaryReferenceError::Stale(input.id).into());
    }
    Ok(())
}
