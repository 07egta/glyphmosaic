use crate::dict::Dictionary;
use crate::imageops::resize_bilinear_rgb;
use crate::matching::MatchResult;
use crate::render::text::{draw_text, text_width};

fn fill_rect(canvas: &mut [u8], cw: u32, ch: u32, x: u32, y: u32, w: u32, h: u32, color: [u8; 3]) {
    for py in y..(y + h).min(ch) {
        for px in x..(x + w).min(cw) {
            let i = (py as usize * cw as usize + px as usize) * 3;
            canvas[i..i + 3].copy_from_slice(&color);
        }
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
    src_w: u32,
    src_h: u32,
    dict: &Dictionary,
    matches: &[MatchResult],
) {
    assert_eq!(canvas.len(), cw as usize * ch as usize * 3);
    if w == 0 || h == 0 || src_w == 0 || src_h == 0 {
        return;
    }

    fill_rect(canvas, cw, ch, x, y, w, h, [10, 10, 14]);

    // Preview keeps the source aspect ratio and is capped to 60% of the panel.
    let max_prev_h = ((h as f64 * 0.6) as u32 & !1).max(2);
    let mut prev_w = w & !1;
    let mut prev_h = (((prev_w as f64 * src_h as f64 / src_w as f64).round() as u32) & !1).max(2);
    if prev_h > max_prev_h {
        prev_h = max_prev_h;
        prev_w = (((prev_h as f64 * src_w as f64 / src_h as f64).round() as u32) & !1).max(2);
    }
    if frame.len() == src_w as usize * src_h as usize * 3 {
        let mut preview = vec![0u8; prev_w as usize * prev_h as usize * 3];
        resize_bilinear_rgb(
            frame,
            src_w as usize,
            src_h as usize,
            &mut preview,
            prev_w as usize,
            prev_h as usize,
        );
        let px = x + (w.saturating_sub(prev_w)) / 2;
        for row in 0..prev_h {
            for col in 0..prev_w {
                let s = ((row * prev_w + col) * 3) as usize;
                let d = (((y + row) * cw + px + col) * 3) as usize;
                canvas[d..d + 3].copy_from_slice(&preview[s..s + 3]);
            }
        }
    }

    if matches.is_empty() {
        return;
    }
    let cols = 2u32;
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
        let ty = start_y
            + (i as u32 / cols) * line_h
            + (line_h.saturating_sub(7 * scale)) / 2;
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
