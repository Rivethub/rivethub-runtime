use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

type Trigram = [u8; 3];

static LIBC_C_UTF8_WORDS: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"[[\p{Alphabetic}\p{Nd}]&&\p{Age=14.0}--[\u{0363}-\u{036F}\u{0C04}\u{0F82}-\u{0F83}\u{1DD3}-\u{1DE6}\u{11080}-\u{11081}]]+",
    )
});

#[derive(Clone, Copy)]
pub(crate) struct Scorer(&'static Regex);

impl Scorer {
    pub(crate) fn new() -> Result<Self, regex::Error> {
        LIBC_C_UTF8_WORDS.as_ref().map(Self).map_err(Clone::clone)
    }

    pub(crate) fn score(self, needle: &str, haystack: &str) -> f32 {
        let needle = self.unique(needle);
        let haystack = self.trigrams(haystack);
        let full = set_similarity(&needle, &haystack.iter().copied().collect());
        full.max(word_similarity(&needle, &haystack))
    }

    pub(crate) fn similarity(self, left: &str, right: &str) -> f32 {
        set_similarity(&self.unique(left), &self.unique(right))
    }

    fn unique(self, text: &str) -> HashSet<Trigram> {
        self.trigrams(text).into_iter().collect()
    }

    fn trigrams(self, text: &str) -> Vec<Trigram> {
        let mut result = Vec::new();
        for word in self.0.find_iter(text) {
            let mut leading = ' ';
            let mut middle = ' ';
            let lowered = word
                .as_str()
                .chars()
                .filter_map(|ch| ch.to_lowercase().next());
            for trailing in lowered.chain(std::iter::once(' ')) {
                result.push(compact([leading, middle, trailing]));
                (leading, middle) = (middle, trailing);
            }
        }
        result
    }
}

fn set_similarity(left: &HashSet<Trigram>, right: &HashSet<Trigram>) -> f32 {
    ratio(left.intersection(right).count(), left.len(), right.len())
}

fn compact(characters: [char; 3]) -> Trigram {
    if characters.iter().all(char::is_ascii) {
        return characters.map(|ch| ch as u8);
    }
    let mut crc = u32::MAX;
    for ch in characters {
        let mut utf8 = [0; 4];
        for byte in ch.encode_utf8(&mut utf8).bytes() {
            let mut entry = (crc >> 24) ^ u32::from(byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(entry & 1);
                entry = (entry >> 1) ^ (mask & 0xedb8_8320);
            }
            crc = (crc << 8) ^ entry;
        }
    }
    let [first, second, third, _] = (!crc).to_ne_bytes();
    [first, second, third]
}

fn ratio(common: usize, needle: usize, extent: usize) -> f32 {
    if common == 0 {
        return 0.0;
    }
    #[expect(clippy::cast_precision_loss)]
    let ratio = common as f32 / (needle + extent - common) as f32;
    ratio
}

#[derive(Clone, Copy)]
struct Extent {
    lower: usize,
    unique: usize,
    common: usize,
}

fn word_similarity(needle: &HashSet<Trigram>, haystack: &[Trigram]) -> f32 {
    let mut last = HashMap::new();
    let mut active: Option<Extent> = None;
    let mut best = 0.0_f32;
    for (i, gram) in haystack.iter().enumerate() {
        let common = needle.contains(gram);
        let Some(mut extent) = active.or_else(|| {
            common.then_some(Extent {
                lower: i,
                unique: 0,
                common: 0,
            })
        }) else {
            continue;
        };
        if last.insert(*gram, i).is_none() {
            extent.unique += 1;
            extent.common += usize::from(common);
        }
        if common {
            let (next, score) = best_extent(needle, haystack, &last, extent, i);
            for item in haystack
                .iter()
                .enumerate()
                .skip(extent.lower)
                .take(next.lower - extent.lower)
            {
                let (index, discarded) = item;
                if last.get(discarded) == Some(&index) {
                    last.remove(discarded);
                }
            }
            extent = next;
            best = best.max(score);
        }
        active = Some(extent);
    }
    best
}

fn best_extent(
    needle: &HashSet<Trigram>,
    haystack: &[Trigram],
    last: &HashMap<Trigram, usize>,
    mut extent: Extent,
    upper: usize,
) -> (Extent, f32) {
    let mut best = extent;
    let mut score = ratio(extent.common, needle.len(), extent.unique);
    for (index, gram) in haystack
        .iter()
        .enumerate()
        .take(upper + 1)
        .skip(extent.lower)
    {
        let next = ratio(extent.common, needle.len(), extent.unique);
        if next > score {
            extent.lower = index;
            best = extent;
            score = next;
        }
        if last.get(gram) == Some(&index) {
            extent.unique -= 1;
            extent.common -= usize::from(needle.contains(gram));
        }
    }
    (best, score)
}

#[cfg(test)]
mod parity;
