use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Rec.601 luma, fixed point.
pub fn rgb_to_gray(rgb: &[u8], out: &mut [u8]) {
    assert_eq!(
        rgb.len(),
        out.len() * 3,
        "rgb buffer must hold 3 bytes per output pixel"
    );
    for (o, p) in out.iter_mut().zip(rgb.chunks_exact(3)) {
        *o = ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32 + 128) >> 8) as u8;
    }
}

fn area_weights(src_len: usize, dst_len: usize) -> Vec<Vec<(usize, f64)>> {
    let scale = src_len as f64 / dst_len as f64;
    (0..dst_len)
        .map(|i| {
            let start = i as f64 * scale;
            let end = (i + 1) as f64 * scale;
            let width = (end - start).max(f64::EPSILON);
            let mut weights = Vec::new();
            let first = start.floor() as usize;
            let last = end.ceil() as usize;
            for s in first..last.min(src_len) {
                let lo = start.max(s as f64);
                let hi = end.min((s + 1) as f64);
                if hi > lo {
                    weights.push((s, (hi - lo) / width));
                }
            }
            weights
        })
        .collect()
}

/// Box-filter weights for one axis, computed once and stored flat.
///
/// Building these is cheap but not free, and the same sizes repeat for every
/// frame (and for every one of the ~200,000 glyphs in a dictionary build), so
/// callers keep them around instead of rebuilding per call.
#[derive(Clone, Debug)]
pub struct AxisWeights {
    offsets: Vec<usize>,
    entries: Vec<(usize, f64)>,
}

impl AxisWeights {
    pub fn new(src_len: usize, dst_len: usize) -> Self {
        let mut offsets = Vec::with_capacity(dst_len + 1);
        let mut entries = Vec::new();
        offsets.push(0);
        for list in area_weights(src_len, dst_len) {
            entries.extend(list);
            offsets.push(entries.len());
        }
        Self { offsets, entries }
    }

    pub fn dst_len(&self) -> usize {
        self.offsets.len() - 1
    }

    #[inline]
    fn span(&self, i: usize) -> &[(usize, f64)] {
        &self.entries[self.offsets[i]..self.offsets[i + 1]]
    }
}

thread_local! {
    static AXIS_CACHE: RefCell<HashMap<(usize, usize), Rc<AxisWeights>>> =
        RefCell::new(HashMap::new());
}

fn cached_axis(src_len: usize, dst_len: usize) -> Rc<AxisWeights> {
    AXIS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 64 {
            cache.clear();
        }
        cache
            .entry((src_len, dst_len))
            .or_insert_with(|| Rc::new(AxisWeights::new(src_len, dst_len)))
            .clone()
    })
}

/// First pass: area-average one source row horizontally into `out` (f64).
#[inline]
fn horizontal_row(row: &[u8], xw: &AxisWeights, out: &mut [f64]) {
    for (x, o) in out.iter_mut().enumerate() {
        let mut v = 0f64;
        for &(s, w) in xw.span(x) {
            v += row[s] as f64 * w;
        }
        *o = v;
    }
}

/// Second pass: combine the horizontally reduced rows vertically.
fn vertical_pass(tmp: &[f64], yw: &AxisWeights, dst: &mut [u8], dw: usize) {
    for y in 0..yw.dst_len() {
        let weights = yw.span(y);
        for x in 0..dw {
            let mut v = 0f64;
            for &(s, w) in weights {
                v += tmp[s * dw + x] * w;
            }
            dst[y * dw + x] = (v.clamp(0.0, 255.0) + 0.5).floor() as u8;
        }
    }
}

/// Area-average downscale (box filter). Works for upscale too, where it
/// degenerates to a nearest-ish sample.
pub fn resize_area_gray(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    assert_eq!(src.len(), sw * sh);
    assert_eq!(dst.len(), dw * dh);
    let xw = cached_axis(sw, dw);
    let yw = cached_axis(sh, dh);

    let mut tmp = vec![0f64; sh * dw];
    for y in 0..sh {
        horizontal_row(
            &src[y * sw..(y + 1) * sw],
            &xw,
            &mut tmp[y * dw..(y + 1) * dw],
        );
    }
    vertical_pass(&tmp, &yw, dst, dw);
}

