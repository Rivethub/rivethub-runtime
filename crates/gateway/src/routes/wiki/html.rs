use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Write as _;
use wire::time::LegacyStrftime as _;
use wire::time::TimestampFormat as _;

use axum::http::StatusCode;
use axum::http::header;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use kernel::wiki::WikiFilter;
use kernel::wiki::page::WikiSlug;
use wire::gateway::wiki::WikiMissBody;
use wire::gateway::wiki::WikiPageResponse;
use wire::gateway::wiki::WikiTopicList;

use super::WikiError;
use super::page_url;

struct Escaped<'a>(&'a str);

impl Display for Escaped<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        for ch in self.0.chars() {
            match ch {
                '&' => formatter.write_str("&amp;")?,
                '<' => formatter.write_str("&lt;")?,
                '>' => formatter.write_str("&gt;")?,
                '"' => formatter.write_str("&quot;")?,
                '\'' => formatter.write_str("&#39;")?,
                other => formatter.write_char(other)?,
            }
        }
        Ok(())
    }
}

pub(super) fn response(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Html(body),
    )
        .into_response()
}

fn document(title: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{} — RivetHub Wiki</title></head><body><nav><a href=\"/wiki\">RivetHub Wiki</a></nav><main><h1>{}</h1>",
        Escaped(title),
        Escaped(title),
    )
}

fn link(output: &mut String, slug: &str, title: &str) -> Result<(), WikiError> {
    let slug = WikiSlug::try_from(slug.to_owned())?;
    write!(
        output,
        "<a href=\"{}\">{}</a>",
        Escaped(&page_url("/wiki/", &slug)?),
        Escaped(title)
    )?;
    Ok(())
}

pub(super) fn index(topics: &WikiTopicList, filter: &WikiFilter) -> Result<String, WikiError> {
    let mut output = document("Wiki");
    let query = match &filter.query {
        Some(query) => query.as_str(),
        None => "",
    };
    write!(
        output,
        "<form action=\"/wiki\" method=\"get\"><label>Search <input name=\"q\" value=\"{}\"></label><button type=\"submit\">Search</button></form><p>{} topics</p><ul>",
        Escaped(query),
        topics.total
    )?;
    for topic in &topics.topics {
        output.push_str("<li><h2>");
        link(&mut output, &topic.slug, &topic.title)?;
        let updated = topic.updated_at.rfc3339_millis();
        write!(
            output,
            "</h2><p>{}</p><time>{}</time></li>",
            Escaped(&topic.excerpt),
            Escaped(&updated)
        )?;
    }
    output.push_str("</ul>");
    if u64::from(filter.offset) + (topics.topics.len() as u64) < topics.total {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if let Some(text) = &filter.query {
            query.append_pair("q", text);
        }
        if let Some(tag) = &filter.tag {
            query.append_pair("tag", tag);
        }
        query.append_pair("limit", &filter.limit.to_string());
        query.append_pair(
            "offset",
            &(u64::from(filter.offset) + u64::from(filter.limit)).to_string(),
        );
        write!(
            output,
            "<a href=\"/wiki?{}\">Next page</a>",
            Escaped(&query.finish())
        )?;
    }
    output.push_str("</main></body></html>");
    Ok(output)
}

pub(super) fn article(page: &WikiPageResponse) -> Result<String, WikiError> {
    let mut output = document(&page.title);
    let updated = page.updated_at.rfc3339_millis();
    write!(
        output,
        "<p>Updated <time>{}</time></p><section><h2>Current state</h2><pre>{}</pre></section>",
        Escaped(&updated),
        Escaped(&page.current_state)
    )?;
    if let Some(article) = &page.article {
        write!(
            output,
            "<section><h2>Article</h2><pre>{}</pre></section>",
            Escaped(article)
        )?;
    }
    if !page.tags.is_empty() {
        output.push_str("<section><h2>Tags</h2><ul>");
        for tag in &page.tags {
            write!(output, "<li>{}</li>", Escaped(tag))?;
        }
        output.push_str("</ul></section>");
    }
    for related in [&page.related, &page.see_also].into_iter().flatten() {
        output.push_str("<section><h2>Related topics</h2><ul>");
        for slug in related {
            output.push_str("<li>");
            link(&mut output, slug, slug)?;
            output.push_str("</li>");
        }
        output.push_str("</ul></section>");
    }
    if !page.history.is_empty() {
        output.push_str("<section><h2>History</h2>");
        for entry in &page.history {
            let date = entry.date.legacy_strftime("%Y-%m-%d");
            write!(
                output,
                "<h3>{}</h3><time>{}</time><pre>{}</pre>",
                Escaped(&entry.title),
                Escaped(&date),
                Escaped(&entry.body)
            )?;
        }
        output.push_str("</section>");
    }
    if !page.citations.is_empty() {
        output.push_str("<section><h2>Citations</h2><ul>");
        for citation in &page.citations {
            write!(output, "<li>{}", Escaped(&citation.summary_id))?;
            if let Some(note) = &citation.note {
                write!(output, " — {}", Escaped(note))?;
            }
            output.push_str("</li>");
        }
        output.push_str("</ul></section>");
    }
    output.push_str("</main></body></html>");
    Ok(output)
}

pub(super) fn archive(slug: &str, markdown: &str) -> Result<String, WikiError> {
    let mut output = document(&format!("Archive: {slug}"));
    output.push_str("<p>Back to ");
    link(&mut output, slug, slug)?;
    output.push_str("</p><pre>");
    for line in markdown.lines() {
        match anchor(line) {
            Some(id) => write!(output, "</pre><span id=\"{id}\"></span><pre>")?,
            None => writeln!(output, "{}", Escaped(line))?,
        }
    }
    output.push_str("</pre></main></body></html>");
    Ok(output)
}

fn anchor(line: &str) -> Option<&str> {
    let id = line
        .trim()
        .strip_prefix("<a id=\"")?
        .strip_suffix("\"></a>")?;
    id.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        .then_some(id)
}

pub(super) fn missing(body: &WikiMissBody) -> Result<String, WikiError> {
    let mut output = document("Wiki page not found");
    output.push_str("<p>Try searching for another title.</p><ul>");
    for suggestion in &body.suggestions {
        output.push_str("<li>");
        link(&mut output, &suggestion.slug, &suggestion.title)?;
        output.push_str("</li>");
    }
    output.push_str("</ul></main></body></html>");
    Ok(output)
}
