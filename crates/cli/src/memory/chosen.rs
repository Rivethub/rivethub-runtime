use std::path::PathBuf;
use std::sync::Arc;

use capture::share::Chosen;
use chrono::Utc;
use config::Environment;
use kernel::memory::share::Choice;
use kernel::memory::share::SessionChoices;
use kernel::memory::share::SessionRedaction;
use kernel::memory::share::Term;
use kernel::owner::Owner;
use memory_postgres::PostgresMemory;
use uuid::Uuid;

use super::SessionStore;
use super::parse::nonempty;
use super::run::open_sqlite;
use super::target::Kind;
use super::target::Target;
use super::target::resolve;
use crate::Error;
use crate::argv::Scan;
use crate::argv::Taken;

const SESSION: &str = "--session";
const REDACT_TERM: &str = "--redact-term";
const DROP_TOOL_OUTPUTS: &str = "--drop-tool-outputs";
const SQLITE: &str = "--sqlite";

#[derive(Debug, Default)]
pub(super) struct Options {
    sessions: Vec<Uuid>,
    terms: Vec<(Uuid, String)>,
    drops: Vec<Uuid>,
    sqlite: Option<PathBuf>,
}

#[derive(Debug)]
pub(super) struct Store {
    choices: SessionChoices,
    sqlite: Option<PathBuf>,
}

impl Options {
    pub(super) fn take(&mut self, scan: &mut Scan<'_>) -> Result<bool, Error> {
        if let Some(value) = value(scan, SESSION)? {
            self.sessions.push(id(&value)?);
            return Ok(true);
        }
        if let Some(value) = value(scan, REDACT_TERM)? {
            let (session, term) = value
                .split_once('=')
                .ok_or_else(|| Error::RedactTermForm(value.clone()))?;
            self.terms.push((id(session)?, term.to_owned()));
            return Ok(true);
        }
        if let Some(value) = value(scan, DROP_TOOL_OUTPUTS)? {
            self.drops.push(id(&value)?);
            return Ok(true);
        }
        if let Some(value) = value(scan, SQLITE)? {
            self.sqlite = Some(nonempty(&value, Error::SqliteRequired)?);
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
            && self.terms.is_empty()
            && self.drops.is_empty()
            && self.sqlite.is_none()
    }

    pub(super) fn store(self) -> Result<Option<Store>, Error> {
        let unchosen = |flag: &'static str, id: &Uuid| {
            (!self.sessions.contains(id)).then(|| Error::ChoiceWithoutSession {
                flag,
                id: id.to_string(),
            })
        };
        if let Some(error) = self
            .terms
            .iter()
            .find_map(|(id, _)| unchosen(REDACT_TERM, id))
            .or_else(|| {
                self.drops
                    .iter()
                    .find_map(|id| unchosen(DROP_TOOL_OUTPUTS, id))
            })
        {
            return Err(error);
        }
        if self.sessions.is_empty() {
            return match self.sqlite {
                Some(_) => Err(Error::SqliteWithoutSession),
                None => Ok(None),
            };
        }
        let chosen = self
            .sessions
            .iter()
            .map(|session| {
                let terms = self
                    .terms
                    .iter()
                    .filter(|(id, _)| id == session)
                    .map(|(_, term)| Term::try_from(term.clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                let drop = self.drops.contains(session);
                let choice = if terms.is_empty() && !drop {
                    Choice::Approve
                } else {
                    Choice::Redact(SessionRedaction::new(terms, drop)?)
                };
                Ok((*session, choice))
            })
            .collect::<Result<Vec<_>, kernel::memory::share::ChoiceError>>()?;
        Ok(Some(Store {
            choices: SessionChoices::try_from(chosen)?,
            sqlite: self.sqlite,
        }))
    }
}

fn value(scan: &mut Scan<'_>, name: &'static str) -> Result<Option<String>, Error> {
    match scan.option(name, None) {
        Taken::Value(value) if !value.trim().is_empty() => Ok(Some(value)),
        Taken::Value(_) | Taken::Missing => Err(Error::OptionValue(name)),
        Taken::Absent => Ok(None),
    }
}

fn id(text: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(text.trim()).map_err(|_| Error::ConversationId(text.to_owned()))
}

pub(super) async fn load(store: Store, env: &Environment) -> Result<Chosen, Error> {
    let target = resolve(env, store.sqlite, Kind::Either)?;
    let author = super::bundle::exporter(env).await?;
    let owner = Owner::new(author.user.clone())?;
    let sessions = match target {
        Target::Postgres(url) => {
            SessionStore::Postgres(Arc::new(PostgresMemory::connect(&url).await?))
        }
        Target::Sqlite(path) => SessionStore::Sqlite(open_sqlite(&path).await?),
    };
    let chosen = sessions.chosen(&author, &owner, &store.choices).await?;
    Ok(capture::share::chosen(author, chosen, Utc::now())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Option<Store>, Error> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let mut scan = Scan::new(&args);
        let mut options = Options::default();
        while scan.current().is_some() {
            assert!(options.take(&mut scan)?, "unexpected {:?}", scan.current());
        }
        options.store()
    }

    const A: &str = "00000000-0000-4000-8000-00000000000a";
    const B: &str = "00000000-0000-4000-8000-00000000000b";

    #[test]
    fn each_session_is_approved_unless_a_rule_names_it() -> Result<(), Error> {
        let store = parsed(&[
            SESSION,
            A,
            SESSION,
            B,
            REDACT_TERM,
            &format!("{B}=Falcon Labs"),
            DROP_TOOL_OUTPUTS,
            B,
        ])?
        .ok_or(Error::OutRequired)?;
        let falcon = Term::try_from("Falcon Labs".to_owned())?;
        assert_eq!(
            store.choices.as_slice(),
            [
                (id(A)?, Choice::Approve),
                (
                    id(B)?,
                    Choice::Redact(SessionRedaction::new(vec![falcon], true)?)
                ),
            ]
        );
        Ok(())
    }

    #[test]
    fn rules_must_name_a_chosen_session() {
        assert!(matches!(
            parsed(&[SESSION, A, REDACT_TERM, &format!("{B}=x")]),
            Err(Error::ChoiceWithoutSession { flag: REDACT_TERM, id }) if id == B
        ));
        assert!(matches!(
            parsed(&[DROP_TOOL_OUTPUTS, A]),
            Err(Error::ChoiceWithoutSession { flag: DROP_TOOL_OUTPUTS, id }) if id == A
        ));
        assert!(matches!(
            parsed(&[SQLITE, "m.sqlite"]),
            Err(Error::SqliteWithoutSession)
        ));
    }

    #[test]
    fn malformed_choices_are_refused() {
        assert!(matches!(
            parsed(&[SESSION, "c1"]),
            Err(Error::ConversationId(text)) if text == "c1"
        ));
        assert!(matches!(
            parsed(&[SESSION, A, REDACT_TERM, A]),
            Err(Error::RedactTermForm(_))
        ));
        assert!(matches!(
            parsed(&[SESSION, A, REDACT_TERM, &format!("{A}=  ")]),
            Err(Error::Choice(kernel::memory::share::ChoiceError::BlankTerm))
        ));
        assert!(matches!(
            parsed(&[SESSION, A, SESSION, A]),
            Err(Error::Choice(
                kernel::memory::share::ChoiceError::Duplicate(_)
            ))
        ));
        assert!(matches!(
            parsed(&[SESSION]),
            Err(Error::OptionValue(SESSION))
        ));
    }
}
