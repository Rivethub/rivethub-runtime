pub(super) struct Kept {
    pub(super) text: String,
    pub(super) source_chars: usize,
    pub(super) excerpted: bool,
}

pub(super) struct Unit {
    pub(super) text: String,
    pub(super) chars: usize,
    pub(super) messages: u64,
    pub(super) excerpt_chars: u64,
    pub(super) source_chars: u64,
}

pub(super) struct Tally {
    pub(super) sent: u64,
    pub(super) messages: u64,
    pub(super) excerpt_chars: u64,
    pub(super) source_chars: u64,
}

pub(super) struct Piece {
    pub(super) line: String,
    pub(super) start: usize,
    pub(super) end: usize,
}
