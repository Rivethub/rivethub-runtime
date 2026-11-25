use jiff::Timestamp;
use std::path::Path;
use wire::time::TimestampFormat;

use kernel::topics::PageEdit;
use kernel::topics::PageSection;
use kernel::topics::TopicId;
use kernel::topics::TopicPage;
use kernel::wiki::builder::WikiRouting;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::builder::WikiSummary;
use kernel::wiki::merge::MergedPage;
use kernel::wiki::page::ArticleSection;
use kernel::wiki::page::CitationKind;
use kernel::wiki::page::PageCitation;
use kernel::wiki::page::PagePatch;
use kernel::wiki::page::PageSource;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::WikiSectionRef;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::WikiSourceKind;
use kernel::wiki::page::bound::MergeBase;
use kernel::wiki::page::build_wiki_search_text;
use rusqlite::Connection;
use rusqlite::params;
use wiki_files::wiki_writer::AppliedPage;
use wiki_files::wiki_writer::LockedRepository;

use crate::page_files::Files;
use crate::time;
use crate::wiki_builder::query;

pub(crate) struct FiledPatch {
    pub(crate) slug: String,
    pub(crate) wiki: kernel::wiki::page::WikiPatch,
    pub(crate) page: PagePatch,
    pub(crate) merge: Option<MergeBase>,
    pub(crate) note: Option<String>,
    pub(crate) stored: Option<TopicPage>,
}

pub(crate) fn write_in_order(
    locked: &mut LockedRepository,
    conn: &Connection,
    summary: &WikiSummary,
    patches: Vec<kernel::wiki::page::WikiPatch>,
    routing: &WikiRouting,
    now: Timestamp,
) -> Result<(Vec<String>, Option<String>), WikiStoreError> {
    let mut touched = Vec::new();
    let mut last_sha = None;
    for patch in patches {
        let planned = plan(conn, summary, vec![patch], routing, now)?;
        let applied = tokio::runtime::Handle::current()
            .block_on(commit_pages(locked, conn, summary, &planned))?;
        crate::page_files::pause_before_row();
        let (slugs, sha) = remember_pages(conn, summary, routing, &planned, &applied, now)?;
        touched.extend(slugs);
        last_sha = sha.or(last_sha);
    }
    touched.sort();
    touched.dedup();
    Ok((touched, last_sha))
}

pub(crate) fn plan(
    conn: &Connection,
    summary: &WikiSummary,
    patches: Vec<kernel::wiki::page::WikiPatch>,
    routing: &WikiRouting,
    now: Timestamp,
) -> Result<Vec<FiledPatch>, WikiStoreError> {
    let mut planned = Vec::new();
    for patch in patches {
        planned.push(one(conn, summary, &patch, routing, now)?);
    }
    Ok(planned)
}

fn one(
    conn: &Connection,
    summary: &WikiSummary,
    patch: &kernel::wiki::page::WikiPatch,
    routing: &WikiRouting,
    now: Timestamp,
) -> Result<FiledPatch, WikiStoreError> {
    let slug = crate::wiki_topics::canonical(conn, patch, routing.topic_page.as_ref())?;
    let exists = exists(conn, &slug)?;
    let note = exists && slug != patch.slug.as_ref();
    let mut page = page_patch(summary, patch, &slug, exists, routing, now);
    let merge = merge_base(&routing.merged, &slug, note, &mut page).cloned();
    if note {
        note_patch(&mut page, patch, summary, now);
    }
    page.add_aliases =
        crate::wiki_topics::claimable_aliases(conn, &slug, std::mem::take(&mut page.add_aliases))?;
    let noted = page.history_entry.as_ref().map(|entry| entry.body.clone());
    let stored = stored_page(conn, &slug, exists)?;
    Ok(FiledPatch {
        slug,
        wiki: patch.clone(),
        page,
        merge,
        note: noted,
        stored,
    })
}

fn stored_page(
    conn: &Connection,
    slug: &str,
    exists: bool,
) -> Result<Option<TopicPage>, WikiStoreError> {
    if !exists {
        return Ok(None);
    }
    let slug = WikiSlug::try_from(slug.to_owned())
        .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
    crate::topic_pages::read(conn, &slug)
}

