use kernel::topics::Alias;
use kernel::topics::NewTopic;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicChanges;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::wiki::WikiFilter;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::HeldEntity;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;

use crate::topics::Outcome;
use crate::topics::at;
use crate::topics::new_topic;
use crate::topics::same;
use crate::wiki_routes::HOST;
use crate::wiki_routes::RouteStores;
use crate::wiki_routes::SESSION_KEY;
use crate::wiki_routes::SESSION_TITLE;
use crate::wiki_routes::host;
use crate::wiki_routes::patch;

pub const EARLIER_KEY: &str = "claude-code:00000000-0000-4000-8000-000000000043";

type Found<T> = Result<T, Box<dyn std::error::Error>>;

pub async fn an_entity_page_names_the_sessions_that_cite_it(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores.place(&acmekit).await?;
    let host = host("The build box runs the nightly AcmeKit builds.")?;
    stores.mentioned_elsewhere(job, &host).await?;
    stores.route(job, vec![host]).await?;
    let entity = stores
        .named(TopicKind::Entity, HOST)
        .await?
        .ok_or("the second mention creates the entity")?;
    let slug = stores.pages.slug(&entity).await?;
    let cited: Vec<_> = stores
        .pages
        .shown(slug.as_ref())
        .await?
        .citations
        .into_iter()
        .map(|citation| {
            (
                citation.conversation_id,
                citation
                    .session_id
                    .map(|session| session.as_str().to_owned()),
                citation.title,
                citation.topic_id,
            )
        })
        .collect();
    same(
        "cited by",
        cited,
        vec![(
            job.conversation_id.map(|id| id.to_string()),
            Some(SESSION_KEY.to_owned()),
            Some(SESSION_TITLE.to_owned()),
            Some(acmekit.id.to_string()),
        )],
    )?;
    same(
        "the topic mentions the entity",
        mentioned(&stores, acmekit.id).await?,
        vec![(entity.id, HOST.to_owned())],
    )?;
    let elsewhere = stores
        .pages
        .create(new_topic("Billing", &[], None)?)
        .await?;
    same(
        "another topic mentions nothing",
        mentioned(&stores, elsewhere.id).await?,
        Vec::new(),
    )?;
    same(
        "owners",
        owners(&stores).await?,
        vec![
            (slug.as_ref().to_owned(), Some(entity.id.to_string())),
            ("acmekit".to_owned(), Some(acmekit.id.to_string())),
            ("billing".to_owned(), Some(elsewhere.id.to_string())),
        ],
    )?;
    Ok(())
}

pub async fn a_mention_held_before_its_entity_existed_still_lands(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
    earlier: &ExtractWiki,
) -> Outcome {
    let hosts = stores
        .named(TopicKind::Type, "Hosts")
        .await?
        .ok_or("the Hosts type is seeded")?;
    let key = Slug::fold(HOST)?;
    stores
        .wiki
        .hold_entity(HeldEntity {
            key: key.clone(),
            source: EARLIER_KEY.to_owned(),
            summary_id: earlier.summary_id,
            patch: host("The build box sits in the lab rack.")?,
            at: at(2),
        })
        .await?;
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
        .route(job, vec![host("The build box runs the nightly builds.")?])
        .await?;
    let slug = stores.pages.slug(&entity).await?;
    let mut cited: Vec<String> = stores
        .pages
        .shown(slug.as_ref())
        .await?
        .citations
        .into_iter()
        .map(|citation| citation.summary_id)
        .collect();
    cited.sort();
    let mut both = vec![earlier.summary_id.to_string(), job.summary_id.to_string()];
    both.sort();
    same("the held mention is written and cited", cited, both)?;
    same(
        "the written mention is no longer held",
        stores.wiki.held_entities(&key).await?,
        Vec::new(),
    )?;
    Ok(())
}

pub async fn an_entity_type_is_found_by_its_whole_path(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let agents = stores
        .named(TopicKind::Type, "Agents")
        .await?
        .ok_or("the Agents type is seeded")?;
    let apps = stores
        .named(TopicKind::Type, "Apps")
        .await?
        .ok_or("the Apps type is seeded")?;
    let changes = TopicChanges {
        aliases: Some(vec![Alias::new("Apps")?]),
        ..TopicChanges::default()
    };
    stores
        .pages
        .topics
        .update(agents.id, changes, at(1))
        .await?;
    let mut app = patch("acmekit-desktop", "AcmeKit Desktop", "The desktop client.")?;
    app.entity_type = Some("Software › Apps".to_owned());
    stores.mentioned_elsewhere(job, &app).await?;
    stores.route(job, vec![app]).await?;
    let entity = stores
        .named(TopicKind::Entity, "AcmeKit Desktop")
        .await?
        .ok_or("the second mention creates the entity")?;
    same("the type its path names", entity.parent, Some(apps.id))?;
    Ok(())
}

pub async fn an_entity_goes_by_its_names_never_by_a_description(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Outcome {
    let activity = named_host(
        "building-acmekit",
        "Building AcmeKit on the build box",
        "Built the nightly.",
    )?;
    let mut qualified = named_host(
        HOST,
        "acme-build (the lab build box)",
        "Runs the nightly builds.",
    )?;
    qualified.aliases = vec![WikiSlug::try_from("buildbox".to_owned())?];
    stores.mentioned_elsewhere(job, &activity).await?;
    stores.mentioned_elsewhere(job, &qualified).await?;
    let renamed = named_host("buildbox", "BuildBox", "Sits in the lab rack.")?;
    let mut learned = named_host("lab-box", "lab-box", "Has 64 GB of memory.")?;
    learned.aliases = vec![WikiSlug::try_from("buildbox".to_owned())?];
    stores
        .route(job, vec![activity, qualified, renamed, learned])
        .await?;
    let entities: Vec<(String, Vec<String>)> = stores
        .pages
        .topics
        .list()
        .await?
        .into_iter()
        .filter(|listing| listing.topic.kind == TopicKind::Entity)
        .map(|listing| {
            (
                listing.topic.name.as_str().to_owned(),
                listing
                    .topic
                    .aliases
                    .iter()
                    .map(|alias| alias.as_str().to_owned())
                    .collect(),
            )
        })
        .collect();
    same(
        "one entity under its name, found by its aliases",
        entities,
        vec![(
            HOST.to_owned(),
            vec!["buildbox".to_owned(), "lab-box".to_owned()],
        )],
    )?;
    Ok(())
}

fn named_host(slug: &str, title: &str, text: &str) -> Found<WikiPatch> {
    let mut mention = patch(slug, title, text)?;
    mention.entity_type = Some("Hosts".to_owned());
    Ok(mention)
}

async fn mentioned(stores: &RouteStores<'_>, topic: TopicId) -> Found<Vec<(TopicId, String)>> {
    Ok(stores
        .filing
        .mentions(&[topic], 10)
        .await?
        .into_iter()
        .map(|mention| (mention.entity, mention.name.as_str().to_owned()))
        .collect())
}

async fn owners(stores: &RouteStores<'_>) -> Found<Vec<(String, Option<String>)>> {
    let listed = stores
        .pages
        .wiki
        .wiki_topics(WikiFilter {
            query: None,
            tag: None,
            limit: 50,
            offset: 0,
        })
        .await?;
    let mut owners: Vec<(String, Option<String>)> = listed
        .topics
        .into_iter()
        .map(|topic| (topic.slug, topic.topic_id))
        .collect();
    owners.sort();
    Ok(owners)
}
