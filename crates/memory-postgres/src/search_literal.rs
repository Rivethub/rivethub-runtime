#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Patience {
    Asked,
    Optional,
}

impl Patience {
    pub(crate) const fn statement_timeout(self) -> &'static str {
        match self {
            Self::Asked => "30s",
            Self::Optional => "3s",
        }
    }
}
