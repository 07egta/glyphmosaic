pub fn coarse_score(q: &[u8; 16], g: &[u8; 16]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: feature detection above guarantees AVX2.
        return unsafe { coarse_score_avx2(q, g) };
    }
    coarse_score_scalar(q, g)
}

fn coarse_score_scalar(q: &[u8; 16], g: &[u8; 16]) -> u32 {
    q.iter()
        .zip(g.iter())
        .map(|(&a, &b)| {
            let d = a as i32 - b as i32;
            (d * d) as u32
        })
        .sum()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn coarse_score_avx2(q: &[u8; 16], g: &[u8; 16]) -> u32 {
    use std::arch::x86_64::*;
    let qv = _mm_loadu_si128(q.as_ptr() as *const __m128i);
    let gv = _mm_loadu_si128(g.as_ptr() as *const __m128i);
    let q16 = _mm256_cvtepu8_epi16(qv);
    let g16 = _mm256_cvtepu8_epi16(gv);
    let d = _mm256_sub_epi16(q16, g16);
    let sums = _mm256_madd_epi16(d, d);
    let lo = _mm256_castsi256_si128(sums);
    let hi = _mm256_extracti128_si256(sums, 1);
    let s = _mm_add_epi32(lo, hi);
    let s = _mm_hadd_epi32(s, s);
    let s = _mm_hadd_epi32(s, s);
    _mm_cvtsi128_si32(s) as u32
}

pub fn full_score(q: &[u8; 256], g: &[u8; 256]) -> u32 {
    let mut sum = 0u32;
    for i in 0..16 {
        let qc: &[u8; 16] = q[i * 16..i * 16 + 16].try_into().unwrap();
        let gc: &[u8; 16] = g[i * 16..i * 16 + 16].try_into().unwrap();
        sum += coarse_score(qc, gc);
    }
    sum
}

pub fn coarse_score64(q: &[u8; 64], g: &[u8; 64]) -> u32 {
    let mut sum = 0u32;
    for i in 0..4 {
        let qc: &[u8; 16] = q[i * 16..i * 16 + 16].try_into().unwrap();
        let gc: &[u8; 16] = g[i * 16..i * 16 + 16].try_into().unwrap();
        sum += coarse_score(qc, gc);
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn avx2_and_scalar_agree() {
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        for _ in 0..100 {
            let mut q = [0u8; 16];
            let mut g = [0u8; 16];
            for i in 0..16 {
                q[i] = next();
                g[i] = next();
            }
            assert_eq!(coarse_score(&q, &g), coarse_score_scalar(&q, &g));
        }
    }
}
