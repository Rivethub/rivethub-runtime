use kernel::filing::Correction;
use kernel::filing::FilingStore;
use kernel::filing::Target;
use kernel::memory::MemoryStore;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::WorkerRole;
use kernel::tidy::Batch;
use kernel::tidy::Decision;
use kernel::tidy::Evidence;
use kernel::tidy::ProposalState;
use kernel::tidy::Proposer;
use kernel::tidy::Reason;
use kernel::tidy::RoleActivity;
use kernel::tidy::Sample;
use kernel::tidy::TidyPorts;
use kernel::tidy::TidyStore;
use kernel::tidy::TreeEdit;
use kernel::tidy::UpkeepStore;
use kernel::topics::Alias;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use wire::JsonObject;

use crate::filing::capture;
use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::new_topic;
use crate::topics::same;
use crate::topics::session;

fn evidence(sessions: u64) -> Evidence {
    Evidence {
        sessions,
        samples: Vec::new(),
    }
}

pub async fn proposals_are_decided_and_rejections_remembered<S>(store: &S) -> Outcome
where
    S: TidyStore + TopicStore + FilingStore + TopicPageStore,
{
    let app = store.create(new_topic("Desktop App", &[], None)?).await?;
    let client = store
        .create(new_topic("Desktop Client", &[], None)?)
        .await?;
    let spike = store.create(new_topic("Old Spike", &[], None)?).await?;
    let members = (1..=3)
        .map(|n| session(&format!("00000000-0000-4000-8000-00000000006{n}")))
        .collect::<Result<Vec<_>, _>>()?;
    let created = TopicId::random();
    let close = TreeEdit::Close { topic: spike.id };
    let mut batch = Batch::new(Proposer::Backfill, at(1));
    batch.add(
        TreeEdit::Merge {
            source: client.id,
            into: app.id,
        },
        Reason::new("one app under two names")?,
        evidence(2),
    );
    batch.add(close.clone(), Reason::new("idle for months")?, evidence(0));
    batch.add(
        TreeEdit::CreateTopic {
            topic: created,
            name: TopicName::new("Release Tooling")?,
            parent: None,
            aliases: Vec::new(),
            members: members.iter().cloned().map(Target::Session).collect(),
        },
        Reason::new("three sessions suggested it")?,
        evidence(3),
    );
    let stored = store.propose(batch).await?;
    decided_and_remembered(store, &stored, (&client, app.id), close, &members, created).await
}

async fn decided_and_remembered<S>(
    store: &S,
    stored: &Batch,
    (client, app): (&Topic, TopicId),
    close: TreeEdit,
    members: &[wire::session_id::SessionId],
    created: TopicId,
) -> Outcome
where
    S: TidyStore + TopicStore + FilingStore + TopicPageStore,
{
    let ids: Vec<_> = stored
        .proposals
        .iter()
        .map(|proposal| proposal.id)
        .collect();
    same(
        "the batch is listed pending in order",
        store
            .pending()
            .await?
            .iter()
            .map(|pending| {
                pending
                    .proposals
                    .iter()
                    .map(|proposal| proposal.id)
                    .collect()
            })
            .collect::<Vec<Vec<_>>>(),
        vec![ids.clone()],
    )?;
    let owner = crate::owner()?;
    let ports = TidyPorts {
        tidy: store,
        topics: store,
        filing: store,
        pages: Some(store),
        owner: &owner,
    };
    let [merge, rejected, create] = ids.as_slice() else {
        return Err("three proposals were kept".into());
    };
    let approved = kernel::tidy::approve(&ports, *merge, at(2)).await?;
    same(
        "the merge is approved",
        approved.state,
        ProposalState::Approved,
    )?;
    same(
        "the merged name redirects",
        store
            .find(&TopicKey::Slug(TopicKind::Work, client.slug.clone()))
            .await?
            .map(|topic| topic.id),
        Some(app),
    )?;
    kernel::tidy::reject(store, *rejected, at(3)).await?;
    let mut again = Batch::new(Proposer::Categorizer, at(4));
    again.add(close.clone(), Reason::new("still idle")?, evidence(0));
    same(
        "a rejected change is never proposed again",
        store.propose(again).await?.proposals.len(),
        0,
    )?;
    same(
        "its fingerprint stays known",
        store.known().await?.contains(&close.fingerprint()?),
        true,
    )?;
    same(
        "an empty weekly batch still counts as a run",
        store.last_batch(Proposer::Categorizer).await?,
        Some(at(4)),
    )?;
    approves_the_rest(&ports, stored, *create, members, created).await
}