pub(crate) async fn commit_pages(
    locked: &mut LockedRepository,
    conn: &Connection,
    summary: &WikiSummary,
    planned: &[FiledPatch],
) -> Result<Vec<AppliedPage>, WikiStoreError> {
    let mut applied = Vec::new();
    for item in planned {
        if let Some(stored) = &item.stored {
            crate::wiki_materialize::record_stored(
                locked,
                conn,
                &stored.slug,
                item.page.verified_at,
            )
            .await?;
        }
        applied.push(
            locked
                .apply(
                    item.page.clone(),
                    &summary.id.to_string(),
                    item.merge.as_ref(),
                )
                .await
                .map_err(WikiStoreError::from)?,
        );
    }
    Ok(applied)
}

pub(crate) fn remember_pages(
    conn: &Connection,
    summary: &WikiSummary,
    routing: &WikiRouting,
    planned: &[FiledPatch],
    applied: &[AppliedPage],
    now: Timestamp,
) -> Result<(Vec<String>, Option<String>), WikiStoreError> {
    if planned.len() != applied.len() {
        return Err(WikiStoreError::InvalidPatch(
            "wiki file apply lost a page".to_owned(),
        ));
    }
    let mut touched = Vec::new();
    let mut last_sha = None;
    for (item, done) in planned.iter().zip(applied.iter()) {
        upsert(conn, &done.page, &done.git_sha, now)?;
        remember(conn, summary, routing, item, done, now)?;
        touched.push(item.slug.clone());
        last_sha = Some(done.git_sha.clone());
    }
    Ok((touched, last_sha))
}

fn page_patch(
    summary: &WikiSummary,
    patch: &kernel::wiki::page::WikiPatch,
    slug: &str,
    exists: bool,
    routing: &WikiRouting,
    now: Timestamp,
) -> PagePatch {
    let note = patch
        .history_entry
        .as_ref()
        .map_or_else(|| patch.title.clone(), |entry| entry.title.clone());
    let cited = attributed(patch, routing.topic_page.as_ref(), summary, &routing.also);
    let (add_citations, add_sources) = citations(&cited, &note);
    PagePatch {
        action: if exists {
            WikiAction::Update
        } else {
            WikiAction::Create
        },
        slug: slug.to_owned(),
        verified_at: now,
        title: Some(patch.title.clone()),
        add_aliases: patch
            .aliases
            .iter()
            .map(|alias| alias.as_ref().to_owned())
            .collect(),
        add_tags: patch.tags.clone(),
        add_entities: patch.entities.clone(),
        add_related: patch
            .related
            .iter()
            .flatten()
            .map(|slug| slug.as_ref().to_owned())
            .collect(),
        summary_delta: Some(patch.summary.clone()),
        current_state: None,
        article: patch.article.clone(),
        article_patches: Vec::new(),
        history_entry: patch.history_entry.clone(),
        add_citations,
        add_sources,
        allow_shrink: false,
    }
}

fn note_patch(
    page_patch: &mut PagePatch,
    patch: &kernel::wiki::page::WikiPatch,
    summary: &WikiSummary,
    now: Timestamp,
) {
    let day = summary.created_at.unwrap_or(now).utc_date();
    page_patch.title = None;
    page_patch.add_aliases = vec![patch.slug.as_ref().to_owned()];
    page_patch.add_tags.clear();
    page_patch.add_entities.clear();
    page_patch.summary_delta = None;
    page_patch.article = None;
    page_patch.history_entry = Some(patch.note(day));
}

fn merge_base<'a>(
    merged: &'a [MergedPage],
    slug: &str,
    note: bool,
    patch: &mut PagePatch,
) -> Option<&'a MergeBase> {
    let base = merged
        .iter()
        .find(|page| !note && page.slug.as_ref() == slug)
        .map(|page| &page.base)?;
    patch.current_state = patch.summary_delta.take();
    patch.allow_shrink = true;
    Some(base)
}

