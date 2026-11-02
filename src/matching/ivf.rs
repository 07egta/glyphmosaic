use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use super::scan::truncate_top;
use super::simd::coarse_score64;
use crate::dict::Dictionary;

pub const IVF_MAGIC: &[u8; 4] = b"GMI1";
pub const IVF_VERSION: u32 = 1;
pub const IVF_DIMS: usize = 64;
pub const IVF_K: usize = 4_096;
pub const IVF_PROBE: usize = 128;
const SAMPLE_LIMIT: usize = 16_000;

#[derive(Clone, Debug, PartialEq)]
pub struct IvfIndex {
    pub k: usize,
    pub centroids: Vec<[u8; IVF_DIMS]>,
    pub assigns: Vec<u16>,
    pub lists: Vec<Vec<u32>>,
}

pub fn index_path(dict_path: &Path) -> PathBuf {
    let mut p = dict_path.as_os_str().to_os_string();
    p.push(".idx");
    PathBuf::from(p)
}

fn nearest(centroids: &[[u8; IVF_DIMS]], point: &[u8; IVF_DIMS]) -> (u32, usize) {
    let mut best = (u32::MAX, usize::MAX);
    for (i, c) in centroids.iter().enumerate() {
        let score = coarse_score64(point, c);
        if (score, i) < best {
            best = (score, i);
        }
    }
    best
}

impl IvfIndex {
    pub fn train(dict: &Dictionary, k: usize, iterations: usize, threads: usize) -> Result<Self> {
        if dict.is_empty() {
            bail!("cannot train an index on an empty dictionary");
        }
        let k = k.min(dict.len()).max(1);
        let stride = (dict.len() / SAMPLE_LIMIT).max(1);
        let sample: Vec<[u8; IVF_DIMS]> = dict
            .coarse8
            .iter()
            .step_by(stride)
            .take(SAMPLE_LIMIT)
            .copied()
            .collect();

        // Systematic initialization spreads the initial centers across the
        // dictionary instead of depending on a random seed.
        let mut centroids: Vec<[u8; IVF_DIMS]> =
            (0..k).map(|i| sample[i * sample.len() / k]).collect();

        let mut pool_builder = rayon::ThreadPoolBuilder::new();
        if threads > 0 {
            pool_builder = pool_builder.num_threads(threads);
        }
        let pool = pool_builder
            .build()
            .context("failed to build index training pool")?;

        for _ in 0..iterations {
            let assigns: Vec<u16> = pool.install(|| {
                sample
                    .par_iter()
                    .map(|p| nearest(&centroids, p).1 as u16)
                    .collect()
            });
            let mut sums = vec![[0u64; IVF_DIMS]; k];
            let mut counts = vec![0u64; k];
            for (p, &a) in sample.iter().zip(&assigns) {
                let idx = a as usize;
                counts[idx] += 1;
                for d in 0..IVF_DIMS {
                    sums[idx][d] += p[d] as u64;
                }
            }
            for c in 0..k {
                let count = counts[c];
                if count == 0 {
                    centroids[c] = sample[c % sample.len()];
                } else {
                    for d in 0..IVF_DIMS {
                        let avg = (sums[c][d] + count / 2).checked_div(count).unwrap_or(0);
                        centroids[c][d] = avg as u8;
                    }
                }
            }
        }

        let assigns: Vec<u16> = pool.install(|| {
            dict.coarse8
                .par_iter()
                .map(|p| nearest(&centroids, p).1 as u16)
                .collect()
        });
        let mut lists = vec![Vec::new(); k];
        for (i, &a) in assigns.iter().enumerate() {
            lists[a as usize].push(i as u32);
        }
        Ok(Self {
            k,
            centroids,
            assigns,
            lists,
        })
    }

