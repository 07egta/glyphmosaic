use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use unicode_general_category::{get_general_category, GeneralCategory};

use crate::error::GmError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FontRecord {
    pub path: String,
    pub name: String,
    pub sha256: String,
}

pub struct LoadedFont {
    pub record: FontRecord,
    pub bytes: Vec<u8>,
    pub font: fontdue::Font,
}

pub struct FontSet {
    pub fonts: Vec<LoadedFont>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        write!(out, "{b:02x}").unwrap();
    }
    out
}

pub fn is_usable_char(c: char, include_pua: bool) -> bool {
    let cp = c as u32;
    if cp & 0xFFFE == 0xFFFE {
        return false;
    }
    if (0xFE00..=0xFE0F).contains(&cp) || (0xE0100..=0xE01EF).contains(&cp) {
        return false;
    }
    match get_general_category(c) {
        GeneralCategory::Control
        | GeneralCategory::Format
        | GeneralCategory::Surrogate
        | GeneralCategory::Unassigned
        | GeneralCategory::LineSeparator
        | GeneralCategory::ParagraphSeparator
        | GeneralCategory::SpaceSeparator => false,
        GeneralCategory::PrivateUse => include_pua,
        _ => true,
    }
}

pub fn enumerate_codepoints(bytes: &[u8], include_pua: bool) -> Result<Vec<char>> {
    let face = ttf_parser::Face::parse(bytes, 0).context("failed to parse font")?;
    let cmap = face
        .tables()
        .cmap
        .ok_or_else(|| anyhow!("font has no cmap table"))?;
    let mut set = BTreeSet::new();
    for subtable in cmap.subtables {
        if !subtable.is_unicode() {
            continue;
        }
        subtable.codepoints(|cp| {
            if let Some(c) = char::from_u32(cp) {
                if face.glyph_index(c).is_some() && is_usable_char(c, include_pua) {
                    set.insert(cp);
                }
            }
        });
    }
    Ok(set
        .into_iter()
        .filter_map(char::from_u32)
        .collect::<Vec<_>>())
}

fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("ttf" | "otf" | "ttc")
    )
}

fn wildcard_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn collect_dir_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir_recursive(&path, out);
        } else if is_font_file(&path) {
            out.push(path);
        }
    }
}

fn expand_one(pattern: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    let path = Path::new(pattern);
    if path.exists() {
        if path.is_dir() {
            collect_dir_recursive(path, out);
        } else {
            out.push(path.to_path_buf());
        }
        return Ok(());
    }

    let normalized = pattern.replace('\\', "/");
    if let Some(idx) = normalized.find("**") {
        let prefix = &normalized[..idx];
        let suffix = normalized[idx + 2..].trim_start_matches('/');
        let prefix_path = Path::new(prefix);
        if !prefix_path.exists() {
            return Ok(());
        }
        let mut dirs = vec![prefix_path.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if is_font_file(&path)
                    && wildcard_match(suffix, &entry.file_name().to_string_lossy())
                {
                    out.push(path);
                }
            }
        }
        return Ok(());
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_pattern = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if parent.exists() {
        for entry in std::fs::read_dir(parent).with_context(|| {
            format!("failed to read directory {}", parent.display())
        })? {
            let entry = entry?;
            if wildcard_match(&file_pattern, &entry.file_name().to_string_lossy()) {
                out.push(entry.path());
            }
        }
    }
    Ok(())
}

pub fn collect_font_paths(globs: &[String], root: Option<&Path>) -> Result<Vec<PathBuf>> {
    if globs.is_empty() {
        bail!("no fonts given; pass --fonts <glob>");
    }
    let mut out = Vec::new();
    for g in globs {
        let pattern = match root {
            Some(root) => root.join(g).to_string_lossy().into_owned(),
            None => g.clone(),
        };
        let before = out.len();
        expand_one(&pattern, &mut out)?;
        if out.len() == before {
            bail!("no font files matched pattern: {pattern}");
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn font_name(face: &ttf_parser::Face, path: &Path) -> String {
    face.names()
        .get(ttf_parser::name_id::FULL_NAME)
        .and_then(|n| n.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "font".to_string())
        })
}

pub fn load_fonts(paths: &[PathBuf], root: Option<&Path>) -> Result<FontSet> {
    let mut fonts = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read font {}", path.display()))?;
        let sha256 = sha256_hex(&bytes);
        let face = ttf_parser::Face::parse(&bytes, 0)
            .with_context(|| format!("failed to parse font {}", path.display()))?;
        let name = font_name(&face, path);
        let font = fontdue::Font::from_bytes(bytes.clone(), fontdue::FontSettings::default())
            .map_err(|e| anyhow!("failed to load font {}: {e}", path.display()))?;
        let stored_path = match root {
            Some(root) => path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned(),
            None => path.to_string_lossy().into_owned(),
        };
        fonts.push(LoadedFont {
            record: FontRecord {
                path: stored_path,
                name,
                sha256,
            },
            bytes,
            font,
        });
    }
    Ok(FontSet { fonts })
}