fn attributed<'a>(
    patch: &kernel::wiki::page::WikiPatch,
    topic_page: Option<&WikiSlug>,
    lead: &'a WikiSummary,
    also: &'a [WikiSummary],
) -> Vec<&'a WikiSummary> {
    let batch: Vec<&WikiSummary> = std::iter::once(lead).chain(also).collect();
    if also.is_empty() || topic_page == Some(&patch.slug) {
        return batch;
    }
    let mut names = vec![patch.title.to_lowercase()];
    for slug in std::iter::once(&patch.slug).chain(&patch.aliases) {
        names.push(slug.as_ref().to_owned());
        names.push(slug.as_ref().replace('-', " "));
    }
    names.retain(|name| name.chars().count() >= 3);
    let mentioning: Vec<&WikiSummary> = batch
        .into_iter()
        .filter(|summary| {
            let text = summary.content.to_lowercase();
            names.iter().any(|name| text.contains(name.as_str()))
        })
        .collect();
    if mentioning.is_empty() {
        vec![lead]
    } else {
        mentioning
    }
}

fn citations(cited_by: &[&WikiSummary], note: &str) -> (Vec<PageCitation>, Vec<PageSource>) {
    cited_by
        .iter()
        .map(|summary| {
            let id = summary.id.to_string();
            (
                PageCitation {
                    summary_id: id.clone(),
                    date: summary.created_at.map(TimestampFormat::utc_date),
                    kind: Some(CitationKind::Leaf),
                    note: Some(note.to_owned()),
                },
                PageSource {
                    kind: WikiSourceKind::Summary,
                    ids: vec![id],
                    conversation_id: summary.conversation_id.map(|id| id.to_string()),
                    extra: wire::Extra::default(),
                },
            )
        })
        .unzip()
}

fn remember(
    conn: &Connection,
    summary: &WikiSummary,
    routing: &WikiRouting,
    item: &FiledPatch,
    done: &AppliedPage,
    now: Timestamp,
) -> Result<(), WikiStoreError> {
    let at = time::stamp(now);
    let aliases: Vec<String> = done
        .page
        .meta
        .aliases
        .iter()
        .filter(|alias| alias.as_str() != item.slug)
        .cloned()
        .collect();
    crate::wiki_topics::redirects(conn, &item.slug, &aliases, &at)?;
    for cited in attributed(
        &item.wiki,
        routing.topic_page.as_ref(),
        summary,
        &routing.also,
    ) {
        crate::wiki_topics::provenance(conn, &item.slug, cited, item.note.as_deref(), &at)?;
    }
    Ok(())
}

pub(crate) fn upsert(
    conn: &Connection,
    page: &WikiPage,
    sha: &str,
    now: Timestamp,
) -> Result<(), WikiStoreError> {
    let at = time::stamp(now);
    let history = i64::try_from(page.history.len())
        .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
    conn.execute(
        "INSERT INTO ros_wiki_topics (
            slug, title, aliases, tags, entities, related, current_state, article, search_text,
            history_count, git_sha, last_verified_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?12)
         ON CONFLICT(slug) DO UPDATE SET title = excluded.title, aliases = excluded.aliases,
            tags = excluded.tags, entities = excluded.entities, related = excluded.related,
            current_state = excluded.current_state, article = excluded.article,
            search_text = excluded.search_text, history_count = excluded.history_count,
            git_sha = excluded.git_sha, last_verified_at = excluded.last_verified_at,
            updated_at = excluded.updated_at, embedding = NULL, embed_status = NULL,
            embed_failures = 0, embed_error = NULL",
        params![
            page.meta.slug.as_ref(),
            page.meta.title,
            json(&page.meta.aliases)?,
            json(&page.meta.tags)?,
            json(&page.meta.entities)?,
            json(&page.meta.related)?,
            page.current_state,
            page.article,
            build_wiki_search_text(page),
            history,
            sha,
            at
        ],
    )
    .map_err(query)?;
    Ok(())
}

pub(crate) async fn rewrite(
    locked: &mut LockedRepository,
    page: &TopicPage,
    now: Timestamp,
) -> Result<(WikiPage, String), WikiStoreError> {
    let mut stored = match locked
        .read(page.slug.as_ref())
        .await
        .map_err(WikiStoreError::from)?
    {
        Some(stored) => stored,
        None => WikiPage::empty(page.slug.clone(), page.title.clone()),
    };
    copy_topic(&mut stored, page);
    let sha = locked
        .fold(&mut stored, &[], &sync_message(&page.slug), now)
        .await
        .map_err(WikiStoreError::from)?;
    Ok((stored, sha))
}

