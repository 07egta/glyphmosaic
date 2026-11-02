use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::path::PathBuf;

use crate::dict::{DictHeader, Dictionary};
use crate::fonts::{enumerate_codepoints, load_fonts};
use crate::raster::{features_from_coverage, rasterize_stretched};

pub struct BuildOptions {
    pub font_paths: Vec<PathBuf>,
    pub include_pua: bool,
    pub min_ink: u8,
    pub jobs: usize,
}

pub fn build(opts: &BuildOptions) -> Result<Dictionary> {
    if opts.font_paths.is_empty() {
        bail!("no fonts given; pass --fonts <glob>");
    }
    let fontset = load_fonts(&opts.font_paths, None)?;
    if fontset.fonts.is_empty() {
        bail!("no fonts loaded");
    }

    let mut pool_builder = rayon::ThreadPoolBuilder::new();
    if opts.jobs > 0 {
        pool_builder = pool_builder.num_threads(opts.jobs);
    }
    let pool = pool_builder
        .build()
        .context("failed to create worker thread pool")?;

    let header = DictHeader {
        version: crate::dict::format::VERSION,
        cell_feature: 16,
        coarse_feature: 4,
        unicode_version: "17.0".to_string(),
        fonts: fontset.fonts.iter().map(|f| f.record.clone()).collect(),
        created_by: format!("glyphmosaic {}", crate::VERSION),
        include_pua: opts.include_pua,
        min_ink: opts.min_ink,
        render_px: 128.0,
        stretch_px: 64,
    };
    let mut dict = Dictionary::new(header);

    for (font_index, loaded) in fontset.fonts.iter().enumerate() {
        let chars = enumerate_codepoints(&loaded.bytes, opts.include_pua)
            .with_context(|| format!("failed to enumerate {}", loaded.record.path))?;
        if chars.is_empty() {
            bail!("font {} has no usable glyphs", loaded.record.name);
        }

        let bar = ProgressBar::new(chars.len() as u64);
        bar.set_style(
            ProgressStyle::with_template("{spinner} {msg} {pos}/{len}")
                .unwrap()
                .progress_chars("=> "),
        );
        bar.set_message(loaded.record.name.clone());

        let results: Vec<(char, [u8; 256], [u8; 16])> = pool.install(|| {
            chars
                .par_iter()
                .filter_map(|&c| {
                    bar.inc(1);
                    let coverage = rasterize_stretched(&loaded.font, c, 128.0)?;
                    let max_ink = coverage.data.iter().copied().max().unwrap_or(0);
                    if max_ink < opts.min_ink {
                        return None;
                    }
                    let (full, coarse) = features_from_coverage(&coverage)?;
                    Some((c, full, coarse))
                })
                .collect()
        });
        bar.finish_and_clear();

        if results.is_empty() {
            bail!("font {} produced no glyph entries", loaded.record.name);
        }
        for (c, full, coarse) in results {
            dict.push(c, font_index as u16, full, coarse);
        }
    }

    if dict.is_empty() {
        bail!("dictionary is empty");
    }
    Ok(dict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_is_deterministic_and_nonempty() {
        let Some(path) = crate::test_util::test_font_path() else {
            return;
        };
        let opts = BuildOptions {
            font_paths: vec![path],
            include_pua: false,
            min_ink: 1,
            jobs: 2,
        };
        let a = build(&opts).unwrap();
        let b = build(&opts).unwrap();
        assert!(
            a.len() > 100,
            "font should produce many glyphs, got {}",
            a.len()
        );
        assert_eq!(a.codepoints, b.codepoints);
        assert_eq!(a.full, b.full);
        assert!(a.full.iter().any(|f| f.iter().any(|&v| v > 0)));
        assert_eq!(a.header.min_ink, 1);
        assert!(a.header.created_by.starts_with("glyphmosaic"));
    }
}