    pub fn candidates(&self, q: &[u8; IVF_DIMS], probe: usize) -> Vec<u32> {
        let mut scored: Vec<(u32, usize)> = self
            .centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (coarse_score64(q, c), i))
            .collect();
        truncate_top(&mut scored, probe.max(1));
        let mut out = Vec::new();
        for (_, cluster) in scored {
            out.extend_from_slice(&self.lists[cluster]);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut out = BufWriter::new(
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?,
        );
        out.write_all(IVF_MAGIC)?;
        out.write_all(&IVF_VERSION.to_le_bytes())?;
        out.write_all(&(self.k as u32).to_le_bytes())?;
        out.write_all(&(IVF_DIMS as u32).to_le_bytes())?;
        out.write_all(&(self.assigns.len() as u32).to_le_bytes())?;
        for c in &self.centroids {
            out.write_all(c)?;
        }
        for a in &self.assigns {
            out.write_all(&a.to_le_bytes())?;
        }
        out.flush()?;
        Ok(())
    }

    pub fn read(path: &Path, expected_entries: usize) -> Result<Self> {
        let mut input =
            File::open(path).with_context(|| format!("failed to open index {}", path.display()))?;
        let mut magic = [0u8; 4];
        input.read_exact(&mut magic)?;
        if &magic != IVF_MAGIC {
            bail!("bad index magic");
        }
        let version = read_u32(&mut input)?;
        if version != IVF_VERSION {
            bail!("unsupported index version {version}");
        }
        let k = read_u32(&mut input)? as usize;
        let dims = read_u32(&mut input)? as usize;
        let count = read_u32(&mut input)? as usize;
        if dims != IVF_DIMS || k == 0 {
            bail!("index is incompatible: {k} clusters of {dims} dimensions");
        }
        if count != expected_entries {
            bail!("index entry count {count} does not match dictionary {expected_entries}");
        }
        let file_size = std::fs::metadata(path)?.len() as usize;
        let expected = 20usize
            .checked_add(
                k.checked_mul(IVF_DIMS)
                    .ok_or_else(|| anyhow::anyhow!("index overflow"))?,
            )
            .and_then(|v| v.checked_add(count.checked_mul(2)?))
            .ok_or_else(|| anyhow::anyhow!("index size overflow"))?;
        if file_size != expected {
            bail!("index size mismatch: expected {expected} bytes, got {file_size}");
        }

        let mut centroids = Vec::with_capacity(k);
        for _ in 0..k {
            let mut c = [0u8; IVF_DIMS];
            input.read_exact(&mut c)?;
            centroids.push(c);
        }
        let mut assigns = Vec::with_capacity(count);
        for _ in 0..count {
            assigns.push(read_u16(&mut input)?);
        }
        let mut lists = vec![Vec::new(); k];
        for (i, &a) in assigns.iter().enumerate() {
            if a as usize >= k {
                bail!("index assignment {} is outside {k} clusters", a);
            }
            lists[a as usize].push(i as u32);
        }
        Ok(Self {
            k,
            centroids,
            assigns,
            lists,
        })
    }
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::{DictHeader, Dictionary};

    fn dict(n: usize) -> Dictionary {
        let mut d = Dictionary::new(DictHeader::default());
        for i in 0..n {
            let mut full = [0u8; 256];
            for (j, v) in full.iter_mut().enumerate() {
                *v = ((i * 37 + j * 11) % 256) as u8;
            }
            let mut coarse = [0u8; 16];
            for (j, v) in coarse.iter_mut().enumerate() {
                *v = full[j * 16];
            }
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
    fn trains_and_roundtrips() {
        let d = dict(1_000);
        let idx = IvfIndex::train(&d, 64, 3, 2).unwrap();
        assert_eq!(idx.assigns.len(), d.len());
        let candidates = idx.candidates(&d.coarse8[123], 64);
        assert!(candidates.contains(&123));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.idx");
        idx.write(&path).unwrap();
        let back = IvfIndex::read(&path, d.len()).unwrap();
        assert_eq!(back.centroids, idx.centroids);
        assert_eq!(back.assigns, idx.assigns);
    }
}