async fn approves_the_rest(
    ports: &TidyPorts<'_>,
    stored: &Batch,
    create: kernel::tidy::ProposalId,
    members: &[wire::session_id::SessionId],
    created: TopicId,
) -> Outcome {
    let outcome = kernel::tidy::decide_batch(ports, stored.id, Decision::Approve, at(5)).await?;
    same(
        "the batch approves what is left",
        outcome
            .decided
            .iter()
            .map(|proposal| proposal.id)
            .collect::<Vec<_>>(),
        vec![create],
    )?;
    same(
        "the members are filed by the categorizer",
        ports
            .topics
            .placement(members.first().ok_or("a member")?)
            .await?
            .map(|placement| (placement.topic, placement.by)),
        Some((created, PlacementSource::Categorizer)),
    )?;
    same("nothing is pending", ports.tidy.pending().await?.len(), 0)?;
    Ok(())
}

pub async fn an_approval_that_stopped_part_way_finishes_on_retry<S>(store: &S) -> Outcome
where
    S: TidyStore + TopicStore + FilingStore + TopicPageStore,
{
    let app = store.create(new_topic("Desktop App", &[], None)?).await?;
    let client = store
        .create(new_topic("Desktop Client", &[], None)?)
        .await?;
    let spike = store.create(new_topic("Old Spike", &[], None)?).await?;
    let members = (1..=3)
        .map(|n| session(&format!("00000000-0000-4000-8000-00000000008{n}")))
        .collect::<Result<Vec<_>, _>>()?;
    let created = TopicId::random();
    let name = TopicName::new("Release Tooling")?;
    let stored = store
        .propose(stopped_batch(
            TreeEdit::CreateTopic {
                topic: created,
                name: name.clone(),
                parent: None,
                aliases: Vec::new(),
                members: members.iter().cloned().map(Target::Session).collect(),
            },
            TreeEdit::Merge {
                source: client.id,
                into: app.id,
            },
            TreeEdit::Delete { topic: spike.id },
        )?)
        .await?;
    let new = NewTopic {
        kind: TopicKind::Work,
        name,
        aliases: Vec::new(),
        parent: None,
        directory: None,
        owner: Some(crate::owner()?),
    };
    store.create(Topic::create(created, new, at(2))?).await?;
    store
        .place(Placement {
            session: members.first().ok_or("a member")?.clone(),
            topic: created,
            by: PlacementSource::Categorizer,
            at: at(2),
            owner: Some(crate::owner()?),
        })
        .await?;
    store.merge(client.id, app.id, at(2)).await?;
    store.delete(spike.id).await?;
    let owner = crate::owner()?;
    let ports = TidyPorts {
        tidy: store,
        topics: store,
        filing: store,
        pages: Some(store),
        owner: &owner,
    };
    let outcome = kernel::tidy::decide_batch(&ports, stored.id, Decision::Approve, at(3)).await?;
    finished_on_retry(&outcome)?;
    let mut filed = Vec::new();
    for member in &members {
        filed.push(store.placement(member).await?.map(|placed| placed.topic));
    }
    same(
        "the members left over are filed",
        filed,
        vec![Some(created); members.len()],
    )?;
    Ok(())
}

