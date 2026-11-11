use std::fmt;
use std::str::FromStr;

use values::SessionId;

use crate::facts::FactId;
use crate::memory::TagId;
use crate::tidy::ProposalId;
use crate::topics::Slug;
use crate::topics::TopicId;

const NEW_TOPIC: &str = "new-";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FilingTarget {
    Topic(TopicId),
    New(Slug),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewKey {
    Conflict {
        fact: FactId,
        version: i64,
    },
    Fact {
        fact: FactId,
        version: i64,
    },
    Filing {
        session: SessionId,
        into: FilingTarget,
    },
    Tag(TagId),
    Tidy(ProposalId),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a review item id")]
pub struct UnknownReviewKey(pub String);

impl fmt::Display for FilingTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Topic(topic) => topic.fmt(f),
            Self::New(slug) => write!(f, "{NEW_TOPIC}{}", slug.as_str()),
        }
    }
}

impl fmt::Display for ReviewKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { fact, version } => write!(f, "conflict.{fact}.{version}"),
            Self::Fact { fact, version } => write!(f, "fact.{fact}.{version}"),
            Self::Filing { session, into } => write!(f, "filing.{}.{into}", session.as_str()),
            Self::Tag(tag) => write!(f, "tag.{tag}"),
            Self::Tidy(proposal) => write!(f, "tidy.{proposal}"),
        }
    }
}

impl FromStr for ReviewKey {
    type Err = UnknownReviewKey;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let unknown = || UnknownReviewKey(text.to_owned());
        let (group, rest) = text.split_once('.').ok_or_else(unknown)?;
        match group {
            "conflict" => {
                let (fact, version) = versioned(rest).map_err(|_| unknown())?;
                Ok(Self::Conflict { fact, version })
            }
            "fact" => {
                let (fact, version) = versioned(rest).map_err(|_| unknown())?;
                Ok(Self::Fact { fact, version })
            }
            "filing" => {
                let (session, into) = rest.rsplit_once('.').ok_or_else(unknown)?;
                Ok(Self::Filing {
                    session: SessionId::from_str(session).map_err(|_| unknown())?,
                    into: target(into).map_err(|_| unknown())?,
                })
            }
            "tag" => TagId::from_str(rest).map(Self::Tag).map_err(|_| unknown()),
            "tidy" => ProposalId::from_str(rest)
                .map(Self::Tidy)
                .map_err(|_| unknown()),
            _ => Err(unknown()),
        }
    }
}

fn versioned(rest: &str) -> Result<(FactId, i64), UnknownReviewKey> {
    let unknown = || UnknownReviewKey(rest.to_owned());
    let (fact, version) = rest.split_once('.').ok_or_else(unknown)?;
    Ok((
        FactId::from_str(fact).map_err(|_| unknown())?,
        version.parse().map_err(|_| unknown())?,
    ))
}

fn target(text: &str) -> Result<FilingTarget, UnknownReviewKey> {
    let unknown = || UnknownReviewKey(text.to_owned());
    match text.strip_prefix(NEW_TOPIC) {
        Some(slug) => {
            let folded = Slug::fold(slug).map_err(|_| unknown())?;
            if folded.as_str() == slug {
                Ok(FilingTarget::New(folded))
            } else {
                Err(unknown())
            }
        }
        None => TopicId::from_str(text)
            .map(FilingTarget::Topic)
            .map_err(|_| unknown()),
    }
}
