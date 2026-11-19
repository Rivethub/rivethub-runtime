use percent_encoding::utf8_percent_encode;
use wire::session_id::SessionKey;

const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

#[derive(Debug, Clone)]
pub(crate) struct Pages {
    origin: String,
}

impl Pages {
    pub(crate) fn new(mut origin: url::Url) -> Self {
        origin.set_path("");
        origin.set_query(None);
        origin.set_fragment(None);
        Self {
            origin: origin.to_string().trim_end_matches('/').to_owned(),
        }
    }

    pub(crate) fn topic(&self, slug: &str) -> String {
        self.page("topics", slug)
    }

    pub(crate) fn session(&self, id: &str) -> Option<String> {
        if id.parse::<SessionKey>().is_ok() {
            Some(self.page("sessions", id))
        } else {
            None
        }
    }

    fn page(&self, kind: &str, id: &str) -> String {
        format!(
            "{}/{kind}/{}",
            self.origin,
            utf8_percent_encode(id, SEGMENT)
        )
    }
}

pub(crate) fn suffix(url: Option<String>) -> String {
    match url {
        Some(url) => format!("\n{url}"),
        None => String::new(),
    }
}
