pub mod ivf;
pub mod scan;
pub mod simd;

use std::collections::HashMap;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::dict::Dictionary;

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct Query {
    pub full: [u8; 256],
    pub coarse: [u8; 16],
}

/// `MatchResult::index` of a cell that is left empty (black). It is chosen when
/// the cell is so dark that drawing any glyph would fit worse than nothing.
pub const BLANK: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchResult {
    pub index: usize,
    pub mse: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchStats {
    pub queries: usize,
    pub unique: usize,
}

/// Most distinct queries remembered across frames (~350 bytes each). When the
/// memo is full it is simply emptied; it only has to catch repeats such as flat
/// black/white cells and static backgrounds, which recur constantly.
const MEMO_CAPACITY: usize = 1 << 16;

/// Matches cells against one dictionary, reusing work.
///
/// A match is a pure function of the query (for a fixed dictionary, `top_k` and
/// `exact`), so two kinds of repeats are free: identical cells inside one frame
/// are matched once, and a cell identical to one from an earlier frame is
/// answered from a memo. Neither changes any result. The thread pool, if one was
/// requested with `threads`, is built once instead of once per frame.
pub struct Matcher {
    top_k: usize,
    exact: bool,
    pool: Option<rayon::ThreadPool>,
    memo: HashMap<Query, MatchResult>,
    memo_dict_len: usize,
}

impl Matcher {
    pub fn new(top_k: usize, exact: bool, threads: usize) -> Result<Self> {
        let pool = if threads > 0 {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .context("failed to build matching thread pool")?,
            )
        } else {
            None
        };
        Ok(Self {
            top_k,
            exact,
            pool,
            memo: HashMap::new(),
            memo_dict_len: 0,
        })
    }

    /// Match every query in `queries`. Always call with the same dictionary.
    pub fn match_cells(
        &mut self,
        dict: &Dictionary,
        queries: &[Query],
    ) -> (Vec<MatchResult>, MatchStats) {
        if self.memo_dict_len != dict.len() {
            self.memo.clear();
            self.memo_dict_len = dict.len();
        }

        let mut index_of: HashMap<Query, usize> = HashMap::new();
        let mut unique: Vec<Query> = Vec::new();
        let mut order: Vec<usize> = Vec::with_capacity(queries.len());
        for q in queries {
            let idx = *index_of.entry(*q).or_insert_with(|| {
                unique.push(*q);
                unique.len() - 1
            });
            order.push(idx);
        }

        let mut results: Vec<Option<MatchResult>> =
            unique.iter().map(|q| self.memo.get(q).copied()).collect();
        let todo: Vec<usize> = (0..unique.len())
            .filter(|&i| results[i].is_none())
            .collect();

        if !todo.is_empty() {
            let (top_k, exact) = (self.top_k, self.exact);
            let run = || {
                todo.par_iter()
                    .map(|&i| scan::match_one(dict, &unique[i], top_k, exact))
                    .collect::<Vec<_>>()
            };
            let computed = match &self.pool {
                Some(pool) => pool.install(run),
                None => run(),
            };
            if self.memo.len() + todo.len() > MEMO_CAPACITY {
                self.memo.clear();
            }
            for (&i, r) in todo.iter().zip(computed) {
                self.memo.insert(unique[i], r);
                results[i] = Some(r);
            }
        }

        let out = order
            .iter()
            .map(|&i| results[i].expect("every unique query has a result"))
            .collect();
        (
            out,
            MatchStats {
                queries: queries.len(),
                unique: unique.len(),
            },
        )
    }
}

/// One-shot convenience wrapper around [`Matcher`] (no memo across calls).
pub fn match_cells(
    dict: &Dictionary,
    queries: &[Query],
    top_k: usize,
    exact: bool,
    threads: usize,
) -> (Vec<MatchResult>, MatchStats) {
    Matcher::new(top_k, exact, threads)
        .expect("failed to build rayon thread pool")
        .match_cells(dict, queries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::{DictHeader, Dictionary};

    fn dict(n: usize) -> Dictionary {
        let header = DictHeader {
            version: 1,
            cell_feature: 16,
            coarse_feature: 4,
            unicode_version: "17.0".into(),
            fonts: vec![],
            ..DictHeader::default()
        };
        let mut d = Dictionary::new(header);
        for i in 0..n {
            let mut f = [0u8; 256];
            let mut c = [0u8; 16];
            f.fill((i % 200) as u8);
            c.fill((i % 200) as u8);
            d.push('A', 0, f, c);
        }
        d
    }

    #[test]
    fn deduplicates_identical_queries() {
        let d = dict(100);
        let q0 = Query {
            full: d.full[10],
            coarse: d.coarse[10],
        };
        let q1 = Query {
            full: d.full[20],
            coarse: d.coarse[20],
        };
        let queries = vec![q0, q1, q0, q0];
        let (r, s) = match_cells(&d, &queries, 16, false, 1);
        assert_eq!(s.queries, 4);
        assert_eq!(s.unique, 2);
        assert_eq!(r[0].index, 10);
        assert_eq!(r[1].index, 20);
        assert_eq!(r[2], r[0]);
    }

    #[test]
    fn thread_count_does_not_change_results() {
        let d = dict(200);
        let queries: Vec<Query> = (0..40)
            .map(|i| Query {
                full: d.full[i * 3],
                coarse: d.coarse[i * 3],
            })
            .collect();
        let (a, _) = match_cells(&d, &queries, 32, false, 1);
        let (b, _) = match_cells(&d, &queries, 32, false, 4);
        assert_eq!(a, b);
    }

    #[test]
    fn memo_never_changes_results() {
        let d = dict(200);
        let frame = |range: std::ops::Range<usize>| -> Vec<Query> {
            range
                .map(|i| Query {
                    full: d.full[i * 7],
                    coarse: d.coarse[i * 7],
                })
                .collect()
        };
        let frame1 = frame(0..10);
        let frame2 = frame(5..15); // overlaps frame1, so some answers come from the memo
        let mut matcher = Matcher::new(32, false, 2).unwrap();
        let a1 = matcher.match_cells(&d, &frame1).0;
        let a2 = matcher.match_cells(&d, &frame2).0;
        let a2_again = matcher.match_cells(&d, &frame2).0;
        assert_eq!(a1, match_cells(&d, &frame1, 32, false, 0).0);
        assert_eq!(a2, match_cells(&d, &frame2, 32, false, 0).0);
        assert_eq!(a2, a2_again);
        assert_eq!(a1[5], a2[0], "same cell must give the same match");
    }

    #[test]
    fn memo_resets_when_the_dictionary_changes() {
        let small = dict(50);
        let big = dict(150);
        let q = Query {
            full: big.full[120],
            coarse: big.coarse[120],
        };
        let mut matcher = Matcher::new(8, false, 0).unwrap();
        let _ = matcher.match_cells(&small, &[q]);
        let r = matcher.match_cells(&big, &[q]).0;
        assert_eq!(r, match_cells(&big, &[q], 8, false, 0).0);
    }

    #[test]
    fn black_query_is_left_blank() {
        let d = dict(50);
        let q = Query {
            full: [0u8; 256],
            coarse: [0u8; 16],
        };
        let (r, _) = match_cells(&d, &[q], 8, false, 1);
        assert_eq!(r[0].index, BLANK);
    }

    #[test]
    fn white_query_still_returns_an_index() {
        let d = dict(50);
        let q = Query {
            full: [255u8; 256],
            coarse: [255u8; 16],
        };
        let (r, _) = match_cells(&d, &[q], 8, false, 1);
        assert!(r[0].index < d.len());
    }
}
