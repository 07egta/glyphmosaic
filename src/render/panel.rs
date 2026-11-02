use crate::dict::Dictionary;
use crate::imageops::{bilinear_axis, resize_bilinear_rgb_with, BilinearAxis};
use crate::matching::MatchResult;
use crate::render::text::{draw_text, text_width};

#[allow(clippy::too_many_arguments)]
fn fill_rect(canvas: &mut [u8], cw: u32, ch: u32, x: u32, y: u32, w: u32, h: u32, color: [u8; 3]) {
    let x_end = (x + w).min(cw);
    if x >= x_end {
        return;
    }
    for py in y..(y + h).min(ch) {
        let start = (py as usize * cw as usize + x as usize) * 3;
        let end = (py as usize * cw as usize + x_end as usize) * 3;
        for px in canvas[start..end].chunks_exact_mut(3) {
            px.copy_from_slice(&color);
        }
    }
}

/// The scaled-down source frame shown at the top of the panel.
///
/// Its size and the resampling taps depend only on the panel and source sizes,
/// so they are worked out once and the scratch buffer is reused every frame.
pub struct PanelPreview {
    src_w: u32,
    src_h: u32,
    prev_w: u32,
    prev_h: u32,
    xs: BilinearAxis,
    ys: BilinearAxis,
    buf: Vec<u8>,
}

impl PanelPreview {
    pub fn new(panel_w: u32, panel_h: u32, src_w: u32, src_h: u32) -> Self {
        // Preview keeps the source aspect ratio and is capped to 60% of the panel.
        let (w, h) = (panel_w, panel_h);
        let max_prev_h = ((h as f64 * 0.6) as u32 & !1).max(2);
        let mut prev_w = w & !1;
        let mut prev_h =
            (((prev_w as f64 * src_h as f64 / src_w as f64).round() as u32) & !1).max(2);
        if prev_h > max_prev_h {
            prev_h = max_prev_h;
            prev_w = (((prev_h as f64 * src_w as f64 / src_h as f64).round() as u32) & !1).max(2);
        }
        let (xs, ys) = if src_w > 0 && src_h > 0 {
            (
                bilinear_axis(src_w as usize, prev_w as usize),
                bilinear_axis(src_h as usize, prev_h as usize),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        Self {
            src_w,
            src_h,
            prev_w,
            prev_h,
            xs,
            ys,
            buf: vec![0u8; prev_w as usize * prev_h as usize * 3],
        }
    }

    fn render(&mut self, frame: &[u8]) -> &[u8] {
        resize_bilinear_rgb_with(
            frame,
            self.src_w as usize,
            &self.xs,
            &self.ys,
            &mut self.buf,
        );
        &self.buf
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_panel(
    canvas: &mut [u8],
    cw: u32,
    ch: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    frame: &[u8],
    preview: &mut PanelPreview,
    dict: &Dictionary,
    matches: &[MatchResult],
) {
    assert_eq!(canvas.len(), cw as usize * ch as usize * 3);
    if w == 0 || h == 0 || preview.src_w == 0 || preview.src_h == 0 {
        return;
    }

    fill_rect(canvas, cw, ch, x, y, w, h, [10, 10, 14]);

    let (prev_w, prev_h) = (preview.prev_w, preview.prev_h);
    if frame.len() == preview.src_w as usize * preview.src_h as usize * 3 {
        let scaled = preview.render(frame);
        let px = x + (w.saturating_sub(prev_w)) / 2;
        let row_bytes = prev_w as usize * 3;
        for (row, src_row) in scaled.chunks_exact(row_bytes).enumerate() {
            let d = ((y as usize + row) * cw as usize + px as usize) * 3;
            canvas[d..d + row_bytes].copy_from_slice(src_row);
        }
    }

    if matches.is_empty() {
        return;
    }
    let cols = if matches.len() <= 12 {
        2u32
    } else if matches.len() <= 48 {
        3u32
    } else {
        4u32
    };
    let rows = (matches.len() as u32).div_ceil(cols);
    let start_y = y + prev_h + 10;
    let available = (y + h).saturating_sub(start_y + 6);
    let line_h = (available / rows).max(1);
    let col_w = w / cols;
    let base_scale = (line_h / 9).clamp(1, 4);

    for (i, result) in matches.iter().enumerate() {
        let Some(&cp) = dict.codepoints.get(result.index) else {
            continue;
        };
        let label = format!("U+{cp:04X}");
        let mut scale = base_scale;
        while scale > 1 && text_width(&label, scale) > col_w.saturating_sub(8) {
            scale -= 1;
        }
        let tx = x + (i as u32 % cols) * col_w + 4;
        let ty = start_y + (i as u32 / cols) * line_h + (line_h.saturating_sub(7 * scale)) / 2;
        draw_text(
            canvas,
            cw,
            ch,
            tx as i32,
            ty as i32,
            scale,
            &label,
            [230, 230, 230],
        );
    }
}