pub async fn old_names_stay_findable_and_a_change_made_moot_is_skipped<S>(store: &S) -> Outcome
where
    S: TidyStore + TopicStore + FilingStore + TopicPageStore,
{
    let jest = store.create(new_topic("Jest", &[], None)?).await?;
    let vitest = store.create(new_topic("Vitest", &[], None)?).await?;
    let gerty = store.create(new_topic("Gerty", &[], None)?).await?;
    let stored = store.propose(moot_batch(&vitest, &jest, &gerty)?).await?;
    let [merge, delete, rename] = stored
        .proposals
        .iter()
        .map(|proposal| proposal.id)
        .collect::<Vec<_>>()[..]
    else {
        return Err("three proposals were kept".into());
    };
    let owner = crate::owner()?;
    let ports = TidyPorts {
        tidy: store,
        topics: store,
        filing: store,
        pages: Some(store),
        owner: &owner,
    };
    let outcome = kernel::tidy::decide_batch(&ports, stored.id, Decision::Approve, at(2)).await?;
    same(
        "the merge and the rename are approved",
        outcome
            .decided
            .iter()
            .map(|proposal| (proposal.id, proposal.state))
            .collect::<Vec<_>>(),
        vec![
            (merge, ProposalState::Approved),
            (rename, ProposalState::Approved),
        ],
    )?;
    same(
        "the delete the merge made moot is skipped, with why",
        outcome
            .skipped
            .iter()
            .map(|skipped| (skipped.proposal.id, skipped.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![(delete, "the topic it changes is already gone")],
    )?;
    old_names_found(store, [("Vitest", &jest, "Jest"), ("Gerty", &gerty, "g9")]).await
}

async fn old_names_found<S>(store: &S, kept: [(&str, &Topic, &str); 2]) -> Outcome
where
    S: TopicStore,
{
    for (old, topic, name) in kept {
        let found = store
            .search(&kernel::topics::TopicQuery::new(old), 5)
            .await?;
        same(
            "the old name finds the topic",
            found
                .iter()
                .map(|listing| listing.topic.name.as_str())
                .collect::<Vec<_>>(),
            vec![name],
        )?;
        let stored = store
            .find(&TopicKey::Id(topic.id))
            .await?
            .ok_or("the topic stays")?;
        same(
            "the old name is an alias",
            stored.aliases.iter().map(Alias::as_str).collect::<Vec<_>>(),
            vec![old],
        )?;
    }
    Ok(())
}

fn moot_batch(
    vitest: &Topic,
    jest: &Topic,
    gerty: &Topic,
) -> Result<Batch, kernel::tidy::TidyError> {
    let mut batch = Batch::new(Proposer::Categorizer, at(1));
    batch.add(
        TreeEdit::Merge {
            source: vitest.id,
            into: jest.id,
        },
        Reason::new("one test runner")?,
        evidence(2),
    );
    batch.add(
        TreeEdit::Delete { topic: vitest.id },
        Reason::new("not a tool we use")?,
        evidence(0),
    );
    batch.add(
        TreeEdit::Rename {
            topic: gerty.id,
            name: TopicName::new("g9")?,
        },
        Reason::new("the host is called g9")?,
        evidence(1),
    );
    Ok(batch)
}

fn finished_on_retry(outcome: &kernel::tidy::BatchOutcome) -> Outcome {
    same(
        "nothing fails on the second approval",
        outcome
            .failed
            .iter()
            .map(|(_, error)| error.to_string())
            .collect::<Vec<_>>(),
        Vec::new(),
    )?;
    same(
        "the change still to make is approved",
        outcome.decided.len(),
        1,
    )?;
    same(
        "the changes already made are skipped as gone",
        outcome
            .skipped
            .iter()
            .map(|skipped| skipped.reason.as_str())
            .collect::<Vec<_>>(),
        vec!["the topic it changes is already gone"; 2],
    )?;
    Ok(())
}

fn stopped_batch(
    create: TreeEdit,
    merge: TreeEdit,
    delete: TreeEdit,
) -> Result<Batch, kernel::tidy::TidyError> {
    let mut batch = Batch::new(Proposer::Backfill, at(1));
    batch.add(
        create,
        Reason::new("three sessions suggested it")?,
        evidence(3),
    );
    batch.add(merge, Reason::new("one app under two names")?, evidence(2));
    batch.add(delete, Reason::new("nothing is filed there")?, evidence(0));
    Ok(batch)
}

pub async fn upkeep_counts_filings_entities_and_finished_jobs<S>(
    store: &S,
    queue: &dyn JobQueue,
) -> Outcome
where
    S: UpkeepStore + TopicStore + FilingStore + MemoryStore,
{
    let topic = store.create(new_topic("Desktop App", &[], None)?).await?;
    let filed = session("00000000-0000-4000-8000-000000000071")?;
    let yours = session("00000000-0000-4000-8000-000000000072")?;
    let bare = "00000000-0000-4000-8000-000000000073";
    capture(store, filed.as_str(), "claude-code", "Rail polish", None).await?;
    capture(store, yours.as_str(), "claude-code", "Counting", None).await?;
    capture(store, bare, "claude-code", "Older capture", None).await?;
    capture(
        store,
        "grokbot-run-sand-subagent-4",
        "grokbot",
        "Routine",
        None,
    )
    .await?;
    capture(store, "qwen-code:9", "qwen-code", "Quantised bench", None).await?;
    for (placed, by) in [
        (filed.clone(), PlacementSource::Categorizer),
        (yours, PlacementSource::You),
        (session(bare)?, PlacementSource::Categorizer),
    ] {
        store
            .place(Placement {
                session: placed,
                topic: topic.id,
                by,
                at: at(1),
                owner: Some(crate::owner()?),
            })
            .await?;
    }
    store
        .correct(Correction {
            target: Target::Session(filed),
            from: topic.id,
            to: None,
            at: at(10),
        })
        .await?;
    let tally = store.filing(at(5)).await?;
    same(
        "every conversation is counted once",
        (
            tally.categorizer,
            tally.you,
            tally.left_out,
            tally.uncategorized,
            tally.corrections,
        ),
        (2, 1, 1, 1, 1),
    )?;
    entities_and_jobs(store, queue).await
}

async fn entities_and_jobs<S>(store: &S, queue: &dyn JobQueue) -> Outcome
where
    S: UpkeepStore + TopicStore,
{
    let people = store
        .find(&TopicKey::Slug(TopicKind::Type, Slug::fold("people")?))
        .await?
        .ok_or("the People type is seeded")?;
    store
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Entity,
                name: TopicName::new("Ada")?,
                aliases: Vec::new(),
                parent: Some(people.id),
                directory: None,
                owner: Some(crate::owner()?),
            },
            at(0),
        )?)
        .await?;
    same(
        "entities are counted under their type",
        store
            .wiki()
            .await?
            .types
            .iter()
            .find(|tally| tally.topic == people.id)
            .map(|tally| tally.entities),
        Some(1),
    )?;
    let job = |key: &str| Job {
        name: JobName::Fixed(FixedJob::EmbedTarget),
        payload: JsonObject::default(),
        key: Some(key.to_owned()),
        key_mode: JobKeyMode::Replace,
        priority: 0,
        run_at: None,
        max_attempts: Some(5),
    };
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::EmbedTarget)]);
    for (key, finished) in [("upkeep-done", true), ("upkeep-failing", false)] {
        queue.enqueue(job(key)).await?;
        let claimed = queue.claim(&selector).await?.ok_or("a job is claimed")?;
        if finished {
            queue.complete(claimed.id).await?;
        } else {
            queue.fail(claimed.id, "embedding endpoint refused").await?;
        }
    }
    let embedder = RoleActivity::of(WorkerRole::Embedder, &store.tasks().await?);
    same(
        "the embedder finished one job and has one failing",
        (embedder.last_finished.is_some(), embedder.failing),
        (true, 1),
    )?;
    Ok(())
}