/// Rec.601 luma of an RGB frame, area-averaged down to `dw × dh`.
///
/// Gives exactly the same bytes as `rgb_to_gray` followed by
/// [`resize_area_gray`], but converts each row to gray only while it is being
/// reduced (no full-frame gray copy) and spreads the rows over the thread pool.
pub fn resize_area_luma_from_rgb(
    rgb: &[u8],
    sw: usize,
    sh: usize,
    dst: &mut [u8],
    dw: usize,
    dh: usize,
) {
    assert_eq!(rgb.len(), sw * sh * 3);
    assert_eq!(dst.len(), dw * dh);
    let xw = AxisWeights::new(sw, dw);
    let yw = AxisWeights::new(sh, dh);

    let mut tmp = vec![0f64; sh * dw];
    tmp.par_chunks_mut(dw).enumerate().for_each_init(
        || vec![0u8; sw],
        |gray, (y, out)| {
            rgb_to_gray(&rgb[y * sw * 3..(y + 1) * sw * 3], gray);
            horizontal_row(gray, &xw, out);
        },
    );
    vertical_pass(&tmp, &yw, dst, dw);
}

/// Source taps `(i0, i1, frac)` for each destination position on one axis.
pub type BilinearAxis = Vec<(usize, usize, f64)>;

pub fn bilinear_axis(src_len: usize, dst_len: usize) -> BilinearAxis {
    (0..dst_len)
        .map(|i| {
            let x = ((i as f64 + 0.5) * src_len as f64 / dst_len as f64 - 0.5)
                .clamp(0.0, (src_len - 1) as f64);
            let i0 = x.floor() as usize;
            let i1 = (i0 + 1).min(src_len - 1);
            (i0, i1, x - i0 as f64)
        })
        .collect()
}

pub fn resize_bilinear_gray(
    src: &[u8],
    sw: usize,
    sh: usize,
    dst: &mut [u8],
    dw: usize,
    dh: usize,
) {
    assert_eq!(src.len(), sw * sh);
    assert_eq!(dst.len(), dw * dh);
    let xs = bilinear_axis(sw, dw);
    let ys = bilinear_axis(sh, dh);
    for (y, &(y0, y1, ty)) in ys.iter().enumerate() {
        for (x, &(x0, x1, tx)) in xs.iter().enumerate() {
            let a = src[y0 * sw + x0] as f64;
            let b = src[y0 * sw + x1] as f64;
            let c = src[y1 * sw + x0] as f64;
            let d = src[y1 * sw + x1] as f64;
            let top = a + (b - a) * tx;
            let bottom = c + (d - c) * tx;
            let v = top + (bottom - top) * ty;
            dst[y * dw + x] = (v.clamp(0.0, 255.0) + 0.5).floor() as u8;
        }
    }
}

pub fn resize_bilinear_rgb(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    assert_eq!(src.len(), sw * sh * 3);
    assert_eq!(dst.len(), dw * dh * 3);
    let xs = bilinear_axis(sw, dw);
    let ys = bilinear_axis(sh, dh);
    resize_bilinear_rgb_with(src, sw, &xs, &ys, dst);
}

