use async_trait::async_trait;
use kernel::BoxError;
use kernel::topics::TopicKind;
use kernel::wiki::builder::ExtractWiki;
use kernel::wiki::builder::WikiWork;
use kernel::wiki::merge::MergeInput;
use kernel::wiki::merge::Merged;
use kernel::wiki::merge::PageMerger;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::route::TopicTarget;
use kernel::wiki::route::WikiRouter;
use parking_lot::Mutex;

use crate::topics::Outcome;
use crate::topics::new_topic;
use crate::topics::same;
use crate::wiki_routes::HOST;
use crate::wiki_routes::RouteStores;
use crate::wiki_routes::host;
use crate::wiki_routes::patch;

type Found<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
pub struct RecordingMerger {
    pub seen: Mutex<Vec<MergeInput>>,
}

#[async_trait]
impl PageMerger for RecordingMerger {
    async fn merge(&self, input: &MergeInput) -> Result<Merged, BoxError> {
        self.seen.lock().push(input.clone());
        Ok(Merged {
            summary: format!("{} Merged.", input.summary),
            article: format!("{}\n\n{}", input.article, input.material.join("\n\n")),
            history: Some("Merged the new mention.".to_owned()),
        })
    }
}

async fn target(
    stores: RouteStores<'_>,
    job: &ExtractWiki,
) -> Found<(kernel::wiki::builder::WikiSummary, Option<TopicTarget>)> {
    let WikiWork::Pending(summary) = stores.wiki.wiki_work(job).await? else {
        return Err("the summary was already extracted".into());
    };
    let target = stores.router().target(&summary).await?;
    Ok((summary, target))
}

async fn apply(
    router: &WikiRouter<'_>,
    job: &ExtractWiki,
    read: &(kernel::wiki::builder::WikiSummary, Option<TopicTarget>),
    patches: Vec<WikiPatch>,
) -> Outcome {
    router.apply(job, &read.0, read.1.as_ref(), patches).await?;
    Ok(())
}

pub async fn a_sub_topic_rewrite_keeps_its_lead(
    stores: RouteStores<'_>,
    first: &ExtractWiki,
    second: &ExtractWiki,
) -> Outcome {
    let infra = stores.pages.create(new_topic("Infra", &[], None)?).await?;
    let certificates = stores
        .pages
        .create(new_topic("Certificates", &[], Some(infra.id))?)
        .await?;
    stores.place(&certificates).await?;
    let old = "Certificates are issued by step-ca. Currently they renew every week.";
    stores
        .route(first, vec![patch("infra", "Certificates", old)?])
        .await?;
    let read = target(stores, second).await?;
    let current = read
        .1
        .as_ref()
        .and_then(|topic| topic.current.as_ref())
        .and_then(|current| current.section.clone());
    same("the model sees the section", current.as_deref(), Some(old))?;
    let new = "Certificates come from the mesh CA. Currently they renew every day since the move.";
    apply(
        &stores.router(),
        second,
        &read,
        vec![patch("infra", "Certificates", new)?],
    )
    .await?;
    let page = stores.pages.page(&infra).await?;
    same(
        "the lead keeps its description and takes the new Currently sentence",
        page.sections
            .iter()
            .map(|section| section.body.as_str())
            .collect::<Vec<_>>(),
        vec!["Certificates are issued by step-ca. Currently they renew every day since the move."],
    )?;
    Ok(())
}

pub async fn concurrent_rewrites_of_one_page_keep_both(
    stores: RouteStores<'_>,
    first: &ExtractWiki,
    second: &ExtractWiki,
) -> Outcome {
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores.place(&acmekit).await?;
    let one = target(stores, first).await?;
    let two = target(stores, second).await?;
    let mut ships = patch("acmekit", "AcmeKit", "AcmeKit ships on Fridays.")?;
    ships.article = Some("### Releases\n\nReleases go out on Fridays.".to_owned());
    let mut hosts = patch("acmekit", "AcmeKit", "AcmeKit builds on the build box.")?;
    hosts.article = Some("### Builds\n\nBuilds run on the build box.".to_owned());
    let router = stores.router();
    let (left, right) = tokio::join!(
        apply(&router, first, &one, vec![ships]),
        apply(&router, second, &two, vec![hosts])
    );
    left?;
    right?;
    let page = stores.pages.page(&acmekit).await?;
    let leads = [
        "AcmeKit ships on Fridays.",
        "AcmeKit builds on the build box.",
    ];
    same(
        "one stable lead",
        leads
            .iter()
            .filter(|lead| page.summary.contains(*lead))
            .count(),
        1,
    )?;
    for kept in [
        "Releases go out on Fridays.",
        "Builds run on the build box.",
    ] {
        same(kept, page.article.contains(kept), true)?;
    }
    Ok(())
}

pub async fn an_entity_mention_is_merged_into_its_page(
    stores: RouteStores<'_>,
    first: &ExtractWiki,
    second: &ExtractWiki,
) -> Outcome {
    let built = "The build box runs the nightly AcmeKit builds.";
    let mention = host(built)?;
    stores.mentioned_elsewhere(first, &mention).await?;
    stores.route(first, vec![mention]).await?;
    let entity = stores
        .named(TopicKind::Entity, HOST)
        .await?
        .ok_or("the second mention creates the entity")?;
    let merger = RecordingMerger::default();
    let router = stores.router_with(&merger);
    let read = {
        let WikiWork::Pending(summary) = stores.wiki.wiki_work(second).await? else {
            return Err("the summary was already extracted".into());
        };
        (summary, None)
    };
    apply(
        &router,
        second,
        &read,
        vec![host("It now has 64 GB of memory.")?],
    )
    .await?;
    let seen = merger.seen.lock().clone();
    same("one merge", seen.len(), 1)?;
    let input = seen.first().ok_or("merge input")?;
    same("current summary sent", input.summary.as_str(), built)?;
    same(
        "mention sent",
        input
            .material
            .iter()
            .any(|text| text.contains("64 GB of memory")),
        true,
    )?;
    let page = stores.pages.page(&entity).await?;
    same("the lead stays", page.summary.as_str(), built)?;
    same(
        "the merged article lands",
        page.article.contains("64 GB of memory"),
        true,
    )?;
    Ok(())
}

pub async fn updates_keep_the_lead_and_replace_only_the_currently_sentence(
    stores: RouteStores<'_>,
    jobs: [&ExtractWiki; 3],
) -> Outcome {
    let acmekit = stores
        .pages
        .create(new_topic("AcmeKit", &[], None)?)
        .await?;
    stores.place(&acmekit).await?;
    let [first, second, third] = jobs;
    let lead = "AcmeKit is the team's deploy tool.";
    let opening = format!("{lead} Currently, releases ship on Fridays.");
    stores
        .route(first, vec![patch("acmekit", "AcmeKit", &opening)?])
        .await?;
    let update = "Release 2.0 shipped today with new dashboards. Currently, releases ship daily.";
    stores
        .route(second, vec![patch("acmekit", "AcmeKit", update)?])
        .await?;
    let expected = format!("{lead} Currently, releases ship daily.");
    same(
        "lead kept, currently replaced",
        stores.pages.page(&acmekit).await?.summary,
        expected.clone(),
    )?;
    let news = "The billing team adopted AcmeKit.";
    stores
        .route(third, vec![patch("acmekit", "AcmeKit", news)?])
        .await?;
    same(
        "news without a currently sentence leaves the lead alone",
        stores.pages.page(&acmekit).await?.summary,
        expected,
    )?;
    Ok(())
}
