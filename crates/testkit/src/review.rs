use kernel::facts::FactId;
use kernel::memory::TagId;
use kernel::owner::Owner;
use kernel::review::FilingTarget;
use kernel::review::Mark;
use kernel::review::ReviewKey;
use kernel::review::ReviewMark;
use kernel::review::ReviewStore;
use kernel::topics::Slug;
use uuid::Uuid;
use wire::session_id::SessionId;

use crate::topics::at;

pub async fn review_marks_keep_each_owners_decisions(
    store: &dyn ReviewStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let owner = crate::owner()?;
    let conflict = ReviewKey::Conflict {
        fact: FactId::from_uuid(Uuid::parse_str("00000000-0000-4000-8000-0000000000c1")?),
        version: at(3).timestamp_millis(),
    };
    let filing = ReviewKey::Filing {
        session: "claude-code:00000000-0000-4000-8000-0000000000c2".parse::<SessionId>()?,
        into: FilingTarget::New(Slug::fold("Home lab")?),
    };
    let tag = ReviewKey::Tag(TagId::from_uuid(Uuid::parse_str(
        "00000000-0000-4000-8000-0000000000c3",
    )?));
    assert_eq!(store.marks(&owner).await?, Vec::new());
    for (key, mark) in [
        (conflict.clone(), Mark::Decided { at: at(4) }),
        (filing.clone(), Mark::Snoozed { until: at(5) }),
        (tag.clone(), Mark::Snoozed { until: at(6) }),
        (tag.clone(), Mark::Decided { at: at(7) }),
    ] {
        store.mark(&owner, &ReviewMark { key, mark }).await?;
    }
    let mut kept = store.marks(&owner).await?;
    kept.sort_by(|left, right| left.key.cmp(&right.key));
    assert_eq!(
        kept,
        [
            ReviewMark {
                key: conflict,
                mark: Mark::Decided { at: at(4) },
            },
            ReviewMark {
                key: filing,
                mark: Mark::Snoozed { until: at(5) },
            },
            ReviewMark {
                key: tag,
                mark: Mark::Decided { at: at(7) },
            },
        ],
        "a later mark on the same item replaces the earlier one"
    );
    let other = Owner::new("someone-else".to_owned())?;
    assert_eq!(
        store.marks(&other).await?,
        Vec::new(),
        "another person's review starts empty"
    );
    Ok(())
}
