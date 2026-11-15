use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::memory::MemoryHitSource;
use regex::Regex;

pub const HYBRID_RRF_K: f64 = 20.0;
const SUMMARY_FUSION_BONUS: f64 = 1.3;
const GATE_FRACTION: f64 = 0.5;
const HYBRID_POOL_MIN: i64 = 50;
const HYBRID_POOL_MAX: i64 = 100;
pub const HYBRID_MIN_CONTENT_LEN: i64 = 40;

pub trait FusionItem {
    fn fusion_id(&self) -> &str;
    fn fusion_source(&self) -> MemoryHitSource;
    fn fusion_boost(&self) -> f64;
}

#[derive(Debug, Clone, Copy)]
pub struct Fused<'a, T> {
    pub item: &'a T,
    pub rrf: f64,
    pub arm_count: u32,
}

#[must_use]
pub fn hybrid_pool(limit: u32) -> i64 {
    (i64::from(limit) * 3).clamp(HYBRID_POOL_MIN, HYBRID_POOL_MAX)
}

#[expect(clippy::expect_used)]
#[must_use]
pub fn literal_scan(text: &str) -> bool {
    static LITERAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\w[./:_@]\w|\d{1,3}(?:\.\d{1,3}){2,}|[a-z]\d|\d[a-z]")
            .expect("the literal-scan pattern is a constant")
    });
    LITERAL.is_match(text)
}

#[expect(clippy::expect_used)]
#[must_use]
pub fn should_trigram_fallback(text: &str) -> bool {
    static FALLBACK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\S[./:-]\S").expect("the trigram-fallback pattern is a constant")
    });
    literal_scan(text) || FALLBACK.is_match(text)
}

#[must_use]
pub fn reciprocal_rank_fusion<'a, T: FusionItem>(arms: &[&'a [T]], k: f64) -> Vec<Fused<'a, T>> {
    let mut index: BTreeMap<&'a str, usize> = BTreeMap::new();
    let mut fused: Vec<Fused<'a, T>> = Vec::new();
    for arm in arms {
        for (rank, item) in (0u32..).zip(arm.iter()) {
            let contribution = 1.0 / (k + f64::from(rank) + 1.0);
            if let Some(slot) = index
                .get(item.fusion_id())
                .and_then(|&at| fused.get_mut(at))
            {
                slot.rrf += contribution;
                slot.arm_count += 1;
            } else {
                index.insert(item.fusion_id(), fused.len());
                fused.push(Fused {
                    item,
                    rrf: contribution,
                    arm_count: 1,
                });
            }
        }
    }
    fused
}

#[must_use]
pub fn fusion_score<T: FusionItem + ?Sized>(rrf: f64, item: &T) -> f64 {
    let bonus = if item.fusion_source() == MemoryHitSource::Summary {
        SUMMARY_FUSION_BONUS
    } else {
        1.0
    };
    rrf * (1.0 + item.fusion_boost()) * bonus
}

#[must_use]
pub fn passes_fusion_gate(arm_count: u32, score: f64, top: f64) -> bool {
    arm_count >= 2 || score >= top * GATE_FRACTION
}

#[cfg(test)]
mod tests {
    use super::FusionItem;
    use super::HYBRID_RRF_K;
    use super::fusion_score;
    use super::hybrid_pool;
    use super::literal_scan;
    use super::passes_fusion_gate;
    use super::reciprocal_rank_fusion;
    use super::should_trigram_fallback;
    use crate::memory::MemoryHitSource;

    const TOLERANCE: f64 = 1e-12;

    #[derive(Debug)]
    struct Hit {
        id: &'static str,
        source: MemoryHitSource,
        boost: f64,
    }

    impl FusionItem for Hit {
        fn fusion_id(&self) -> &str {
            self.id
        }

        fn fusion_source(&self) -> MemoryHitSource {
            self.source
        }

        fn fusion_boost(&self) -> f64 {
            self.boost
        }
    }

    const fn message(id: &'static str) -> Hit {
        Hit {
            id,
            source: MemoryHitSource::Message,
            boost: 0.0,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= TOLERANCE,
            "{actual} is not within {TOLERANCE} of {expected}"
        );
    }

