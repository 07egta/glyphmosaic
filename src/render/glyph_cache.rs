use anyhow::{bail, Result};
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::fonts::LazyFont;
use crate::raster::{crop_ink, stretch_coverage, Coverage};

pub struct GlyphCache {
    fonts: Arc<Vec<LazyFont>>,
    cell_w: u32,
    cell_h: u32,
    cache: LruCache<(u16, u32), Arc<Vec<u8>>>,
}

impl GlyphCache {
    pub fn new(fonts: Arc<Vec<LazyFont>>, cell_w: u32, cell_h: u32, max_mb: usize) -> Self {
        // Glyphs are cached as ready-to-blit RGB, so one entry is 3 bytes per pixel.
        let cell_bytes = (cell_w as usize * cell_h as usize * 3).max(1);
        let entries = (max_mb * 1024 * 1024 / cell_bytes).max(1);
        Self {
            fonts,
            cell_w,
            cell_h,
            cache: LruCache::new(NonZeroUsize::new(entries).unwrap()),
        }
    }

    /// Parse any not-yet-loaded fonts among `indices`, in parallel when more
    /// than one is needed (the big CJK fonts take seconds each). Errors are not
    /// reported here; they are remembered and surface from [`GlyphCache::get`].
    pub fn preload(&self, indices: impl IntoIterator<Item = u16>) {
        let mut need: Vec<usize> = indices
            .into_iter()
            .map(usize::from)
            .filter(|&i| self.fonts.get(i).is_some_and(|f| !f.is_loaded()))
            .collect();
        need.sort_unstable();
        need.dedup();
        if need.len() > 1 {
            use rayon::prelude::*;
            need.par_iter().for_each(|&i| {
                let _ = self.fonts[i].get();
            });
        }
    }

    /// The glyph stretched to the cell, as RGB (`cell_w * cell_h * 3` bytes).
    pub fn get(&mut self, font_index: u16, cp: u32) -> Result<Arc<Vec<u8>>> {
        if let Some(hit) = self.cache.get(&(font_index, cp)) {
            return Ok(hit.clone());
        }
        let font = self
            .fonts
            .get(font_index as usize)
            .ok_or_else(|| {
                anyhow::anyhow!("dictionary references missing font index {font_index}")
            })?
            .get()?;
        let ch =
            char::from_u32(cp).ok_or_else(|| anyhow::anyhow!("invalid codepoint U+{cp:04X}"))?;
        let (metrics, bitmap) = font.rasterize(ch, self.cell_h as f32);
        if metrics.width == 0 || metrics.height == 0 {
            bail!("glyph U+{cp:04X} has no outline");
        }
        let (x, y, w, h) = crop_ink(&bitmap, metrics.width, metrics.height)
            .ok_or_else(|| anyhow::anyhow!("glyph U+{cp:04X} has no ink"))?;
        let mut cropped = Coverage::new(w, h);
        for row in 0..h {
            let start = (y + row) * metrics.width + x;
            cropped.data[row * w..(row + 1) * w].copy_from_slice(&bitmap[start..start + w]);
        }
        let stretched = stretch_coverage(&cropped, self.cell_w as usize, self.cell_h as usize);
        let mut rgb = Vec::with_capacity(stretched.data.len() * 3);
        for &v in &stretched.data {
            rgb.extend_from_slice(&[v, v, v]);
        }
        let arc = Arc::new(rgb);
        self.cache.put((font_index, cp), arc.clone());
        Ok(arc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn caches_same_glyph() {
        let Some(path) = crate::test_util::test_font_path() else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let fonts = Arc::new(vec![font.into()]);
        let mut cache = GlyphCache::new(fonts, 32, 32, 8);
        let a = cache.get(0, 'A' as u32).unwrap();
        let b = cache.get(0, 'A' as u32).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.len(), 32 * 32 * 3);
        assert!(a.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]));
    }
}
