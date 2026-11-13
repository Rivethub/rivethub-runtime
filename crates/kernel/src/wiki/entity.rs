use crate::topics::Topic;
use crate::topics::TopicKind;
use crate::topics::TopicPageError;
use crate::topics::TopicStore;
use crate::topics::TopicTree;

const PATH_SEPARATOR: &str = " › ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    types: Vec<Topic>,
}

impl Catalog {
    pub async fn load(topics: &dyn TopicStore) -> Result<Self, TopicPageError> {
        let mut types = Vec::new();
        for listing in topics.list().await? {
            if listing.topic.kind == TopicKind::Type {
                types.push(listing.topic);
            }
        }
        Ok(Self { types })
    }

    #[must_use]
    pub fn type_paths(&self) -> Vec<String> {
        let tree = TopicTree::of(&self.types);
        let mut paths: Vec<String> = self
            .types
            .iter()
            .map(|kind| {
                let mut names: Vec<&str> = tree
                    .ancestors(kind.id)
                    .filter_map(|above| self.types.iter().find(|known| known.id == above))
                    .map(|above| above.name.as_str())
                    .collect();
                names.reverse();
                names.push(kind.name.as_str());
                names.join(PATH_SEPARATOR)
            })
            .collect();
        paths.sort();
        paths
    }
}
