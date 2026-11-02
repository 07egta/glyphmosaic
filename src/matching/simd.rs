//! Squared-error kernels for the matching stages.
//!
//! Every function returns the exact integer sum of squared byte differences, so
//! the AVX2 and scalar paths are interchangeable and results never depend on the
//! CPU. The wide kernels (`coarse_score64`, `full_score`) do the whole vector in
//! one call instead of looping over 16-byte pieces, which keeps the hot matching
//! loops free of per-piece call and feature-detection overhead.

#[inline]
fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

fn sq_scalar<const N: usize>(q: &[u8; N], g: &[u8; N]) -> u32 {
    q.iter()
        .zip(g.iter())
        .map(|(&a, &b)| {
            let d = a.abs_diff(b) as u32;
            d * d
        })
        .sum()
}

pub fn coarse_score(q: &[u8; 16], g: &[u8; 16]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: feature detection above guarantees AVX2.
        return unsafe { avx2::score16(q.as_ptr(), g.as_ptr()) };
    }
    coarse_score_scalar(q, g)
}

fn coarse_score_scalar(q: &[u8; 16], g: &[u8; 16]) -> u32 {
    sq_scalar(q, g)
}

/// Squared error over an 8×8 feature (64 bytes).
pub fn coarse_score64(q: &[u8; 64], g: &[u8; 64]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: feature detection above guarantees AVX2; both arrays are 64 bytes.
        return unsafe { avx2::score64(q.as_ptr(), g.as_ptr()) };
    }
    sq_scalar(q, g)
}

/// Squared error over a 16×16 feature (256 bytes).
pub fn full_score(q: &[u8; 256], g: &[u8; 256]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: feature detection above guarantees AVX2; both arrays are 256 bytes.
        return unsafe { avx2::score256(q.as_ptr(), g.as_ptr()) };
    }
    sq_scalar(q, g)
}