impl FontSet {
    pub fn load_for_dictionary(
        infos: &[FontRecord],
        override_globs: &[String],
        root: Option<&Path>,
    ) -> Result<Self> {
        let overrides = if override_globs.is_empty() {
            Vec::new()
        } else {
            collect_font_paths(override_globs, root)?
        };

        let mut by_hash: HashMap<String, PathBuf> = HashMap::new();
        for path in overrides {
            if let Ok(bytes) = std::fs::read(&path) {
                by_hash.entry(sha256_hex(&bytes)).or_insert(path);
            }
        }

        let mut fonts = Vec::with_capacity(infos.len());
        for info in infos {
            let path = by_hash.get(&info.sha256).cloned().or_else(|| {
                root.and_then(|root| {
                    Path::new(&info.path)
                        .file_name()
                        .map(|name| root.join(name))
                        .filter(|p| p.exists())
                })
            });
            let path = match path.or_else(|| {
                let p = PathBuf::from(&info.path);
                p.exists().then_some(p)
            }) {
                Some(p) => p,
                None => return Err(GmError::FontNotFound(info.path.clone()).into()),
            };

            let bytes = std::fs::read(&path)
                .with_context(|| format!("failed to read font {}", path.display()))?;
            let got = sha256_hex(&bytes);
            if got != info.sha256 {
                return Err(GmError::FontHashMismatch(format!(
                    "{} (expected {}, got {})",
                    path.display(),
                    info.sha256,
                    got
                ))
                .into());
            }
            let font = fontdue::Font::from_bytes(bytes.clone(), fontdue::FontSettings::default())
                .map_err(|e| anyhow!("failed to load font {}: {e}", path.display()))?;
            fonts.push(LoadedFont {
                record: info.clone(),
                bytes,
                font,
            });
        }
        Ok(FontSet { fonts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_spaces_and_controls() {
        assert!(!is_usable_char(' ', false));
        assert!(!is_usable_char('\n', false));
        assert!(!is_usable_char('\u{200B}', false));
        assert!(is_usable_char('A', false));
        assert!(is_usable_char('あ', false));
    }

    #[test]
    fn pua_is_opt_in() {
        assert!(!is_usable_char('\u{E000}', false));
        assert!(is_usable_char('\u{E000}', true));
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn enumerates_real_font() {
        let Some(path) = crate::test_util::test_font_path() else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let cps = enumerate_codepoints(&bytes, false).unwrap();
        assert!(cps.contains(&'A'));
        assert!(
            cps.windows(2).all(|w| w[0] < w[1]),
            "codepoints must be sorted and unique"
        );
    }

    #[test]
    fn collects_glob_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ttf"), b"x").unwrap();
        std::fs::write(dir.path().join("b.ttf"), b"x").unwrap();
        let glob = format!("{}/*.ttf", dir.path().to_string_lossy());
        let paths = collect_font_paths(&[glob], None).unwrap();
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn load_for_dictionary_rejects_wrong_hash() {
        let Some(path) = crate::test_util::test_font_path() else {
            return;
        };
        let record = FontRecord {
            path: path.to_string_lossy().into_owned(),
            name: "test".into(),
            sha256: "00".repeat(32),
        };
        let err = FontSet::load_for_dictionary(&[record], &[], None)
            .err()
            .expect("expected a font error")
            .to_string();
        assert!(err.contains("--fonts"), "unexpected error: {err}");
    }
}
