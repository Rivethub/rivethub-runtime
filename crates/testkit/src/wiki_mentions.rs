use kernel::topics::Alias;
use kernel::topics::NewTopic;
use kernel::topics::Slug;
use kernel::topics::Topic;
use kernel::topics::TopicChanges;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
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
use crate::wiki_routes::host;
use crate::wiki_routes::patch;

pub const EARLIER_KEY: &str = "claude-code:00000000-0000-4000-8000-000000000043";

type Found<T> = Result<T, Box<dyn std::error::Error>>;

pub async fn an_entity_page_is_not_written_by_the_topic_router(
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
    same(
        "no entity page",
        stores.named(TopicKind::Entity, HOST).await?,
        None,
    )?;
    let cited: Vec<String> = stores
        .pages
        .shown("acmekit")
        .await?
        .citations
        .into_iter()
        .map(|citation| citation.summary_id)
        .collect();
    same(
        "filed topic still cited",
        cited,
        vec![job.summary_id.to_string()],
    )?;
    same(
        "mentions stay empty",
        mentioned(&stores, acmekit.id).await?,
        Vec::new(),
    )?;
    Ok(())
}

pub async fn a_held_entity_mention_is_left_for_the_entity_worker(
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
    same(
        "the entity topic stays",
        stores
            .named(TopicKind::Entity, HOST)
            .await?
            .as_ref()
            .map(|topic| topic.id),
        Some(entity.id),
    )?;
    same(
        "the hold stays for the entity worker",
        stores.wiki.held_entities(&key).await?.len(),
        1,
    )?;
    Ok(())
}

pub async fn an_entity_type_path_does_not_create_a_page(
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
    same(
        "no entity page",
        stores.named(TopicKind::Entity, "AcmeKit Desktop").await?,
        None,
    )?;
    let paths = stores.router().type_paths().await?;
    same(
        "the path is still known",
        paths.iter().any(|path| path == "Software › Apps"),
        true,
    )?;
    same("apps type stays", apps.name.as_str(), "Apps")?;
    Ok(())
}

pub async fn an_entity_name_is_not_merged_by_the_topic_router(
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
    let entities: Vec<String> = stores
        .pages
        .topics
        .list()
        .await?
        .into_iter()
        .filter(|listing| listing.topic.kind == TopicKind::Entity)
        .map(|listing| listing.topic.name.as_str().to_owned())
        .collect();
    same(
        "the topic router creates no entity",
        entities,
        Vec::<String>::new(),
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