/// Like [`full_score`], but may stop early once the running sum exceeds `bound`.
///
/// If the true score is `<= bound` the exact score is returned. Otherwise the
/// return value is some number `> bound` that is `<=` the true score, which is
/// all a "is this better than the best so far" comparison needs.
pub fn full_score_bounded(q: &[u8; 256], g: &[u8; 256], bound: u32) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: feature detection above guarantees AVX2; both arrays are 256 bytes.
        return unsafe { avx2::score256_bounded(q.as_ptr(), g.as_ptr(), bound) };
    }
    sq_scalar(q, g)
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    /// Eight partial sums (as i32 lanes) of the squared differences of 32 bytes.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn sq32(q: *const u8, g: *const u8) -> __m256i {
        let a = _mm256_loadu_si256(q as *const __m256i);
        let b = _mm256_loadu_si256(g as *const __m256i);
        // |a - b| for unsigned bytes: one of the two saturating subtractions is 0.
        let d = _mm256_or_si256(_mm256_subs_epu8(a, b), _mm256_subs_epu8(b, a));
        let zero = _mm256_setzero_si256();
        let lo = _mm256_unpacklo_epi8(d, zero);
        let hi = _mm256_unpackhi_epi8(d, zero);
        _mm256_add_epi32(_mm256_madd_epi16(lo, lo), _mm256_madd_epi16(hi, hi))
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn hsum(v: __m256i) -> u32 {
        let s = _mm_add_epi32(_mm256_castsi256_si128(v), _mm256_extracti128_si256(v, 1));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b01_00_11_10));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b10_11_00_01));
        _mm_cvtsi128_si32(s) as u32
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn score16(q: *const u8, g: *const u8) -> u32 {
        let qv = _mm_loadu_si128(q as *const __m128i);
        let gv = _mm_loadu_si128(g as *const __m128i);
        let q16 = _mm256_cvtepu8_epi16(qv);
        let g16 = _mm256_cvtepu8_epi16(gv);
        let d = _mm256_sub_epi16(q16, g16);
        hsum(_mm256_madd_epi16(d, d))
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn score64(q: *const u8, g: *const u8) -> u32 {
        let acc = _mm256_add_epi32(sq32(q, g), sq32(q.add(32), g.add(32)));
        hsum(acc)
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn score256(q: *const u8, g: *const u8) -> u32 {
        let mut acc = sq32(q, g);
        for i in 1..8 {
            acc = _mm256_add_epi32(acc, sq32(q.add(i * 32), g.add(i * 32)));
        }
        hsum(acc)
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn score256_bounded(q: *const u8, g: *const u8, bound: u32) -> u32 {
        let mut acc = _mm256_setzero_si256();
        for block in 0..4 {
            let off = block * 64;
            acc = _mm256_add_epi32(acc, sq32(q.add(off), g.add(off)));
            acc = _mm256_add_epi32(acc, sq32(q.add(off + 32), g.add(off + 32)));
            if block < 3 {
                let partial = hsum(acc);
                if partial > bound {
                    return partial;
                }
            }
        }
        hsum(acc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u32);
    impl Rng {
        fn byte(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            (self.0 >> 24) as u8
        }
        fn fill<const N: usize>(&mut self) -> [u8; N] {
            let mut a = [0u8; N];
            for v in a.iter_mut() {
                *v = self.byte();
            }
            a
        }
    }

    #[test]
    fn coarse_matches_hand_calculation() {
        let q = [10u8; 16];
        let g = [12u8; 16];
        assert_eq!(coarse_score(&q, &g), 64);
    }

    #[test]
    fn full_matches_hand_calculation() {
        let q = [0u8; 256];
        let mut g = [0u8; 256];
        g[0] = 3;
        g[255] = 4;
        assert_eq!(full_score(&q, &g), 25);
    }

    #[test]
    fn coarse64_matches_hand_calculation() {
        let q = [10u8; 64];
        let g = [12u8; 64];
        assert_eq!(coarse_score64(&q, &g), 256);
    }

    #[test]
    fn extremes_do_not_overflow() {
        let zeros = [0u8; 256];
        let ones = [255u8; 256];
        assert_eq!(full_score(&zeros, &ones), 256 * 255 * 255);
        assert_eq!(full_score(&ones, &zeros), 256 * 255 * 255);
        assert_eq!(coarse_score64(&[0u8; 64], &[255u8; 64]), 64 * 255 * 255);
        assert_eq!(coarse_score(&[255u8; 16], &[0u8; 16]), 16 * 255 * 255);
    }

    #[test]
    fn avx2_and_scalar_agree() {
        let mut rng = Rng(0x1234_5678);
        for _ in 0..500 {
            let q16: [u8; 16] = rng.fill();
            let g16: [u8; 16] = rng.fill();
            assert_eq!(coarse_score(&q16, &g16), sq_scalar(&q16, &g16));
            let q64: [u8; 64] = rng.fill();
            let g64: [u8; 64] = rng.fill();
            assert_eq!(coarse_score64(&q64, &g64), sq_scalar(&q64, &g64));
            let q256: [u8; 256] = rng.fill();
            let g256: [u8; 256] = rng.fill();
            assert_eq!(full_score(&q256, &g256), sq_scalar(&q256, &g256));
        }
    }

    #[test]
    fn bounded_is_exact_when_within_bound_and_a_lower_bound_otherwise() {
        let mut rng = Rng(0xDEAD_BEEF);
        for _ in 0..2_000 {
            let q: [u8; 256] = rng.fill();
            // Mix near and far candidates so both outcomes are exercised.
            let mut g: [u8; 256] = rng.fill();
            if rng.byte() & 1 == 0 {
                for (gv, &qv) in g.iter_mut().zip(q.iter()) {
                    *gv = qv.saturating_add(rng.byte() & 7);
                }
            }
            let exact = sq_scalar(&q, &g);
            for bound in [0, exact.saturating_sub(1), exact, exact + 1, u32::MAX] {
                let got = full_score_bounded(&q, &g, bound);
                if exact <= bound {
                    assert_eq!(got, exact, "bound {bound}");
                } else {
                    assert!(
                        got > bound && got <= exact,
                        "bound {bound} got {got} exact {exact}"
                    );
                }
            }
        }
    }
}