pub(crate) async fn create(
    locked: &mut LockedRepository,
    page: &TopicPage,
    now: Timestamp,
) -> Result<Option<(WikiPage, String)>, WikiStoreError> {
    let mut stored = WikiPage::empty(page.slug.clone(), page.title.clone());
    copy_topic(&mut stored, page);
    let sha = locked
        .create(&mut stored, &sync_message(&page.slug), now)
        .await
        .map_err(WikiStoreError::from)?;
    Ok(sha.map(|sha| (stored, sha)))
}

fn sync_message(slug: &WikiSlug) -> String {
    format!(
        "wiki({}): sync topic page\n\nPipeline: wiki-v7",
        slug.as_ref()
    )
}

pub(crate) async fn revise(
    locked: &mut LockedRepository,
    slug: &WikiSlug,
    edit: &PageEdit,
    now: Timestamp,
) -> Result<(WikiPage, String), WikiStoreError> {
    let mut stored = locked
        .read(slug.as_ref())
        .await
        .map_err(WikiStoreError::from)?
        .ok_or_else(|| WikiStoreError::TopicMissing(slug.as_ref().to_owned()))?;
    edit.apply_to(&mut stored, now);
    let sha = locked
        .fold(&mut stored, &[], &PageEdit::commit_message(slug), now)
        .await
        .map_err(WikiStoreError::from)?;
    Ok((stored, sha))
}

pub(crate) async fn remove_topic(slug: &str, root: &Path) -> Result<(), WikiStoreError> {
    let mut locked = Files::wiki(root).lock().await?;
    let present = locked.read(slug).await.map_err(WikiStoreError::from);
    let removed = match present {
        Ok(None) => Ok(()),
        Ok(Some(_)) => {
            let message = format!("wiki({slug}): removed after a tidy review");
            locked
                .remove(&[slug.to_owned()], &message)
                .await
                .map(|_| ())
                .map_err(WikiStoreError::from)
        }
        Err(error) => Err(error),
    };
    crate::page_files::finish(locked, removed).await
}

pub(crate) async fn read_topic(
    root: &Path,
    slug: &WikiSlug,
) -> Result<Option<TopicPage>, WikiStoreError> {
    let page = {
        let locked = Files::wiki(root).lock().await?;
        let read = locked
            .read(slug.as_ref())
            .await
            .map_err(WikiStoreError::from);
        crate::page_files::finish(locked, read).await?
    };
    match page {
        Some(page) => to_topic(page, slug).map(Some),
        None => Ok(None),
    }
}

fn copy_topic(stored: &mut WikiPage, page: &TopicPage) {
    page.title.clone_into(&mut stored.meta.title);
    page.summary.clone_into(&mut stored.current_state);
    page.article.clone_into(&mut stored.article);
    stored.meta.sections = page
        .sections
        .iter()
        .filter_map(|section| {
            section.topic.map(|topic| WikiSectionRef {
                topic: topic.to_string(),
                heading: section.heading.clone(),
            })
        })
        .collect();
    stored.extra_sections = page
        .sections
        .iter()
        .map(|section| ArticleSection {
            heading: section.heading.clone(),
            body: section.body.clone(),
        })
        .collect();
}

pub(crate) fn to_topic(page: WikiPage, slug: &WikiSlug) -> Result<TopicPage, WikiStoreError> {
    let refs = page.meta.sections;
    let sections = page
        .extra_sections
        .into_iter()
        .map(|section| section_of(&refs, section))
        .collect::<Result<_, WikiStoreError>>()?;
    Ok(TopicPage {
        slug: slug.clone(),
        title: page.meta.title,
        summary: page.current_state,
        article: page.article,
        sections,
    })
}

fn section_of(
    refs: &[WikiSectionRef],
    section: ArticleSection,
) -> Result<PageSection, WikiStoreError> {
    let topic = refs
        .iter()
        .find(|known| known.heading == section.heading)
        .map(|known| known.topic.parse::<TopicId>())
        .transpose()
        .map_err(|source| WikiStoreError::Query(Box::new(source)))?;
    Ok(PageSection {
        topic,
        heading: section.heading,
        body: section.body,
    })
}

fn exists(conn: &Connection, slug: &str) -> Result<bool, WikiStoreError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ros_wiki_topics WHERE slug = ?1)",
        params![slug],
        |row| row.get(0),
    )
    .map_err(query)
}

fn json(values: &[String]) -> Result<String, WikiStoreError> {
    serde_json::to_string(values).map_err(|source| WikiStoreError::Query(Box::new(source)))
}
