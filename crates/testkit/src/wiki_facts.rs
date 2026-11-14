use kernel::facts::Claim;
use kernel::facts::FactKey;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::facts::facts_as_of;
use kernel::topics::NewTopic;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::page::PatchFact;
use kernel::wiki::page::WikiPatch;

use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::same;
use crate::wiki_routes::HOST;
use crate::wiki_routes::RouteStores;
use crate::wiki_routes::host;

type Found<T> = Result<T, Box<dyn std::error::Error>>;

fn host_with(text: &str, facts: &[(&str, &str)]) -> Found<WikiPatch> {
    let mut patch = host(text)?;
    patch.facts = Some(
        facts
            .iter()
            .map(|(key, value)| PatchFact {
                key: (*key).to_owned(),
                value: (*value).to_owned(),
                evidence: format!("{text} The {key} is {value}."),
            })
            .collect(),
    );
    Ok(patch)
}

type ShownFact = (String, String, bool, Vec<String>, Vec<String>);

fn shown_facts(claims: &[Claim]) -> Vec<ShownFact> {
    facts_as_of(claims, None)
        .into_iter()
        .map(|fact| {
            (
                fact.key.as_str().to_owned(),
                fact.value.as_str().to_owned(),
                fact.pinned,
                fact.sources
                    .iter()
                    .map(|source| source.session.as_str().to_owned())
                    .collect(),
                fact.candidates
                    .iter()
                    .map(|candidate| {
                        format!(
                            "{} {}",
                            candidate.session.as_str(),
                            candidate.value.as_str()
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

pub async fn an_entity_patch_records_its_facts_and_never_overwrites_a_pin(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let hosts = stores
        .named(TopicKind::Type, "Hosts")
        .await?
        .ok_or("the Hosts type is seeded")?;
    let entity = stores
        .pages
        .create(Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Entity,
                name: TopicName::new(HOST)?,
                aliases: Vec::new(),
                parent: Some(hosts.id),
                directory: None,
                owner: Some(crate::owner()?),
            },
            at(0),
        )?)
        .await?;
    stores
        .facts
        .assert_facts(&[NewClaim {
            entity: entity.id,
            key: FactKey::new("os")?,
            value: FactValue::new("Debian 12")?,
            said: Said::You {
                by: crate::owner()?,
                pinned: true,
            },
            owner: None,
            at: at(5),
        }])
        .await?;
    let text = "The build box runs the nightly AcmeKit builds.";
    stores
        .route(
            job,
            vec![host_with(
                text,
                &[
                    ("OS", "Ubuntu 24.04"),
                    ("role", "build box"),
                    (" ", "unnamed"),
                ],
            )?],
        )
        .await?;
    same(
        "the topic router leaves the pin and writes no entity claim",
        shown_facts(&stores.facts.claims(entity.id).await?),
        vec![(
            "os".to_owned(),
            "Debian 12".to_owned(),
            true,
            Vec::new(),
            Vec::new(),
        )],
    )?;
    Ok(())
}

pub async fn a_held_candidate_keeps_its_own_session_for_its_facts(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let text = "The build box runs the nightly AcmeKit builds.";
    stores
        .mentioned_elsewhere(job, &host_with(text, &[("os", "Ubuntu 22.04")])?)
        .await?;
    stores
        .route(job, vec![host_with(text, &[("address", "192.0.2.7")])?])
        .await?;
    same(
        "the topic router does not create the entity",
        stores.named(TopicKind::Entity, HOST).await?,
        None,
    )?;
    Ok(())
}
