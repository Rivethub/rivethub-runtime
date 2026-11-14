use std::collections::BTreeMap;

use super::FakeMemoryStore;

impl FakeMemoryStore {
    pub async fn script_recall_owner(&self, session: String, owner: String) {
        self.owners.lock().await.insert(session, owner);
    }

    pub(super) async fn owners_for(&self, session_keys: Vec<String>) -> BTreeMap<String, String> {
        let owners = self.owners.lock().await;
        session_keys
            .into_iter()
            .filter_map(|key| owners.get(&key).map(|owner| (key, owner.clone())))
            .collect()
    }
}

pub(super) fn wiki_topic(page: &kernel::wiki::WikiPageResponse) -> kernel::wiki::WikiTopic {
    kernel::wiki::WikiTopic {
        slug: page.slug.clone(),
        title: page.title.clone(),
        tags: page.tags.clone(),
        entities: page.entities.clone(),
        updated_at: page.updated_at,
        excerpt: page.current_state.chars().take(240).collect(),
        topic_id: page.topic_id.clone(),
    }
}
