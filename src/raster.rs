use crate::imageops::resize_bilinear_gray;

pub const FEATURE_SIDE: usize = 16;
pub const COARSE_SIDE: usize = 4;
pub const FULL_LEN: usize = FEATURE_SIDE * FEATURE_SIDE;
pub const COARSE_LEN: usize = COARSE_SIDE * COARSE_SIDE;

#[derive(Clone, Debug)]
pub struct Coverage {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Coverage {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0; width * height],
        }
    }
}

/// Downsample a 16×16 feature to 8×8 by averaging 2×2 blocks.
pub fn coarse8_from_full(full: &[u8; FULL_LEN]) -> [u8; 64] {
    let mut out = [0u8; 64];
    for cy in 0..8 {
        for cx in 0..8 {
            let mut sum = 0u32;
            for y in 0..2 {
                for x in 0..2 {
                    sum += full[(cy * 2 + y) * FEATURE_SIDE + cx * 2 + x] as u32;
                }
            }
            out[cy * 8 + cx] = ((sum + 2) / 4) as u8;
        }
    }
    out
}

/// Returns `(x, y, width, height)` of the non-zero ink bounding box.
pub fn crop_ink(data: &[u8], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    assert_eq!(data.len(), width * height);
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            if data[y * width + x] != 0 {
                found = true;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    if !found {
        return None;
    }
    Some((min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
}

/// Bilinear stretch of a coverage bitmap into `dw × dh`.
pub fn stretch_coverage(src: &Coverage, dw: usize, dh: usize) -> Coverage {
    let mut out = Coverage::new(dw, dh);
    resize_bilinear_gray(
        src.data.as_slice(),
        src.width,
        src.height,
        &mut out.data,
        dw,
        dh,
    );
    out
}

/// Box-downsample any coverage into the 16×16 full feature grid, then
/// average 4×4 blocks into the 4×4 coarse grid.
pub fn features_from_coverage(cov: &Coverage) -> Option<([u8; FULL_LEN], [u8; COARSE_LEN])> {
    if !cov.data.iter().any(|&v| v != 0) {
        return None;
    }
    let mut full = [0u8; FULL_LEN];
    if cov.width == FEATURE_SIDE && cov.height == FEATURE_SIDE {
        full.copy_from_slice(&cov.data);
    } else {
        crate::imageops::resize_area_gray(
            &cov.data,
            cov.width,
            cov.height,
            &mut full,
            FEATURE_SIDE,
            FEATURE_SIDE,
        );
    }

    let mut coarse = [0u8; COARSE_LEN];
    for cy in 0..COARSE_SIDE {
        for cx in 0..COARSE_SIDE {
            let mut sum = 0u32;
            for y in 0..4 {
                for x in 0..4 {
                    sum += full[(cy * 4 + y) * FEATURE_SIDE + cx * 4 + x] as u32;
                }
            }
            coarse[cy * COARSE_SIDE + cx] = ((sum + 8) / 16) as u8;
        }
    }
    Some((full, coarse))
}

/// Rasterize one character, crop to ink, and stretch it to fill a 64×64 cell.
/// This is the "stretch the glyph to the whole cell" step of the technique.
pub fn rasterize_stretched(font: &fontdue::Font, c: char, px: f32) -> Option<Coverage> {
    let (metrics, bitmap) = font.rasterize(c, px);
    if metrics.width == 0 || metrics.height == 0 {
        return None;
    }
    let (x, y, w, h) = crop_ink(&bitmap, metrics.width, metrics.height)?;
    let mut cropped = Coverage::new(w, h);
    for row in 0..h {
        let src = &bitmap[(y + row) * metrics.width + x..(y + row) * metrics.width + x + w];
        cropped.data[row * w..(row + 1) * w].copy_from_slice(src);
    }
    Some(stretch_coverage(&cropped, 64, 64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_trims_transparent_border() {
        let mut b = vec![0u8; 5 * 4];
        b[6] = 255;
        b[7] = 128;
        assert_eq!(crop_ink(&b, 5, 4), Some((1, 1, 2, 1)));
    }

    #[test]
    fn features_preserve_left_right_split() {
        let mut data = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..8 {
                data[y * 16 + x] = 255;
            }
        }
        let (full, coarse) = features_from_coverage(&Coverage {
            width: 16,
            height: 16,
            data,
        })
        .unwrap();
        assert_eq!(full[0], 255);
        assert_eq!(full[15], 0);
        assert_eq!(coarse[0], 255); // top-left 4x4
        assert_eq!(coarse[3], 0); // top-right 4x4
    }

    #[test]
    fn stretch_keeps_corner_values() {
        let src = Coverage {
            width: 2,
            height: 2,
            data: vec![255, 0, 0, 0],
        };
        let out = stretch_coverage(&src, 4, 4);
        assert_eq!(out.width, 4);
        assert!(out.data[0] > 200);
        assert!(out.data[3] < 40);
    }
}
