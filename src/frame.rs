use anyhow::{bail, Result};

use crate::imageops::{resize_area_gray, rgb_to_gray};
use crate::matching::Query;
use crate::raster::{features_from_coverage, Coverage, FEATURE_SIDE};

pub fn queries_from_rgb(
    rgb: &[u8],
    sw: usize,
    sh: usize,
    cols: usize,
    rows: usize,
) -> Result<Vec<Query>> {
    if cols == 0 || rows == 0 {
        bail!("grid must have at least one row and column");
    }
    if rgb.len() != sw * sh * 3 {
        bail!(
            "frame buffer is {} bytes, expected {} for {}x{} RGB",
            rgb.len(),
            sw * sh * 3,
            sw,
            sh
        );
    }

    let mut gray = vec![0u8; sw * sh];
    rgb_to_gray(rgb, &mut gray);

    let dw = cols * FEATURE_SIDE;
    let dh = rows * FEATURE_SIDE;
    let mut small = vec![0u8; dw * dh];
    resize_area_gray(&gray, sw, sh, &mut small, dw, dh);

    let mut out = Vec::with_capacity(cols * rows);
    for cy in 0..rows {
        for cx in 0..cols {
            let mut cell = Coverage::new(FEATURE_SIDE, FEATURE_SIDE);
            for y in 0..FEATURE_SIDE {
                let start = (cy * FEATURE_SIDE + y) * dw + cx * FEATURE_SIDE;
                let src = &small[start..start + FEATURE_SIDE];
                cell.data[y * FEATURE_SIDE..(y + 1) * FEATURE_SIDE].copy_from_slice(src);
            }
            let (full, coarse) =
                features_from_coverage(&cell).unwrap_or(([0u8; 256], [0u8; 16]));
            out.push(Query { full, coarse });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_white_right_black_is_two_queries() {
        let w = 32u32;
        let h = 32u32;
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..(w / 2) {
                let i = ((y * w + x) * 3) as usize;
                rgb[i..i + 3].fill(255);
            }
        }
        let q = queries_from_rgb(&rgb, w as usize, h as usize, 2, 1).unwrap();
        assert_eq!(q.len(), 2);
        assert!(q[0].full.iter().all(|&v| v >= 250));
        assert!(q[1].full.iter().all(|&v| v <= 5));
        assert!(q[0].coarse.iter().all(|&v| v >= 250));
    }
}
