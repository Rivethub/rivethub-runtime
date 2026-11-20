use thiserror::Error;

pub(crate) const PART_PARALLELISM: usize = 3;
pub(crate) const FALLBACK_CONTEXT_TOKENS: u32 = 131_072;
pub(crate) const CHUNK_TOKENS: u32 = 90_000;
pub(crate) const PART_CEILING: u32 = 3_000;
pub(crate) const SESSION_CEILING: u32 = 20_000;
pub(crate) const CARRY_CEILING: u32 = 10_000;
pub(crate) const TARGET_DIVISOR: usize = 10;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    context_tokens: u32,
    chunk_tokens: u32,
    thinking: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid top-down context or chunk budget")]
pub(crate) struct InvalidLimits;

impl Limits {
    pub(crate) fn new(
        context_tokens: u32,
        chunk_tokens: u32,
        thinking: bool,
    ) -> Result<Self, InvalidLimits> {
        if context_tokens <= request_max_tokens(PART_CEILING, thinking)
            || chunk_tokens == 0
            || chunk_tokens >= context_tokens
        {
            return Err(InvalidLimits);
        }
        Ok(Self {
            context_tokens,
            chunk_tokens,
            thinking,
        })
    }

    pub(crate) const fn context_tokens(self) -> usize {
        self.context_tokens as usize
    }

    pub(crate) const fn chunk_tokens(self) -> usize {
        self.chunk_tokens as usize
    }

    pub(crate) const fn thinking(self) -> bool {
        self.thinking
    }
}

pub(crate) const fn request_max_tokens(ceiling: u32, thinking: bool) -> u32 {
    ceiling + ceiling / 2 + 512 + if thinking { 8192 } else { 0 }
}
