use std::collections::{HashMap, HashSet};

type Trigram = [u8; 3];

pub(super) fn score(needle: &str, haystack: &str) -> f32 {
    let needle = trigrams(needle).into_iter().collect::<HashSet<_>>();
    let haystack = trigrams(haystack);
    if needle.is_empty() || haystack.is_empty() {
        return 0.0;
    }
    let unique = haystack.iter().copied().collect::<HashSet<_>>();
    let full = similarity(
        needle.intersection(&unique).count(),
        needle.len(),
        unique.len(),
    );
    full.max(word_similarity(&needle, &haystack))
}

fn trigrams(text: &str) -> Vec<Trigram> {
    let text = text.to_lowercase();
    let mut result = Vec::new();
    for word in text
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        let mut leading = ' ';
        let mut middle = ' ';
        for trailing in word.chars().chain(std::iter::once(' ')) {
            result.push(compact([leading, middle, trailing]));
            (leading, middle) = (middle, trailing);
        }
    }
    result
}

fn compact(characters: [char; 3]) -> Trigram {
    if characters.iter().all(char::is_ascii) {
        return characters.map(|ch| ch as u8);
    }
    let mut crc = u32::MAX;
    for ch in characters {
        let mut utf8 = [0; 4];
        for byte in ch.encode_utf8(&mut utf8).bytes() {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(crc >> 31);
                crc = (crc << 1) ^ (mask & 0x04c1_1db7);
            }
        }
    }
    let [first, second, third, _] = (!crc).to_ne_bytes();
    [first, second, third]
}

fn similarity(common: usize, needle: usize, extent: usize) -> f32 {
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
    let mut score = similarity(extent.common, needle.len(), extent.unique);
    for (index, gram) in haystack
        .iter()
        .enumerate()
        .take(upper + 1)
        .skip(extent.lower)
    {
        let next = similarity(extent.common, needle.len(), extent.unique);
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
