use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::entity_pass::EntityStoreError;

pub(super) fn query_error(error: sqlx::Error) -> EntityStoreError {
    EntityStoreError::Query(Box::new(error))
}

pub(super) fn store_error(error: WikiStoreError) -> EntityStoreError {
    match error {
        WikiStoreError::DirectoryMissing => EntityStoreError::DirectoryMissing,
        WikiStoreError::Page(error) => EntityStoreError::Page(error),
        WikiStoreError::Reference(error) => EntityStoreError::Reference(error),
        WikiStoreError::Missing(id) => EntityStoreError::Missing(id),
        WikiStoreError::Query(error) => EntityStoreError::Query(error),
        WikiStoreError::ConversationMismatch(id) => {
            EntityStoreError::Query(Box::new(std::io::Error::other(id.to_string())))
        }
        error @ WikiStoreError::RedirectsHeld { .. } => EntityStoreError::Query(Box::new(error)),
        WikiStoreError::SlugConflict(slug)
        | WikiStoreError::TopicMissing(slug)
        | WikiStoreError::InvalidPatch(slug) => {
            EntityStoreError::Query(Box::new(std::io::Error::other(slug)))
        }
    }
}

pub(super) fn fence_error(error: crate::summary_replace::FenceError) -> EntityStoreError {
    match error {
        crate::summary_replace::FenceError::Reference(error) => error.into(),
        crate::summary_replace::FenceError::Query(error) => query_error(error),
    }
}