    fn rrf_of(fused: &[super::Fused<'_, Hit>], id: &str) -> f64 {
        fused
            .iter()
            .find(|entry| entry.item.id == id)
            .expect("the id is fused")
            .rrf
    }

    fn ranked(fused: &[super::Fused<'_, Hit>]) -> Vec<(&'static str, u32)> {
        fused
            .iter()
            .map(|entry| (entry.item.id, entry.arm_count))
            .collect()
    }

    #[test]
    fn each_rank_contributes_the_reciprocal_of_k_plus_its_position() {
        let arm = [message("a"), message("b"), message("c")];
        let fused = reciprocal_rank_fusion(&[&arm], 20.0);
        assert_eq!(ranked(&fused), [("a", 1), ("b", 1), ("c", 1)]);
        assert_close(rrf_of(&fused, "a"), 1.0 / 21.0);
        assert_close(rrf_of(&fused, "b"), 1.0 / 22.0);
        assert_close(rrf_of(&fused, "c"), 1.0 / 23.0);
    }

    #[test]
    fn a_row_found_by_several_arms_sums_its_contributions() {
        let lexical = [message("a"), message("b")];
        let semantic = [message("b"), message("c")];
        let fused = reciprocal_rank_fusion(&[&lexical, &semantic], HYBRID_RRF_K);
        assert_eq!(ranked(&fused), [("a", 1), ("b", 2), ("c", 1)]);
        assert_close(rrf_of(&fused, "b"), 1.0 / 22.0 + 1.0 / 21.0);
        assert_close(rrf_of(&fused, "a"), 1.0 / 21.0);
        assert_close(rrf_of(&fused, "c"), 1.0 / 22.0);
    }

    #[test]
    fn agreement_between_arms_outranks_a_single_first_place() {
        let lexical = [message("solo"), message("shared")];
        let semantic = [message("shared")];
        let trigram = [message("shared")];
        let fused = reciprocal_rank_fusion(&[&lexical, &semantic, &trigram], HYBRID_RRF_K);
        assert!(rrf_of(&fused, "shared") > rrf_of(&fused, "solo"));
    }

    #[test]
    fn the_fused_row_is_the_first_arm_s_copy() {
        let lexical = [Hit {
            id: "a",
            source: MemoryHitSource::Message,
            boost: 0.1,
        }];
        let semantic = [Hit {
            id: "a",
            source: MemoryHitSource::Message,
            boost: 0.9,
        }];
        let fused = reciprocal_rank_fusion(&[&lexical, &semantic], HYBRID_RRF_K);
        assert_eq!(fused.len(), 1);
        assert_close(fused.first().expect("one fused row").item.boost, 0.1);
    }

    #[test]
    fn no_arms_fuse_to_nothing() {
        let empty: [Hit; 0] = [];
        assert!(reciprocal_rank_fusion::<Hit>(&[], HYBRID_RRF_K).is_empty());
        assert!(reciprocal_rank_fusion(&[&empty, &empty], HYBRID_RRF_K).is_empty());
    }

    #[test]
    fn the_boost_scales_the_fused_score() {
        let plain = message("a");
        let boosted = Hit {
            id: "b",
            source: MemoryHitSource::Message,
            boost: 0.5,
        };
        assert_close(fusion_score(0.04, &plain), 0.04);
        assert_close(fusion_score(0.04, &boosted), 0.06);
    }

    #[test]
    fn summaries_earn_the_fusion_bonus() {
        let summary = Hit {
            id: "s",
            source: MemoryHitSource::Summary,
            boost: 0.5,
        };
        assert_close(fusion_score(0.04, &summary), 0.078);
        assert!(fusion_score(0.04, &summary) > fusion_score(0.04, &message("m")));
    }

    #[test]
    fn the_gate_keeps_rows_found_by_two_arms_whatever_their_score() {
        assert!(passes_fusion_gate(2, 0.0, 1.0));
        assert!(passes_fusion_gate(3, 0.01, 1.0));
    }

    #[test]
    fn the_gate_keeps_single_arm_rows_within_half_the_top_score() {
        assert!(passes_fusion_gate(1, 1.0, 1.0));
        assert!(passes_fusion_gate(1, 0.5, 1.0));
        assert!(!passes_fusion_gate(1, 0.499, 1.0));
        assert!(!passes_fusion_gate(0, 0.1, 1.0));
    }

    #[test]
    fn the_pool_is_three_times_the_limit_within_fifty_and_a_hundred() {
        assert_eq!(hybrid_pool(0), 50);
        assert_eq!(hybrid_pool(1), 50);
        assert_eq!(hybrid_pool(16), 50);
        assert_eq!(hybrid_pool(17), 51);
        assert_eq!(hybrid_pool(30), 90);
        assert_eq!(hybrid_pool(33), 99);
        assert_eq!(hybrid_pool(34), 100);
        assert_eq!(hybrid_pool(u32::MAX), 100);
    }

    #[test]
    fn literal_scan_finds_paths_addresses_and_mixed_tokens() {
        for literal in [
            "crates/kernel",
            "main.rs",
            "rivet@host",
            "host:5441",
            "snake_case",
            "203.0.113.12",
            "v2",
            "4k",
            "see 198.51.100 for details",
        ] {
            assert!(literal_scan(literal), "{literal}");
        }
    }

    #[test]
    fn literal_scan_skips_plain_prose() {
        for prose in [
            "what did we decide",
            "Deploy Tonight",
            "",
            "1 . 2",
            "a / b",
            "12 34",
        ] {
            assert!(!literal_scan(prose), "{prose}");
        }
    }

    #[test]
    fn trigram_fallback_also_takes_punctuated_tokens() {
        assert!(should_trigram_fallback("well-known"));
        assert!(should_trigram_fallback("A/B"));
        assert!(should_trigram_fallback("crates/kernel"));
        assert!(!should_trigram_fallback("what did we decide"));
        assert!(!should_trigram_fallback("a - b"));
    }
}
