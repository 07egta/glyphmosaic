use anyhow::{bail, Result};
use std::sync::Arc;

use crate::dict::Dictionary;
use crate::fonts::LazyFont;
use crate::matching::MatchResult;
use glyph_cache::GlyphCache;

pub mod glyph_cache;
pub mod panel;
pub mod text;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Mosaic,
    Panel,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderConfig {
    pub grid_cols: usize,
    pub grid_rows: usize,
    pub cell_height: u32,
    pub layout: Layout,
    pub panel_width: u32,
    pub cache_mb: usize,
}

pub struct Compositor {
    config: RenderConfig,
    src_w: u32,
    src_h: u32,
    canvas_w: u32,
    canvas_h: u32,
    cell_w: u32,
    cell_h: u32,
    mosaic_w: u32,
    mosaic_h: u32,
    canvas: Vec<u8>,
    cache: GlyphCache,
    panel: Option<panel::PanelPreview>,
}

impl Compositor {
    pub fn new(
        config: RenderConfig,
        fonts: Arc<Vec<LazyFont>>,
        src_w: u32,
        src_h: u32,
    ) -> Result<Self> {
        if config.grid_cols == 0 || config.grid_rows == 0 {
            bail!("grid must have at least one row and column");
        }
        if src_w == 0 || src_h == 0 {
            bail!("source dimensions must be non-zero");
        }
        if config.cell_height < 2 {
            bail!("cell height must be at least 2");
        }

        let cell_h = (config.cell_height & !1).max(2);
        let aspect =
            (src_w as f64 / config.grid_cols as f64) / (src_h as f64 / config.grid_rows as f64);
        let cell_w = (((cell_h as f64 * aspect).round() as u32) & !1).max(2);
        let mosaic_w = cell_w * config.grid_cols as u32;
        let mosaic_h = cell_h * config.grid_rows as u32;
        let panel_w = match config.layout {
            Layout::Panel => (config.panel_width & !1).max(2),
            Layout::Mosaic => 0,
        };
        let canvas_w = mosaic_w + panel_w;
        let canvas_h = mosaic_h;

        let cache = GlyphCache::new(fonts, cell_w, cell_h, config.cache_mb);
        let panel = (config.layout == Layout::Panel)
            .then(|| panel::PanelPreview::new(panel_w, mosaic_h, src_w, src_h));
        Ok(Self {
            config,
            src_w,
            src_h,
            canvas_w,
            canvas_h,
            cell_w,
            cell_h,
            mosaic_w,
            mosaic_h,
            canvas: vec![0; canvas_w as usize * canvas_h as usize * 3],
            cache,
            panel,
        })
    }

    pub fn canvas_size(&self) -> (u32, u32) {
        (self.canvas_w, self.canvas_h)
    }

    pub fn compose(
        &mut self,
        frame: &[u8],
        matches: &[MatchResult],
        dict: &Dictionary,
        _frame_no: u64,
    ) -> Result<&[u8]> {
        let expected_frame = self.src_w as usize * self.src_h as usize * 3;
        if frame.len() != expected_frame {
            bail!(
                "frame buffer is {} bytes, expected {}",
                frame.len(),
                expected_frame
            );
        }
        let expected_cells = self.config.grid_cols * self.config.grid_rows;
        if matches.len() != expected_cells {
            bail!("got {} matches, expected {}", matches.len(), expected_cells);
        }

        // No clearing pass: the glyph cells tile the whole mosaic area and the
        // panel (if any) paints the rest, so every canvas byte is written below.
        self.cache.preload(
            matches
                .iter()
                .filter_map(|r| dict.font_indices.get(r.index).copied()),
        );
        let stride = self.canvas_w as usize * 3;
        let row_bytes = self.cell_w as usize * 3;
        for (i, result) in matches.iter().enumerate() {
            let cx = i % self.config.grid_cols;
            let cy = i / self.config.grid_cols;
            if result.index == crate::matching::BLANK {
                // Empty cell: plain black.
                let ox = cx * self.cell_w as usize * 3;
                let oy = cy * self.cell_h as usize;
                for y in 0..self.cell_h as usize {
                    let start = (oy + y) * stride + ox;
                    self.canvas[start..start + row_bytes].fill(0);
                }
                continue;
            }
            if result.index >= dict.len() {
                bail!("match index {} is outside the dictionary", result.index);
            }
            let font_index = dict.font_indices[result.index];
            let cp = dict.codepoints[result.index];
            let bitmap = self.cache.get(font_index, cp)?;
            if bitmap.len() != row_bytes * self.cell_h as usize {
                bail!(
                    "glyph bitmap is {} bytes, expected {}",
                    bitmap.len(),
                    row_bytes * self.cell_h as usize
                );
            }
            let ox = cx * self.cell_w as usize * 3;
            let oy = cy * self.cell_h as usize;
            for (y, src_row) in bitmap.chunks_exact(row_bytes).enumerate() {
                let start = (oy + y) * stride + ox;
                self.canvas[start..start + row_bytes].copy_from_slice(src_row);
            }
        }

        if let Some(preview) = self.panel.as_mut() {
            panel::draw_panel(
                &mut self.canvas,
                self.canvas_w,
                self.canvas_h,
                self.mosaic_w,
                0,
                self.canvas_w - self.mosaic_w,
                self.mosaic_h,
                frame,
                preview,
                dict,
                matches,
            );
        }
        Ok(&self.canvas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictHeader;

    #[test]
    fn builds_even_canvas_for_4x3_on_4x3() {
        let Some(path) = crate::test_util::test_font_path() else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let cfg = RenderConfig {
            grid_cols: 4,
            grid_rows: 3,
            cell_height: 240,
            layout: Layout::Mosaic,
            panel_width: 480,
            cache_mb: 16,
        };
        let c = Compositor::new(cfg, Arc::new(vec![font.into()]), 640, 480).unwrap();
        assert_eq!(c.canvas_size(), (960, 720));
    }

    #[test]
    fn odd_source_still_gives_even_canvas() {
        let Some(path) = crate::test_util::test_font_path() else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let cfg = RenderConfig {
            grid_cols: 4,
            grid_rows: 3,
            cell_height: 241,
            layout: Layout::Panel,
            panel_width: 481,
            cache_mb: 16,
        };
        let c = Compositor::new(cfg, Arc::new(vec![font.into()]), 101, 75).unwrap();
        let (w, h) = c.canvas_size();
        assert_eq!((w % 2, h % 2), (0, 0));
    }

    #[test]
    fn composes_all_black_frame_without_panicking() {
        let Some(path) = crate::test_util::test_font_path() else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let mut dict = Dictionary::new(DictHeader {
            version: 1,
            cell_feature: 16,
            coarse_feature: 4,
            unicode_version: "17.0".into(),
            fonts: vec![],
            ..DictHeader::default()
        });
        dict.push('A', 0, [255u8; 256], [255u8; 16]);
        let cfg = RenderConfig {
            grid_cols: 4,
            grid_rows: 3,
            cell_height: 24,
            layout: Layout::Mosaic,
            panel_width: 80,
            cache_mb: 8,
        };
        let mut c = Compositor::new(cfg, Arc::new(vec![font.into()]), 64, 48).unwrap();
        let frame = vec![0u8; 64 * 48 * 3];
        let matches = vec![MatchResult { index: 0, mse: 0 }; 12];
        let out = c.compose(&frame, &matches, &dict, 0).unwrap();
        assert_eq!(out.len(), 96 * 72 * 3);
        assert!(out.chunks(3).any(|p| p[0] > 0));
    }
}
