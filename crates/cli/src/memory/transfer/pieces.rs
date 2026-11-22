use super::{Batch, Row};

const ARRAY_HEADER_BYTES: usize = 20;
const LENGTH_BYTES: usize = 4;

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub rows: usize,
    pub bytes: usize,
}

impl Limits {
    pub(super) const DEFAULT: Self = Self {
        rows: 5_000,
        bytes: 64 * 1024 * 1024,
    };

    pub(super) fn split(self, batch: &Batch) -> impl Iterator<Item = &[Row]> {
        let mut remaining = batch.rows.as_slice();
        let headers = batch.table.columns.len() * (ARRAY_HEADER_BYTES + LENGTH_BYTES);
        std::iter::from_fn(move || {
            let (first, rest) = remaining.split_first()?;
            let mut budget = self.bytes.saturating_sub(headers + row_bytes(first));
            let fitting = rest
                .iter()
                .take(self.rows.saturating_sub(1))
                .take_while(|row| {
                    let Some(left) = budget.checked_sub(row_bytes(row)) else {
                        return false;
                    };
                    budget = left;
                    true
                })
                .count();
            let (piece, rest) = remaining.split_at(1 + fitting);
            remaining = rest;
            Some(piece)
        })
    }
}

fn row_bytes(row: &Row) -> usize {
    row.iter()
        .map(|cell| LENGTH_BYTES + cell.as_ref().map_or(0, String::len))
        .sum()
}
