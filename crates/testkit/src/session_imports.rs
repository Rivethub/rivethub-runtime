use kernel::memory::MemoryStoreError;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionRecord;
use wire::session_id::SessionId;
use wire::session_id::SessionKey;

pub(crate) async fn record_of(
    catalog: &dyn SessionCatalog,
    session: &SessionId,
) -> Result<Option<SessionRecord>, MemoryStoreError> {
    Ok(catalog
        .get(&SessionKey::from(session))
        .await?
        .and_then(SessionRecord::narrow))
}
