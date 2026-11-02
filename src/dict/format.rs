use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::fonts::FontRecord;
use crate::matching::ivf::{index_path, IvfIndex};

pub const MAGIC: &[u8; 4] = b"GMD1";
pub const VERSION: u32 = 1;
pub const ENTRY_SIZE: usize = 4 + 2 + 2 + 256 + 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DictHeader {
    pub version: u32,
    pub cell_feature: u16,
    pub coarse_feature: u16,
    pub unicode_version: String,
    pub fonts: Vec<FontRecord>,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub include_pua: bool,
    #[serde(default = "default_min_ink")]
    pub min_ink: u8,
    #[serde(default = "default_render_px")]
    pub render_px: f32,
    #[serde(default = "default_stretch_px")]
    pub stretch_px: u16,
}

fn default_min_ink() -> u8 {
    1
}
fn default_render_px() -> f32 {
    128.0
}
fn default_stretch_px() -> u16 {
    64
}

impl Default for DictHeader {
    fn default() -> Self {
        Self {
            version: VERSION,
            cell_feature: 16,
            coarse_feature: 4,
            unicode_version: "17.0".to_string(),
            fonts: Vec::new(),
            created_by: format!("glyphmosaic {}", crate::VERSION),
            include_pua: false,
            min_ink: 1,
            render_px: 128.0,
            stretch_px: 64,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dictionary {
    pub header: DictHeader,
    pub codepoints: Vec<u32>,
    pub font_indices: Vec<u16>,
    pub full: Vec<[u8; 256]>,
    pub coarse: Vec<[u8; 16]>,
    /// Runtime-only 8×8 downsample of `full`, used by the middle match stage.
    pub coarse8: Vec<[u8; 64]>,
    /// Optional IVF index loaded from `<dict>.idx`; `None` means full scan.
    pub index: Option<IvfIndex>,
}

impl Dictionary {
    pub fn new(header: DictHeader) -> Self {
        Self {
            header,
            codepoints: Vec::new(),
            font_indices: Vec::new(),
            full: Vec::new(),
            coarse: Vec::new(),
            coarse8: Vec::new(),
            index: None,
        }
    }

    pub fn len(&self) -> usize {
        self.codepoints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codepoints.is_empty()
    }

    pub fn push(&mut self, cp: char, font_index: u16, full: [u8; 256], coarse: [u8; 16]) {
        self.codepoints.push(cp as u32);
        self.font_indices.push(font_index);
        self.coarse8.push(crate::raster::coarse8_from_full(&full));
        self.full.push(full);
        self.coarse.push(coarse);
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let header_json = serde_json::to_vec(&self.header)?;
        let mut out = BufWriter::new(
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?,
        );
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&(header_json.len() as u32).to_le_bytes())?;
        out.write_all(&(self.len() as u32).to_le_bytes())?;
        out.write_all(&header_json)?;
        for i in 0..self.len() {
            out.write_all(&self.codepoints[i].to_le_bytes())?;
            out.write_all(&self.font_indices[i].to_le_bytes())?;
            out.write_all(&0u16.to_le_bytes())?;
            out.write_all(&self.full[i])?;
            out.write_all(&self.coarse[i])?;
        }
        out.flush()?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let mut input = BufReader::new(
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?,
        );
        let mut magic = [0u8; 4];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            bail!(
                "bad dictionary magic: expected {}, got {}",
                String::from_utf8_lossy(MAGIC),
                String::from_utf8_lossy(&magic)
            );
        }
        let version = read_u32(&mut input)?;
        if version != VERSION {
            bail!("unsupported dictionary version: {version}");
        }
        let header_len = read_u32(&mut input)? as usize;
        let count = read_u32(&mut input)? as usize;

        let file_size = std::fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .len() as usize;
        let expected = 16usize
            .checked_add(header_len)
            .and_then(|v| v.checked_add(count.checked_mul(ENTRY_SIZE)?))
            .ok_or_else(|| anyhow::anyhow!("dictionary size overflow"))?;
        if expected != file_size {
            bail!(
                "dictionary size mismatch: header and {count} entries need {expected} bytes, file has {file_size}"
            );
        }
        let mut header_json = vec![0u8; header_len];
        input.read_exact(&mut header_json)?;
        let header: DictHeader =
            serde_json::from_slice(&header_json).context("failed to parse dictionary header")?;
        if header.cell_feature as usize != 16 || header.coarse_feature as usize != 4 {
            bail!(
                "dictionary is incompatible: features {}×{}, expected 16×16 and 4×4",
                header.cell_feature,
                header.coarse_feature
            );
        }
        if header.version != VERSION {
            bail!(
                "dictionary is incompatible: header version {}, expected {}",
                header.version,
                VERSION
            );
        }

        let mut dict = Dictionary::new(header);
        dict.codepoints.reserve(count);
        dict.font_indices.reserve(count);
        dict.full.reserve(count);
        dict.coarse.reserve(count);
        dict.coarse8.reserve(count);
        for _ in 0..count {
            let cp = read_u32(&mut input)?;
            let font_index = read_u16(&mut input)?;
            let _reserved = read_u16(&mut input)?;
            let mut full = [0u8; 256];
            input.read_exact(&mut full)?;
            let mut coarse = [0u8; 16];
            input.read_exact(&mut coarse)?;
            dict.codepoints.push(cp);
            dict.font_indices.push(font_index);
            dict.coarse8.push(crate::raster::coarse8_from_full(&full));
            dict.full.push(full);
            dict.coarse.push(coarse);
        }

        let mut extra = [0u8; 1];
        if input.read(&mut extra)? != 0 {
            bail!("dictionary has trailing bytes after {count} entries");
        }
        dict.index = IvfIndex::read(&index_path(path), dict.len()).ok();
        Ok(dict)
    }
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Dictionary {
        let header = DictHeader {
            version: 1,
            cell_feature: 16,
            coarse_feature: 4,
            unicode_version: "17.0".into(),
            fonts: vec![crate::fonts::FontRecord {
                path: "fonts/Jigmo.ttf".into(),
                name: "Jigmo".into(),
                sha256: "00".repeat(32),
            }],
            ..DictHeader::default()
        };
        let mut d = Dictionary::new(header);
        d.push('A', 0, [1u8; 256], [2u8; 16]);
        d.push('あ', 0, [3u8; 256], [4u8; 16]);
        d
    }

    #[test]
    fn roundtrips_dictionary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.gmd");
        let a = sample();
        a.write(&path).unwrap();
        let b = Dictionary::read(&path).unwrap();
        assert_eq!(b.codepoints, a.codepoints);
        assert_eq!(b.font_indices, a.font_indices);
        assert_eq!(b.full, a.full);
        assert_eq!(b.coarse, a.coarse);
        assert_eq!(b.header.fonts, a.header.fonts);
        assert_eq!(b.header.created_by, a.header.created_by);
        assert_eq!(b.header.min_ink, 1);
        assert_eq!(b.header.stretch_px, 64);
    }

    #[test]
    fn rejects_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.gmd");
        std::fs::write(&path, b"NOPE").unwrap();
        assert!(Dictionary::read(&path)
            .unwrap_err()
            .to_string()
            .contains("magic"));
    }

    #[test]
    fn rejects_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.gmd");
        sample().write(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(Dictionary::read(&path)
            .unwrap_err()
            .to_string()
            .contains("size mismatch"));
    }

    #[test]
    fn empty_dictionary_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.gmd");
        let mut d = sample();
        d.codepoints.clear();
        d.font_indices.clear();
        d.full.clear();
        d.coarse.clear();
        d.coarse8.clear();
        d.write(&path).unwrap();
        assert_eq!(Dictionary::read(&path).unwrap().len(), 0);
    }
}
