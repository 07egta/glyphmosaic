use super::ivf::IVF_PROBE;
use super::simd::{coarse_score, coarse_score64, full_score_bounded};
use super::{MatchResult, Query};
use crate::dict::Dictionary;
use crate::raster::coarse8_from_full;

/// Number of coarse candidates kept before the 8×8 rescore.
const COARSE_CANDIDATES: usize = 32_768;

pub(crate) fn truncate_top(scored: &mut Vec<(u32, usize)>, top_k: usize) {
    let k = top_k.max(1).min(scored.len());
    if k < scored.len() {
        scored.select_nth_unstable_by(k - 1, |a, b| a.cmp(b));
        scored.truncate(k);
    }
    scored.sort_unstable();
}

/// Return the `top_k` best `(mse, index)` candidates by coarse score.
pub fn scan_coarse(dict: &Dictionary, q: &[u8; 16], top_k: usize) -> Vec<(u32, usize)> {
    if dict.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u32, usize)> = dict
        .coarse
        .iter()
        .enumerate()
        .map(|(index, glyph)| (coarse_score(q, glyph), index))
        .collect();
    truncate_top(&mut scored, top_k);
    scored
}

/// Rescore coarse candidates at 8×8 and keep the best `top_k`.
pub fn refine_mid(
    dict: &Dictionary,
    q: &[u8; 64],
    candidates: &[(u32, usize)],
    top_k: usize,
) -> Vec<(u32, usize)> {
    let mut scored: Vec<(u32, usize)> = candidates
        .iter()
        .filter(|&&(_, index)| index < dict.len())
        .map(|&(_, index)| {
            let glyph8 = &dict.coarse8[index];
            (coarse_score64(q, glyph8), index)
        })
        .collect();
    truncate_top(&mut scored, top_k);
    scored
}

/// Same as [`refine_mid`], for candidates that come straight from the IVF index
/// as plain dictionary indices (no intermediate `(score, index)` list).
pub fn refine_mid_indices(
    dict: &Dictionary,
    q: &[u8; 64],
    candidates: &[u32],
    top_k: usize,
) -> Vec<(u32, usize)> {
    let mut scored: Vec<(u32, usize)> = candidates
        .iter()
        .map(|&i| i as usize)
        .filter(|&index| index < dict.len())
        .map(|index| (coarse_score64(q, &dict.coarse8[index]), index))
        .collect();
    truncate_top(&mut scored, top_k);
    scored
}

/// Rescore candidates at full resolution and return `(index, mse)`.
/// With an empty candidate list, scan the whole dictionary.
///
/// The comparison is on `(score, index)`, so ties go to the lowest index no
/// matter in which order candidates arrive. A candidate is abandoned part-way
/// as soon as it is strictly worse than the best so far, which never changes
/// the winner.
pub fn refine_full(dict: &Dictionary, q: &[u8; 256], candidates: &[(u32, usize)]) -> (usize, u32) {
    if dict.is_empty() {
        return (0, u32::MAX);
    }
    let mut best = (u32::MAX, usize::MAX);
    if candidates.is_empty() {
        for (index, glyph) in dict.full.iter().enumerate() {
            let score = full_score_bounded(q, glyph, best.0);
            if (score, index) < best {
                best = (score, index);
            }
        }
    } else {
        for &(_, index) in candidates {
            if index >= dict.len() {
                continue;
            }
            let score = full_score_bounded(q, &dict.full[index], best.0);
            if (score, index) < best {
                best = (score, index);
            }
        }
    }
    if best.1 == usize::MAX {
        (0, u32::MAX)
    } else {
        (best.1, best.0)
    }
}

pub fn match_one(dict: &Dictionary, q: &Query, top_k: usize, exact: bool) -> MatchResult {
    if dict.is_empty() {
        return MatchResult {
            index: 0,
            mse: u32::MAX,
        };
    }
    let (index, mse) = if exact {
        refine_full(dict, &q.full, &[])
    } else {
        let q8 = coarse8_from_full(&q.full);
        let mid = match &dict.index {
            Some(index) if dict.len() >= 20_000 => {
                let candidates = index.candidates(&q8, IVF_PROBE);
                refine_mid_indices(dict, &q8, &candidates, top_k.max(1))
            }
            _ => {
                let coarse = scan_coarse(dict, &q.coarse, COARSE_CANDIDATES);
                refine_mid(dict, &q8, &coarse, top_k.max(1))
            }
        };
        refine_full(dict, &q.full, &mid)
    };
    MatchResult { index, mse }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::{DictHeader, Dictionary};

    fn dict_with(entries: usize) -> Dictionary {
        let header = DictHeader {
            version: 1,
            cell_feature: 16,
            coarse_feature: 4,
            unicode_version: "17.0".into(),
            fonts: vec![],
            ..DictHeader::default()
        };
        let mut d = Dictionary::new(header);
        for i in 0..entries {
            let mut full = [0u8; 256];
            let mut coarse = [0u8; 16];
            for (j, v) in full.iter_mut().enumerate() {
                *v = ((i * 37 + j * 11) % 256) as u8;
            }
            full[0] = (i & 0xFF) as u8;
            full[1] = ((i >> 8) & 0xFF) as u8;
            for j in 0..16 {
                coarse[j] = full[j * 16];
            }
            coarse[0] = full[0];
            coarse[1] = full[1];
            d.push(
                char::from_u32(0x41 + (i as u32 % 26)).unwrap(),
                0,
                full,
                coarse,
            );
        }
        d
    }

    #[test]
    fn finds_planted_best_match() {
        let d = dict_with(500);
        let q = Query {
            full: d.full[42],
            coarse: d.coarse[42],
        };
        let r = match_one(&d, &q, 32, false);
        assert_eq!(r.index, 42);
        assert_eq!(r.mse, 0);
    }

    #[test]
    fn ties_pick_lowest_index() {
        let mut d = dict_with(2);
        d.full[1] = d.full[0];
        d.coarse[1] = d.coarse[0];
        let q = Query {
            full: d.full[0],
            coarse: d.coarse[0],
        };
        assert_eq!(match_one(&d, &q, 8, false).index, 0);
        assert_eq!(match_one(&d, &q, 8, true).index, 0);
    }

    #[test]
    fn exact_agrees_with_coarse_fine_on_planted_data() {
        let d = dict_with(300);
        for i in [0usize, 7, 99, 299] {
            let q = Query {
                full: d.full[i],
                coarse: d.coarse[i],
            };
            assert_eq!(match_one(&d, &q, 64, false).index, i);
        }
    }

    #[test]
    fn coarse8_is_block_average() {
        let mut full = [0u8; 256];
        for y in 0..16 {
            for x in 0..16 {
                full[y * 16 + x] = if x < 8 { 200 } else { 100 };
            }
        }
        let c = coarse8_from_full(&full);
        assert_eq!(c[0], 200);
        assert_eq!(c[7], 100);
        assert_eq!(c[8], 200);
    }

    #[test]
    fn truncate_top_keeps_best_and_breaks_ties() {
        let mut scored = vec![(5, 1), (5, 0), (1, 5), (1, 4), (9, 9)];
        truncate_top(&mut scored, 3);
        assert_eq!(scored, vec![(1, 4), (1, 5), (5, 0)]);
    }

    #[test]
    fn staged_match_survives_pruning() {
        let d = dict_with(40_000);
        let i = 35_000;
        let q = Query {
            full: d.full[i],
            coarse: d.coarse[i],
        };
        assert_eq!(match_one(&d, &q, 1_024, false).index, i);
    }
}
