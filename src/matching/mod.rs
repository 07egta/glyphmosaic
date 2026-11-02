pub mod ivf;
pub mod scan;
pub mod simd;

use std::collections::HashMap;

use rayon::prelude::*;

use crate::dict::Dictionary;

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct Query {
    pub full: [u8; 256],
    pub coarse: [u8; 16],
}

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

pub fn match_cells(
    dict: &Dictionary,
    queries: &[Query],
    top_k: usize,
    exact: bool,
    threads: usize,
) -> (Vec<MatchResult>, MatchStats) {
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

    let run = || {
        unique
            .par_iter()
            .map(|q| scan::match_one(dict, q, top_k, exact))
            .collect::<Vec<_>>()
    };
    let results = if threads > 0 {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("failed to build rayon thread pool");
        pool.install(run)
    } else {
        run()
    };

    let out = order.iter().map(|&i| results[i]).collect();
    (
        out,
        MatchStats {
            queries: queries.len(),
            unique: unique.len(),
        },
    )
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
    fn black_query_still_returns_an_index() {
        let d = dict(50);
        let q = Query {
            full: [0u8; 256],
            coarse: [0u8; 16],
        };
        let (r, _) = match_cells(&d, &[q], 8, false, 1);
        assert!(r[0].index < d.len());
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
