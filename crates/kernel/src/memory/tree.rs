use std::collections::HashMap;
use std::collections::HashSet;

use chrono::DateTime;
use chrono::Utc;

use super::MemoryMessageTally;
use super::MemorySummaryNode;
use super::SummaryDag;
use super::SummaryKind;
use super::VectorState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeWindow {
    pub offset: usize,
    pub limit: usize,
}

impl TreeWindow {
    pub const ALL: Self = Self {
        offset: 0,
        limit: usize::MAX,
    };
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SummaryCounts {
    pub leaves: u64,
    pub branches: u64,
    pub roots: u64,
    pub embedded: u64,
    pub top: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryTree {
    pub session_key: Option<String>,
    pub summaries: Vec<MemorySummaryNode>,
    pub total: u64,
    pub counts: SummaryCounts,
    pub messages: MemoryMessageTally,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryShape {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: SummaryKind,
    pub embedded: bool,
    pub earliest_at: Option<DateTime<Utc>>,
    pub latest_at: Option<DateTime<Utc>>,
}

impl From<&MemorySummaryNode> for SummaryShape {
    fn from(node: &MemorySummaryNode) -> Self {
        Self {
            id: node.id.clone(),
            parent_id: node.parent_id.clone(),
            kind: node.kind,
            embedded: node.vector == VectorState::Embedded,
            earliest_at: node.earliest_at,
            latest_at: node.latest_at,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeCut {
    pub ids: Vec<String>,
    pub children: HashMap<String, u64>,
    pub total: u64,
    pub counts: SummaryCounts,
}

#[must_use]
pub fn cut(shapes: &[SummaryShape], parent: Option<&str>, window: TreeWindow) -> Option<TreeCut> {
    let known: HashSet<&str> = shapes.iter().map(|shape| shape.id.as_str()).collect();
    if parent.is_some_and(|parent| !known.contains(parent)) {
        return None;
    }
    let mut children: HashMap<String, u64> =
        shapes.iter().map(|shape| (shape.id.clone(), 0)).collect();
    let mut counts = SummaryCounts::default();
    let mut siblings: Vec<&SummaryShape> = Vec::new();
    for shape in shapes {
        match shape.kind {
            SummaryKind::Leaf => counts.leaves += 1,
            SummaryKind::Branch => counts.branches += 1,
            SummaryKind::Root => counts.roots += 1,
        }
        if shape.embedded {
            counts.embedded += 1;
        }
        let above = shape
            .parent_id
            .as_deref()
            .filter(|above| known.contains(above));
        match above {
            Some(above) => {
                if let Some(count) = children.get_mut(above) {
                    *count += 1;
                }
            }
            None => counts.top += 1,
        }
        if above == parent {
            siblings.push(shape);
        }
    }
    siblings.sort_by(|left, right| {
        (left.earliest_at.or(left.latest_at), &left.id)
            .cmp(&(right.earliest_at.or(right.latest_at), &right.id))
    });
    let total = siblings.iter().fold(0, |total, _| total + 1);
    let ids = siblings
        .into_iter()
        .skip(window.offset)
        .take(window.limit)
        .map(|shape| shape.id.clone())
        .collect();
    Some(TreeCut {
        ids,
        children,
        total,
        counts,
    })
}

impl TreeCut {
    #[must_use]
    pub fn pick(&self, nodes: Vec<MemorySummaryNode>) -> Vec<MemorySummaryNode> {
        let mut by_id: HashMap<String, MemorySummaryNode> = nodes
            .into_iter()
            .map(|node| (node.id.clone(), node))
            .collect();
        self.ids
            .iter()
            .filter_map(|id| {
                let mut node = by_id.remove(id)?;
                node.children = self.children.get(id).copied();
                Some(node)
            })
            .collect()
    }
}

impl SummaryDag {
    #[must_use]
    pub fn tree(self, parent: Option<&str>, window: TreeWindow) -> Option<SummaryTree> {
        let shapes: Vec<SummaryShape> = self.summaries.iter().map(SummaryShape::from).collect();
        let cut = cut(&shapes, parent, window)?;
        Some(SummaryTree {
            session_key: self.session_key,
            summaries: cut.pick(self.summaries),
            total: cut.total,
            counts: if parent.is_none() {
                cut.counts
            } else {
                SummaryCounts::default()
            },
            messages: if parent.is_none() {
                self.messages
            } else {
                MemoryMessageTally::default()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, kind: SummaryKind, minute: u32) -> MemorySummaryNode {
        let at = DateTime::from_timestamp(i64::from(minute) * 60, 0);
        MemorySummaryNode {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            level: 0,
            kind,
            content: id.to_owned(),
            message_count: 1,
            earliest_at: at,
            latest_at: at,
            created_at: DateTime::UNIX_EPOCH,
            source_message_ids: Vec::new(),
            vector: if kind == SummaryKind::Leaf {
                VectorState::Pending
            } else {
                VectorState::Embedded
            },
            truncated: false,
            children: None,
        }
    }

    fn dag() -> SummaryDag {
        SummaryDag {
            session_key: Some("cli:tree".to_owned()),
            summaries: vec![
                node("late-leaf", None, SummaryKind::Leaf, 50),
                node("root", None, SummaryKind::Root, 10),
                node("branch", Some("root"), SummaryKind::Branch, 10),
                node("leaf-2", Some("branch"), SummaryKind::Leaf, 20),
                node("leaf-1", Some("branch"), SummaryKind::Leaf, 10),
                node("orphan", Some("gone"), SummaryKind::Leaf, 30),
            ],
            messages: MemoryMessageTally::default(),
        }
    }

    fn named(tree: &SummaryTree) -> Vec<(&str, Option<u64>)> {
        tree.summaries
            .iter()
            .map(|node| (node.id.as_str(), node.children))
            .collect()
    }

    #[test]
    fn the_top_comes_in_time_order_windowed_with_each_node_counting_its_children() {
        let window = TreeWindow {
            offset: 1,
            limit: 2,
        };
        let tree = dag().tree(None, window).expect("the top");
        assert_eq!(named(&tree), [("orphan", Some(0)), ("late-leaf", Some(0))]);
        assert_eq!(tree.total, 3);
        assert_eq!(
            tree.counts,
            SummaryCounts {
                leaves: 4,
                branches: 1,
                roots: 1,
                embedded: 2,
                top: 3,
            }
        );
        let whole = dag().tree(None, TreeWindow::ALL).expect("the top");
        assert_eq!(
            named(&whole),
            [
                ("root", Some(1)),
                ("orphan", Some(0)),
                ("late-leaf", Some(0))
            ]
        );
    }

    #[test]
    fn a_node_answers_its_own_children_and_an_unknown_one_answers_none() {
        let tree = dag()
            .tree(Some("branch"), TreeWindow::ALL)
            .expect("the branch");
        assert_eq!(named(&tree), [("leaf-1", Some(0)), ("leaf-2", Some(0))]);
        assert_eq!(tree.total, 2);
        assert_eq!(dag().tree(Some("gone"), TreeWindow::ALL), None);
    }
}