pub async fn samples_show_each_session_s_current_title<S>(store: &S) -> Outcome
where
    S: TidyStore + TopicStore + MemoryStore,
{
    let spike = store.create(new_topic("Old Spike", &[], None)?).await?;
    let renamed = session("00000000-0000-4000-8000-000000000081")?;
    capture(
        store,
        renamed.as_str(),
        "claude-code",
        "Port the parser",
        None,
    )
    .await?;
    let sample = |id: &str| Sample {
        id: id.to_owned(),
        title: Some("Claude Code session".to_owned()),
        last_active: None,
    };
    let mut batch = Batch::new(Proposer::Backfill, at(1));
    batch.add(
        TreeEdit::Close { topic: spike.id },
        Reason::new("idle for months")?,
        Evidence {
            sessions: 2,
            samples: vec![sample(renamed.as_str()), sample("claude-code:gone")],
        },
    );
    store.propose(batch).await?;
    let titles: Vec<Option<String>> = store
        .pending()
        .await?
        .iter()
        .flat_map(|batch| &batch.proposals)
        .flat_map(|proposal| &proposal.evidence.samples)
        .map(|sample| sample.title.clone())
        .collect();
    same(
        "samples read the conversation's title now",
        titles,
        vec![
            Some("Port the parser".to_owned()),
            Some("Claude Code session".to_owned()),
        ],
    )?;
    Ok(())
}
