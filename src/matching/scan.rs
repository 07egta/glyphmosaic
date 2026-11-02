use std::collections::BinaryHeap;

use super::simd::{coarse_score, full_score};
use super::{MatchResult, Query};
use crate::dict::Dictionary;

/// Return the `top_k` best `(mse, index)` candidates by coarse score,
/// sorted by `(mse, index)`.
pub fn scan_coarse(dict: &Dictionary, q: &[u8; 16], top_k: usize) -> Vec<(u32, usize)> {
    if dict.is_empty() {
        return Vec::new();
    }
    let k = top_k.max(1).min(dict.len());
    let mut heap: BinaryHeap<(u32, usize)> = BinaryHeap::with_capacity(k + 1);
    for (index, glyph) in dict.coarse.iter().enumerate() {
        let score = coarse_score(q, glyph);
        if heap.len() < k {
            heap.push((score, index));
        } else if let Some(&worst) = heap.peek() {
            if (score, index) < worst {
                heap.pop();
                heap.push((score, index));
            }
        }
    }
    let mut out = heap.into_vec();
    out.sort_unstable();
    out
}

/// Rescore candidates at full resolution and return `(index, mse)`.
/// With an empty candidate list, scan the whole dictionary.
pub fn refine_full(
    dict: &Dictionary,
    q: &[u8; 256],
    candidates: &[(u32, usize)],
) -> (usize, u32) {
    if dict.is_empty() {
        return (0, u32::MAX);
    }
    let mut best = (u32::MAX, usize::MAX);
    if candidates.is_empty() {
        for (index, glyph) in dict.full.iter().enumerate() {
            let score = full_score(q, glyph);
            if (score, index) < best {
                best = (score, index);
            }
        }
    } else {
        for &(_, index) in candidates {
            if index >= dict.len() {
                continue;
            }
            let score = full_score(q, &dict.full[index]);
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
        let candidates = scan_coarse(dict, &q.coarse, top_k);
        refine_full(dict, &q.full, &candidates)
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
        };
        let mut d = Dictionary::new(header);
        for i in 0..entries {
            let mut full = [0u8; 256];
            let mut coarse = [0u8; 16];
            for j in 0..256 {
                full[j] = ((i * 37 + j * 11) % 256) as u8;
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
}