/// Bilinear RGB resize with precomputed axis taps; rows are independent, so
/// they run in parallel. The per-pixel arithmetic is the same as
/// [`resize_bilinear_rgb`].
pub fn resize_bilinear_rgb_with(
    src: &[u8],
    sw: usize,
    xs: &BilinearAxis,
    ys: &BilinearAxis,
    dst: &mut [u8],
) {
    let dw = xs.len();
    assert_eq!(dst.len(), dw * ys.len() * 3);
    dst.par_chunks_mut(dw * 3)
        .zip(ys.par_iter())
        .for_each(|(row, &(y0, y1, ty))| {
            for (x, &(x0, x1, tx)) in xs.iter().enumerate() {
                let p00 = (y0 * sw + x0) * 3;
                let p01 = (y0 * sw + x1) * 3;
                let p10 = (y1 * sw + x0) * 3;
                let p11 = (y1 * sw + x1) * 3;
                let out = x * 3;
                for c in 0..3 {
                    let a = src[p00 + c] as f64;
                    let b = src[p01 + c] as f64;
                    let c0 = src[p10 + c] as f64;
                    let d = src[p11 + c] as f64;
                    let top = a + (b - a) * tx;
                    let bottom = c0 + (d - c0) * tx;
                    let v = top + (bottom - top) * ty;
                    row[out + c] = (v.clamp(0.0, 255.0) + 0.5).floor() as u8;
                }
            }
        });
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
    }

    /// The original, unoptimized area resize, kept as the reference.
    fn reference_resize_area_gray(
        src: &[u8],
        sw: usize,
        sh: usize,
        dst: &mut [u8],
        dw: usize,
        dh: usize,
    ) {
        let xw = area_weights(sw, dw);
        let yw = area_weights(sh, dh);
        let mut tmp = vec![0f64; sh * dw];
        for y in 0..sh {
            let row = &src[y * sw..(y + 1) * sw];
            for (x, weights) in xw.iter().enumerate() {
                let mut v = 0f64;
                for &(s, w) in weights {
                    v += row[s] as f64 * w;
                }
                tmp[y * dw + x] = v;
            }
        }
        for (y, weights) in yw.iter().enumerate() {
            for x in 0..dw {
                let mut v = 0f64;
                for &(s, w) in weights {
                    v += tmp[s * dw + x] * w;
                }
                dst[y * dw + x] = (v.clamp(0.0, 255.0) + 0.5).floor() as u8;
            }
        }
    }

    #[test]
    fn rgb_to_gray_uses_rec601() {
        let mut out = [0u8; 2];
        rgb_to_gray(&[255, 0, 0, 0, 255, 0], &mut out);
        assert!((out[0] as i32 - 76).abs() <= 1);
        assert!((out[1] as i32 - 150).abs() <= 1);
    }

    #[test]
    fn area_resize_averages_2x2_to_1x1() {
        let src = [0u8, 100, 200, 255];
        let mut dst = [0u8; 1];
        resize_area_gray(&src, 2, 2, &mut dst, 1, 1);
        assert_eq!(dst[0], 139); // round((0+100+200+255)/4) = 139
    }

    #[test]
    fn area_resize_matches_reference_bit_for_bit() {
        let mut rng = Rng(7);
        // Integer ratios, fractional ratios, upscales, and the dictionary's 64 -> 16.
        for &(sw, sh, dw, dh) in &[
            (64, 64, 16, 16),
            (1920, 1080, 64, 48),
            (101, 75, 32, 16),
            (30, 30, 7, 5),
            (5, 3, 16, 16),
            (16, 16, 16, 16),
        ] {
            let src: Vec<u8> = (0..sw * sh).map(|_| rng.byte()).collect();
            let mut want = vec![0u8; dw * dh];
            let mut got = vec![0u8; dw * dh];
            reference_resize_area_gray(&src, sw, sh, &mut want, dw, dh);
            resize_area_gray(&src, sw, sh, &mut got, dw, dh);
            assert_eq!(got, want, "{sw}x{sh} -> {dw}x{dh}");
            // A second call goes through the cached weights.
            resize_area_gray(&src, sw, sh, &mut got, dw, dh);
            assert_eq!(got, want, "{sw}x{sh} -> {dw}x{dh} (cached)");
        }
    }

    #[test]
    fn fused_luma_resize_matches_gray_then_resize() {
        let mut rng = Rng(99);
        for &(sw, sh, dw, dh) in &[(1920, 1080, 64, 48), (101, 75, 32, 16), (64, 48, 16, 16)] {
            let rgb: Vec<u8> = (0..sw * sh * 3).map(|_| rng.byte()).collect();
            let mut gray = vec![0u8; sw * sh];
            rgb_to_gray(&rgb, &mut gray);
            let mut want = vec![0u8; dw * dh];
            reference_resize_area_gray(&gray, sw, sh, &mut want, dw, dh);
            let mut got = vec![0u8; dw * dh];
            resize_area_luma_from_rgb(&rgb, sw, sh, &mut got, dw, dh);
            assert_eq!(got, want, "{sw}x{sh} -> {dw}x{dh}");
        }
    }

    #[test]
    fn bilinear_resize_copies_solid_rgb() {
        let src = [10u8, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30];
        let mut dst = [0u8; 12];
        resize_bilinear_rgb(&src, 2, 2, &mut dst, 2, 2);
        assert_eq!(dst, src);
    }

    #[test]
    fn bilinear_rgb_matches_per_channel_gray_resize() {
        let mut rng = Rng(5);
        let (sw, sh, dw, dh) = (97, 61, 48, 30);
        let rgb: Vec<u8> = (0..sw * sh * 3).map(|_| rng.byte()).collect();
        let mut got = vec![0u8; dw * dh * 3];
        resize_bilinear_rgb(&rgb, sw, sh, &mut got, dw, dh);
        for c in 0..3 {
            let plane: Vec<u8> = rgb.chunks_exact(3).map(|p| p[c]).collect();
            let mut want = vec![0u8; dw * dh];
            resize_bilinear_gray(&plane, sw, sh, &mut want, dw, dh);
            let got_plane: Vec<u8> = got.chunks_exact(3).map(|p| p[c]).collect();
            assert_eq!(got_plane, want, "channel {c}");
        }
    }
}
