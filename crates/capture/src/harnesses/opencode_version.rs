use super::version::Version;

pub(super) const FIRST_LOADING: Version = Version::new(1, 3, 4);
pub(super) const NEWEST_CHECKED: Version = Version::new(2, 3, 0);
pub(super) const CHECKED: &str = "1.3.4, 1.19.5, 2.0.0, 2.2.5 and 2.3.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fit {
    Loads(Version),
    TooOld(Version),
    Newer(Version),
    Unknown,
}

impl Fit {
    pub(super) fn of(version: Option<Version>) -> Self {
        match version {
            None => Self::Unknown,
            Some(version) if version < FIRST_LOADING => Self::TooOld(version),
            Some(version) if version.major() > NEWEST_CHECKED.major() => Self::Newer(version),
            Some(version) => Self::Loads(version),
        }
    }
}
