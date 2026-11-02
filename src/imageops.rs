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

/// Area-average downscale (box filter). Works for upscale too, where it
/// degenerates to a nearest-ish sample.
pub fn resize_area_gray(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    assert_eq!(src.len(), sw * sh);
    assert_eq!(dst.len(), dw * dh);
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

fn bilinear_axis(src_len: usize, dst_len: usize) -> Vec<(usize, usize, f64)> {
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
    for (y, &(y0, y1, ty)) in ys.iter().enumerate() {
        for (x, &(x0, x1, tx)) in xs.iter().enumerate() {
            let p00 = (y0 * sw + x0) * 3;
            let p01 = (y0 * sw + x1) * 3;
            let p10 = (y1 * sw + x0) * 3;
            let p11 = (y1 * sw + x1) * 3;
            let out = (y * dw + x) * 3;
            for c in 0..3 {
                let a = src[p00 + c] as f64;
                let b = src[p01 + c] as f64;
                let c0 = src[p10 + c] as f64;
                let d = src[p11 + c] as f64;
                let top = a + (b - a) * tx;
                let bottom = c0 + (d - c0) * tx;
                let v = top + (bottom - top) * ty;
                dst[out + c] = (v.clamp(0.0, 255.0) + 0.5).floor() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn bilinear_resize_copies_solid_rgb() {
        let src = [10u8, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30];
        let mut dst = [0u8; 12];
        resize_bilinear_rgb(&src, 2, 2, &mut dst, 2, 2);
        assert_eq!(dst, src);
    }
}
