# glyphmosaic Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `glyphmosaic`, a Rust CLI that turns any video into a grid of Unicode glyphs chosen by MSE over a rendered font dictionary, fast enough to reproduce the "12 characters per frame (4×3)" Bad Apple effect.

**Architecture:** A dictionary builder rasterizes every encoded glyph, crops to its ink bounding box, stretches it to fill a square cell, and stores 16×16 plus 4×4 u8 feature vectors. The renderer decodes video frames, computes per-cell features, scans the dictionary coarse-to-fine with AVX2 + rayon, recomposes each cell from the original font at output size, and streams raw RGB into ffmpeg for H.264 + audio muxing.

**Tech Stack:** Rust 2021, `clap`, `fontdue`, `ttf-parser`, `rayon`, `serde`/`serde_json`, `sha2`, `unicode-general-category`, `lru`, `memmap2`, `indicatif`, ffmpeg/ffprobe subprocesses.

**Spec:** `docs/superpowers/specs/2026-11-01-glyphmosaic-design.md`

## Global Constraints

- Repo root: `C:\Users\taich\Documents\デフォルトプロジェクト\glyphmosaic`.
- Edition 2021; MSRV 1.85; primary target Windows x86_64, code must stay portable.
- Full feature grid is 16×16 (256 u8); coarse grid is 4×4 (16 u8).
- Grid default `4x3`; top-k default 256; cell height default 240; panel width default 480.
- Dictionary magic `GMD1`, fixed entry: `codepoint u32 | font u16 | reserved u16 | full [u8;256] | coarse [u8;16]` = 280 bytes.
- Matching uses integer MSE; ties break by dictionary index (lowest wins). No floating-point scores.
- Input envelope: SDR, 8-bit, progressive, first audio track only. HDR/10-bit/interlaced must hard-error with a clear message.
- Output: H.264 `libx264 -preset veryfast -crf 18 -pix_fmt yuv420p`; audio default AAC 192k, `copy` optional; output width/height must be even.
- Fonts are never committed: `fonts/` is gitignored; `scripts/fetch-fonts.ps1` + `scripts/fonts.lock.json` pin downloads and SHA-256.
- Determinism is guaranteed only for the same machine, build, fonts, and parameters. Rasterization float noise may differ across CPUs.
- No AI image generation. Every pick is a real font glyph.
- Use `anyhow` for application errors; user-facing errors must name the fix (for example, `pass --fonts`).

## Review Focus

These are the input classes most likely to bite a user, each pinned by a test in the task that owns the code:

1. **Odd or non-4:3 source dimensions** (for example 101×75, or 16:9 with a 4×3 grid): cell aspect and H.264 even-dimension rules must not produce an odd canvas or a stretched/broken output. Tests in Tasks 11, 13, 14.
2. **No audio stream**: render must still succeed and produce a video-only MP4. Test in Task 13.
3. **Missing, moved, or hash-mismatched font at render time**: clear error naming `--fonts`, never a panic or a silent wrong glyph. Tests in Tasks 3 and 13.
4. **All-black or all-white frames** (very common in Bad Apple): deduplication must still return a visible glyph for every cell, with no panic on zero-ink or saturated queries. Tests in Tasks 6 and 11.
5. **Audio codec that cannot be copied into MP4** (for example Opus): `--audio copy` must fail with a message suggesting `--audio aac`, not produce a broken file. Test in Task 9.

---

### Task 1: Bootstrap the repo, toolchain, and crate skeleton

**Files:**
- Create: `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `src/cli.rs`, `src/error.rs`, `.gitignore`, `README.md`, `LICENSE-MIT`, `LICENSE-APACHE`
- Create: `src/test_util.rs`

**Interfaces:**
- Produces: `glyphmosaic::VERSION: &'static str`; `glyphmosaic::cli::run() -> anyhow::Result<()>`; `glyphmosaic::error::GmError`; `#[cfg(test)] test_util::test_font_path() -> Option<PathBuf>`.

- [ ] **Step 1: Install the toolchain**

```powershell
winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
winget install --id Git.Git -e --accept-source-agreements --accept-package-agreements
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Program Files\Git\cmd;$env:Path"
rustup default stable-msvc
rustc --version; cargo --version; git --version
```

Expected: three version lines, no "not recognized".

- [ ] **Step 2: Create the crate skeleton**

`Cargo.toml`:

```toml
[package]
name = "glyphmosaic"
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
description = "Turn video frames into a mosaic of Unicode font glyphs"
license = "MIT OR Apache-2.0"

[lib]
name = "glyphmosaic"
path = "src/lib.rs"

[[bin]]
name = "glyphmosaic"
path = "src/main.rs"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
fontdue = "0.9"
ttf-parser = "0.25"
rayon = "1.10"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
unicode-general-category = "1"
lru = "0.12"
memmap2 = "0.9"
indicatif = "0.17"
thiserror = "2"

[dev-dependencies]
tempfile = "3"
```

`src/lib.rs`:

```rust
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod cli;
pub mod error;

#[cfg(test)]
mod test_util;
```

`src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum GmError {
    #[error("unsupported input: {0}")]
    Unsupported(String),
    #[error("font not found: {0} (pass --fonts with the same files used for dict build)")]
    FontNotFound(String),
    #[error("font hash mismatch: {0} (pass --fonts with the exact files used for dict build)")]
    FontHashMismatch(String),
    #[error("dictionary is incompatible: {0}")]
    DictIncompatible(String),
}
```

`src/cli.rs`:

```rust
pub fn run() -> anyhow::Result<()> {
    println!("glyphmosaic {}", crate::VERSION);
    Ok(())
}
```

`src/main.rs`:

```rust
fn main() -> anyhow::Result<()> {
    glyphmosaic::cli::run()
}
```

`src/test_util.rs`:

```rust
use std::path::PathBuf;

pub fn test_font_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GLYPHMOSAIC_TEST_FONT") {
        return Some(PathBuf::from(p));
    }
    ["fonts/Jigmo.ttf", "C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/segoeui.ttf"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}
```

`.gitignore`:

```text
/target
/fonts
/demo
/out
*.gmd
*.tsv
*.mp4
```

- [ ] **Step 3: Write the smoke test**

Add to `src/lib.rs`:

```rust
#[cfg(test)]
mod smoke_tests {
    #[test]
    fn version_is_set() {
        assert!(!crate::VERSION.is_empty());
    }
}
```

- [ ] **Step 4: Run it**

Run: `cargo test`
Expected: PASS, 1 test.

- [ ] **Step 5: Initialize git and commit**

```powershell
git init
git config user.name  # if empty: git config user.name "glyphmosaic"
git config user.email # if empty: git config user.email "dev@localhost"
git add Cargo.toml Cargo.lock src .gitignore README.md LICENSE-MIT LICENSE-APACHE
git commit -m "chore: bootstrap glyphmosaic crate"
```

`README.md` is a one-paragraph placeholder; licenses are standard MIT and Apache-2.0 texts.

---

### Task 2: Image operations and glyph feature extraction

**Files:**
- Create: `src/imageops.rs`, `src/raster.rs`
- Modify: `src/lib.rs` (add `pub mod imageops; pub mod raster;`)

**Interfaces:**
- Produces:
  - `imageops::rgb_to_gray(rgb: &[u8], out: &mut [u8])`
  - `imageops::resize_area_gray(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize)`
  - `imageops::resize_bilinear_rgb(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize)`
  - `raster::Coverage { width: usize, height: usize, data: Vec<u8> }`
  - `raster::crop_ink(data: &[u8], width: usize, height: usize) -> Option<(usize, usize, usize, usize)>`
  - `raster::stretch_coverage(src: &Coverage, dw: usize, dh: usize) -> Coverage`
  - `raster::features_from_coverage(cov: &Coverage) -> Option<([u8; 256], [u8; 16])>`
  - `raster::rasterize_stretched(font: &fontdue::Font, c: char, px: f32) -> Option<Coverage>`

- [ ] **Step 1: Write failing tests**

In `src/imageops.rs`:

```rust
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
```

In `src/raster.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_trims_transparent_border() {
        let mut b = vec![0u8; 5 * 4];
        b[1 * 5 + 1] = 255;
        b[1 * 5 + 2] = 128;
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
        let (full, coarse) = features_from_coverage(&Coverage { width: 16, height: 16, data }).unwrap();
        assert_eq!(full[0], 255);
        assert_eq!(full[15], 0);
        assert_eq!(coarse[0], 255);  // top-left 4x4
        assert_eq!(coarse[3], 0);    // top-right 4x4
    }

    #[test]
    fn stretch_keeps_corner_values() {
        let src = Coverage { width: 2, height: 2, data: vec![255, 0, 0, 0] };
        let out = stretch_coverage(&src, 4, 4);
        assert_eq!(out.width, 4);
        assert!(out.data[0] > 200);
        assert!(out.data[3] < 40);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test imageops raster`
Expected: FAIL, functions not found.

- [ ] **Step 3: Implement**

- `rgb_to_gray`: `(77*r + 150*g + 29*b + 128) >> 8`, matching Rec.601.
- `resize_area_gray`: compute each destination pixel's fractional source rectangle; accumulate with `f64`; round with `+0.5`; clamp. Handle downscale only; if `dw >= sw && dh >= sh`, delegate to nearest.
- `resize_bilinear_rgb`: standard separable bilinear with f64 coordinates `(x + 0.5) * sw / dw - 0.5`, clamp edges.
- `crop_ink`: trim rows/columns whose max value is 0.
- `stretch_coverage`: bilinear resize of the cropped coverage into `dw × dh`.
- `features_from_coverage`: box-average into 16×16 using fractional areas (works for any input size), then each 4×4 block to coarse. Return `None` if all zero.
- `rasterize_stretched`: `font.rasterize(c, px)`, use `metrics.width/height`, crop via `crop_ink`, then stretch to 64×64 and return.

- [ ] **Step 4: Run tests**

Run: `cargo test imageops raster`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/imageops.rs src/raster.rs src/lib.rs
git commit -m "feat: add image resampling and glyph feature extraction"
```

---

### Task 3: Font loading, codepoint filtering, and dictionary format

**Files:**
- Create: `src/fonts.rs`, `src/dict/mod.rs`, `src/dict/format.rs`
- Modify: `src/lib.rs` (add `pub mod dict; pub mod fonts;`)

**Interfaces:**
- Produces:
  - `fonts::FontRecord { path: String, name: String, sha256: String }` (`Clone`, `Debug`, `Serialize`, `Deserialize`, `PartialEq`)
  - `fonts::FontSet { fonts: Vec<LoadedFont> }`
  - `fonts::LoadedFont { record: FontRecord, bytes: Vec<u8>, font: fontdue::Font }`
  - `fonts::sha256_hex(bytes: &[u8]) -> String`
  - `fonts::is_usable_char(c: char, include_pua: bool) -> bool`
  - `fonts::collect_font_paths(globs: &[String], root: Option<&Path>) -> anyhow::Result<Vec<PathBuf>>`
  - `fonts::enumerate_codepoints(bytes: &[u8], include_pua: bool) -> anyhow::Result<Vec<char>>`
  - `fonts::load_fonts(paths: &[PathBuf], root: Option<&Path>) -> anyhow::Result<FontSet>`
  - `fonts::FontSet::load_for_dictionary(infos: &[FontRecord], override_globs: &[String], root: Option<&Path>) -> anyhow::Result<Self>`
  - `dict::DictHeader { version: u32, cell_feature: u16, coarse_feature: u16, unicode_version: String, fonts: Vec<FontRecord> }`
  - `dict::Dictionary { header, codepoints: Vec<u32>, font_indices: Vec<u16>, full: Vec<[u8;256]>, coarse: Vec<[u8;16]> }`
  - `Dictionary::{new, len, is_empty, push, write, read}`

- [ ] **Step 1: Write failing tests**

In `src/fonts.rs`:

```rust
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
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn enumerates_real_font() {
        let Some(path) = crate::test_util::test_font_path() else { return };
        let bytes = std::fs::read(path).unwrap();
        let cps = enumerate_codepoints(&bytes, false).unwrap();
        assert!(cps.contains(&'A'));
        assert!(cps.windows(2).all(|w| w[0] < w[1]), "codepoints must be sorted and unique");
    }
}
```

In `src/dict/format.rs`:

```rust
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
    }

    #[test]
    fn rejects_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.gmd");
        std::fs::write(&path, b"NOPE").unwrap();
        assert!(Dictionary::read(&path).unwrap_err().to_string().contains("magic"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test fonts dict::format`
Expected: FAIL, types/functions missing.

- [ ] **Step 3: Implement**

- `is_usable_char`: reject `Cc`, `Cf`, `Cs`, `Cn`, `Zl`, `Zp`, `Zs`; reject `Co` unless `include_pua`; reject default-ignorable ranges `U+FE00..=U+FE0F` and `U+E0100..=U+E01EF`; reject `U+FFFE/FFFF` and plane noncharacters (`cp & 0xFFFE == 0xFFFE`).
- `enumerate_codepoints`: `ttf_parser::Face::parse(bytes, 0)`, then `face.tables().cmap`, iterate `subtables()` where `is_unicode()`, call `subtable.codepoints(|cp| ...)`, filter to `char::from_u32`, require `face.glyph_index(c).is_some()`, filter usable, collect into `BTreeSet<u32>`, return sorted `Vec<char>`.
- `sha256_hex`: `Sha256::digest` and hex-encode lowercase.
- `collect_font_paths`: for each glob, support `*` and `?` in the final component by scanning its parent directory; recurse `**` one level (`fonts/**/*.ttf`), expand directories to their `.ttf`/`.otf`/`.ttc` files, sort and deduplicate. A literal path that exists is returned as-is. No match → error listing the pattern.
- Font name: `face.names().get(ttf_parser::name_id::FULL_NAME)` decoded via `to_string()`; fall back to file stem.
- `load_fonts`: read bytes, hash, `fontdue::Font::from_bytes(bytes.clone(), FontSettings::default())`, build records; `root` is used to make paths relative when possible.
- `load_for_dictionary`: resolve each `FontRecord` by trying `override_globs` (glob-expanded via `glob` behavior implemented with `std::fs::read_dir` recursion), then `root.join(file_name)`, then the recorded path; verify SHA-256; error `GmError::FontNotFound` / `FontHashMismatch` with a `--fonts` hint.
- Dictionary `new`, `push`, `write`, `read`: implement the `GMD1` format exactly as specified. `write` uses `BufWriter`, JSON header, then each entry little-endian. `read` validates magic, version, header length, count, and exact file size.

- [ ] **Step 4: Run tests**

Run: `cargo test fonts dict::format`
Expected: PASS (font test is skipped when no font exists).

- [ ] **Step 5: Commit**

```powershell
git add src/fonts.rs src/dict src/lib.rs
git commit -m "feat: add font handling and GMD1 dictionary format"
```

---

### Task 4: Build the glyph dictionary

**Files:**
- Create: `src/dict/build.rs`
- Modify: `src/dict/mod.rs` (re-export `build`)

**Interfaces:**
- Produces:
  - `dict::build::BuildOptions { font_paths: Vec<PathBuf>, include_pua: bool, min_ink: u8, jobs: usize }`
  - `dict::build::build(opts: &BuildOptions) -> anyhow::Result<Dictionary>`

- [ ] **Step 1: Write the failing test**

In `src/dict/build.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_is_deterministic_and_nonempty() {
        let Some(path) = crate::test_util::test_font_path() else { return };
        let opts = BuildOptions {
            font_paths: vec![path],
            include_pua: false,
            min_ink: 1,
            jobs: 2,
        };
        let a = build(&opts).unwrap();
        let b = build(&opts).unwrap();
        assert!(a.len() > 100, "font should produce many glyphs, got {}", a.len());
        assert_eq!(a.codepoints, b.codepoints);
        assert_eq!(a.full, b.full);
        assert!(a.full.iter().any(|f| f.iter().any(|&v| v > 0)));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test build_is_deterministic`
Expected: FAIL, `build` not found.

- [ ] **Step 3: Implement**

- Load each font path once (bytes, record, `fontdue::Font`).
- For each font index, enumerate chars, sort, then `par_iter().map(|c| rasterize_stretched(&font, c, 128.0))`, collect in order.
- Skip `None` and glyphs whose max coverage `< min_ink`.
- `features_from_coverage` → `(full, coarse)`; push `(cp, font_idx)`.
- Print per-font progress with `indicatif` (`{spinner} {font} {pos}/{len}`).
- Header fonts are the loaded records; header version 1, cell_feature 16, coarse_feature 4, unicode_version `"17.0"`.
- Determinism: preserve font order and codepoint order; no HashMap iteration in output.

- [ ] **Step 4: Run tests**

Run: `cargo test build_is_deterministic`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/dict
git commit -m "feat: build glyph dictionary from fonts"
```

---

### Task 5: SIMD scoring and top-k scan

**Files:**
- Create: `src/matching/mod.rs`, `src/matching/simd.rs`, `src/matching/scan.rs`
- Modify: `src/lib.rs` (add `pub mod matching;`)

**Interfaces:**
- Produces:
  - `matching::simd::coarse_score(q: &[u8;16], g: &[u8;16]) -> u32`
  - `matching::simd::full_score(q: &[u8;256], g: &[u8;256]) -> u32`
  - `matching::scan::scan_coarse(dict: &Dictionary, q: &[u8;16], top_k: usize) -> Vec<(u32, usize)>`
  - `matching::scan::refine_full(dict: &Dictionary, q: &[u8;256], candidates: &[(u32, usize)]) -> (usize, u32)`
  - `matching::scan::match_one(dict: &Dictionary, q: &Query, top_k: usize, exact: bool) -> MatchResult`

- [ ] **Step 1: Write failing tests**

In `src/matching/simd.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_matches_hand_calculation() {
        let q = [10u8; 16];
        let g = [12u8; 16];
        assert_eq!(coarse_score(&q, &g), 64);
    }

    #[test]
    fn full_matches_hand_calculation() {
        let q = [0u8; 256];
        let mut g = [0u8; 256];
        g[0] = 3;
        g[255] = 4;
        assert_eq!(full_score(&q, &g), 25);
    }
}
```

In `src/matching/scan.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn dict_with(entries: usize) -> Dictionary {
        let header = DictHeader {
            version: 1, cell_feature: 16, coarse_feature: 4,
            unicode_version: "17.0".into(), fonts: vec![],
        };
        let mut d = Dictionary::new(header);
        for i in 0..entries {
            let mut full = [0u8; 256];
            let mut coarse = [0u8; 16];
            for j in 0..256 {
                full[j] = ((i * 37 + j * 11) % 256) as u8;
            }
            for j in 0..16 {
                coarse[j] = full[j * 16];
            }
            d.push(char::from_u32(0x41 + (i as u32 % 26)).unwrap(), 0, full, coarse);
        }
        d
    }

    #[test]
    fn finds_planted_best_match() {
        let d = dict_with(500);
        let q = Query { full: d.full[42], coarse: d.coarse[42] };
        let r = match_one(&d, &q, 32, false);
        assert_eq!(r.index, 42);
        assert_eq!(r.mse, 0);
    }

    #[test]
    fn ties_pick_lowest_index() {
        let mut d = dict_with(2);
        d.full[1] = d.full[0];
        d.coarse[1] = d.coarse[0];
        let q = Query { full: d.full[0], coarse: d.coarse[0] };
        assert_eq!(match_one(&d, &q, 8, false).index, 0);
        assert_eq!(match_one(&d, &q, 8, true).index, 0);
    }

    #[test]
    fn exact_agrees_with_coarse_fine_on_planted_data() {
        let d = dict_with(300);
        for i in [0usize, 7, 99, 299] {
            let q = Query { full: d.full[i], coarse: d.coarse[i] };
            assert_eq!(match_one(&d, &q, 64, false).index, i);
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test matching`
Expected: FAIL, functions missing.

- [ ] **Step 3: Implement**

- `coarse_score`: AVX2 path loads 16 bytes from each, widens u8→i16, subtracts, squares with `_mm256_mullo_epi16`, sums pairs with `_mm256_madd_epi16` against ones, horizontal add. Scalar fallback sums `(a-b)^2` as u32.
- `full_score`: loop 16 chunks of 16 bytes, accumulate `coarse_score`-style chunk sums.
- Runtime dispatch: `#[cfg(any(target_arch = "x86", target_arch = "x86_64"))] if std::is_x86_feature_detected!("avx2") { return unsafe { ... } }`.
- `scan_coarse`: iterate all entries, maintain `BinaryHeap<(u32, usize)>` (max-heap; worst on top). Push when `len < top_k` or `(score, index) < *heap.peek()`. Extract and sort by `(score, index)`.
- `refine_full`: score each candidate with `full_score`, return min `(score, index)`.
- `match_one`: if `exact`, scan all entries with `full_score` and track min; else coarse scan then refine.
- `Query` / `MatchResult` are defined in `matching/mod.rs` (next task) — define them now to unblock:

```rust
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct Query { pub full: [u8; 256], pub coarse: [u8; 16] }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchResult { pub index: usize, pub mse: u32 }
```

- [ ] **Step 4: Run tests**

Run: `cargo test matching`
Expected: PASS, including AVX2/scalar agreement if the CPU supports AVX2.

- [ ] **Step 5: Commit**

```powershell
git add src/matching src/lib.rs
git commit -m "feat: add integer MSE scoring and top-k dictionary scan"
```

---

### Task 6: Multi-cell matching with deduplication and rayon

**Files:**
- Modify: `src/matching/mod.rs`

**Interfaces:**
- Produces:
  - `matching::MatchStats { queries: usize, unique: usize }`
  - `matching::match_cells(dict: &Dictionary, queries: &[Query], top_k: usize, exact: bool, threads: usize) -> (Vec<MatchResult>, MatchStats)`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn dict(n: usize) -> Dictionary {
        let header = DictHeader {
            version: 1, cell_feature: 16, coarse_feature: 4,
            unicode_version: "17.0".into(), fonts: vec![],
        };
        let mut d = Dictionary::new(header);
        for i in 0..n {
            let mut f = [0u8; 256];
            let mut c = [0u8; 16];
            f.fill((i % 200) as u8);
            c.fill((i % 200) as u8);
            d.push('A', 0, f, c);
        }
        d
    }

    #[test]
    fn deduplicates_identical_queries() {
        let d = dict(100);
        let q0 = Query { full: d.full[10], coarse: d.coarse[10] };
        let q1 = Query { full: d.full[20], coarse: d.coarse[20] };
        let queries = vec![q0, q1, q0, q0];
        let (r, s) = match_cells(&d, &queries, 16, false, 1);
        assert_eq!(s.queries, 4);
        assert_eq!(s.unique, 2);
        assert_eq!(r[0].index, 10);
        assert_eq!(r[1].index, 20);
        assert_eq!(r[2], r[0]);
    }

    #[test]
    fn thread_count_does_not_change_results() {
        let d = dict(200);
        let queries: Vec<Query> = (0..40).map(|i| Query {
            full: d.full[i * 3], coarse: d.coarse[i * 3],
        }).collect();
        let (a, _) = match_cells(&d, &queries, 32, false, 1);
        let (b, _) = match_cells(&d, &queries, 32, false, 4);
        assert_eq!(a, b);
    }

    #[test]
    fn black_query_still_returns_an_index() {
        let d = dict(50);
        let q = Query { full: [0u8; 256], coarse: [0u8; 16] };
        let (r, _) = match_cells(&d, &[q], 8, false, 1);
        assert!(r[0].index < d.len());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test matching::tests`
Expected: FAIL, `match_cells` missing.

- [ ] **Step 3: Implement**

- Build `HashMap<Query, usize>` mapping first occurrence; `unique: Vec<Query>`.
- Build a `rayon::ThreadPool` when `threads > 0`; run `unique.par_iter().map(|q| match_one(...))` inside `pool.install`.
- Map results back to input order; return `MatchStats`.
- No shared mutable state; deterministic.

- [ ] **Step 4: Run tests**

Run: `cargo test matching::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/matching
git commit -m "feat: match frames with query dedupe and rayon"
```

---

### Task 7: Video probe and input validation

**Files:**
- Create: `src/video/mod.rs`, `src/video/probe.rs`
- Modify: `src/lib.rs` (add `pub mod video;`)

**Interfaces:**
- Produces:
  - `video::VideoInfo { width: u32, height: u32, fps_num: u32, fps_den: u32, frame_count: Option<u64>, duration: Option<f64>, has_audio: bool, pix_fmt: String, field_order: String, color_transfer: Option<String>, audio_codec: Option<String>, sar_num: u32, sar_den: u32, rotation: i32 }`
  - `video::probe::probe(path: &Path) -> anyhow::Result<VideoInfo>`
  - `video::probe::validate_input(info: &VideoInfo) -> anyhow::Result<()>`
  - `video::probe::display_dims(width: u32, height: u32, sar_num: u32, sar_den: u32, rotation: i32) -> (u32, u32)`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_clip(path: &std::path::Path) {
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i",
                   "color=c=black:s=96x72:r=10:d=0.5",
                   "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn probes_generated_clip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        make_clip(&path);
        let info = probe(&path).unwrap();
        assert_eq!((info.width, info.height), (96, 72));
        assert_eq!((info.fps_num, info.fps_den), (10, 1));
        assert!(!info.has_audio);
        validate_input(&info).unwrap();
    }

    #[test]
    fn rejects_ten_bit_and_interlaced() {
        let mut info = VideoInfo {
            width: 96, height: 72, fps_num: 10, fps_den: 1, frame_count: Some(5),
            duration: Some(0.5), has_audio: false, pix_fmt: "yuv420p10le".into(),
            field_order: "progressive".into(), color_transfer: None, audio_codec: None,
            sar_num: 1, sar_den: 1, rotation: 0,
        };
        assert!(validate_input(&info).unwrap_err().to_string().contains("10-bit"));
        info.pix_fmt = "yuv420p".into();
        info.field_order = "tt".into();
        assert!(validate_input(&info).unwrap_err().to_string().contains("interlaced"));
    }

    #[test]
    fn display_dims_normalizes_sar_and_rotation() {
        assert_eq!(display_dims(720, 480, 1, 1, 0), (720, 480));
        assert_eq!(display_dims(720, 480, 2, 1, 0), (1440, 480));
        assert_eq!(display_dims(720, 480, 1, 1, 90), (480, 720));
        assert_eq!(display_dims(720, 480, 2, 1, 90), (480, 1440));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test video::probe`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `probe`: run `ffprobe -v error -print_format json -show_streams -show_format <path>`, parse JSON.
- Video stream: first `codec_type == "video"`; width/height; `avg_frame_rate` parsed as `num/den` (fallback `r_frame_rate`); `nb_frames` parsed if present; duration from stream or format.
- `pix_fmt`, `field_order`, `color_transfer` copied.
- `sar_num`/`sar_den` from `sample_aspect_ratio` (`"2:1"`; missing/`0:1` → `1:1`).
- `rotation` from the first `side_data_list` entry's `rotation`, else `tags.rotate`, else 0; normalize to `{0,90,180,270}`.
- Audio stream: first `codec_type == "audio"`; `has_audio`; `audio_codec`.
- `display_dims`: width × sar ratio, rounded to even; if rotation is 90/270, swap width/height. Never return 0.
- `validate_input`: reject pix_fmt containing `10le`, `10be`, `12`, `p010`, or color_transfer `smpte2084`/`arib-std-b67` with "HDR/10-bit input is not supported"; reject field_order not `progressive`/`unknown` with "interlaced input is not supported".

- [ ] **Step 4: Run tests**

Run: `cargo test video::probe`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/video src/lib.rs
git commit -m "feat: probe and validate video input"
```

---

### Task 8: Frame decoding and per-cell query extraction

**Files:**
- Create: `src/frame.rs`, `src/video/decode.rs`
- Modify: `src/lib.rs` (add `pub mod frame;`)

**Interfaces:**
- Produces:
  - `frame::queries_from_rgb(rgb: &[u8], sw: usize, sh: usize, cols: usize, rows: usize) -> anyhow::Result<Vec<Query>>`
  - `video::decode::FrameReader { info: VideoInfo, ... }`
  - `FrameReader::open(path: &Path, info: VideoInfo) -> anyhow::Result<Self>`
  - `FrameReader::read_frame(&mut self, buf: &mut [u8]) -> anyhow::Result<bool>`
  - `FrameReader::frame_size(&self) -> usize`
  - `FrameReader::frame_dims(&self) -> (u32, u32)`

- [ ] **Step 1: Write failing tests**

In `src/frame.rs`:

```rust
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
```

In `src/video/decode.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::probe::probe;

    #[test]
    fn reads_expected_frame_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i",
                   "testsrc2=s=64x48:r=10:d=0.5", "-c:v", "libx264",
                   "-pix_fmt", "yuv420p"])
            .arg(&path).status().unwrap();
        assert!(ok.success());
        let info = probe(&path).unwrap();
        let mut reader = FrameReader::open(&path, info).unwrap();
        let mut buf = vec![0u8; reader.frame_size()];
        let mut n = 0;
        while reader.read_frame(&mut buf).unwrap() { n += 1; }
        assert_eq!(n, 5);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test frame video::decode`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `queries_from_rgb`: RGB→gray; area-resize to `cols*16 × rows*16`; for each cell, copy 16×16 to `full`; coarse is the average of each 4×4 block (use `features_from_coverage` on a `Coverage` of the cell).
- `FrameReader::open`: `ffmpeg -v error -i <path> -map 0:v:0 -vf fps=<num>/<den>,scale=trunc(iw*sar/2)*2:ih,setsar=1 -f rawvideo -pix_fmt rgb24 -`; stdout piped. Compute `(w, h) = display_dims(info.width, info.height, info.sar_num, info.sar_den, info.rotation)`, `frame_size = w*h*3`, `frame_dims = (w, h)`.
- `read_frame`: `read_exact` into `buf`; on EOF (`UnexpectedEof` with 0 bytes read first) return `Ok(false)`; partial frame is an error. Implement with a manual loop so zero bytes means clean EOF.
- Keep the child handle; drop kills/waits.

- [ ] **Step 4: Run tests**

Run: `cargo test frame video::decode`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/frame.rs src/video/decode.rs src/lib.rs
git commit -m "feat: decode frames and extract cell queries"
```

---

### Task 9: Video encoding and audio modes

**Files:**
- Create: `src/video/encode.rs`
- Modify: `src/video/mod.rs`

**Interfaces:**
- Produces:
  - `video::encode::AudioMode { Aac, Copy, None }`
  - `video::encode::EncodeOptions { crf: u8, audio: AudioMode, fps_num: u32, fps_den: u32 }`
  - `video::encode::Encoder::new(out: &Path, input: &Path, width: u32, height: u32, opts: &EncodeOptions) -> anyhow::Result<Self>`
  - `Encoder::write(&mut self, rgb: &[u8]) -> anyhow::Result<()>`
  - `Encoder::finish(self) -> anyhow::Result<()>`
  - `video::encode::mp4_audio_copy_supported(codec: &str) -> bool`
  - `video::encode::validate_audio_copy(codec: Option<&str>) -> anyhow::Result<()>`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_support_list() {
        assert!(mp4_audio_copy_supported("aac"));
        assert!(mp4_audio_copy_supported("mp3"));
        assert!(!mp4_audio_copy_supported("opus"));
        assert!(!mp4_audio_copy_supported("vorbis"));
    }

    #[test]
    fn copy_mode_names_the_fix_for_opus() {
        let err = validate_audio_copy(Some("opus")).unwrap_err().to_string();
        assert!(err.contains("--audio aac"), "unexpected error: {err}");
        validate_audio_copy(Some("aac")).unwrap();
        validate_audio_copy(None).unwrap();
    }

    #[test]
    fn writes_video_only_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i",
                   "color=c=black:s=64x48:r=10:d=0.5", "-c:v", "libx264",
                   "-pix_fmt", "yuv420p"])
            .arg(&input).status().unwrap();
        assert!(ok.success());

        let out = dir.path().join("out.mp4");
        let opts = EncodeOptions { crf: 18, audio: AudioMode::None, fps_num: 10, fps_den: 1 };
        let mut enc = Encoder::new(&out, &input, 64, 48, &opts).unwrap();
        for _ in 0..5 { enc.write(&vec![0u8; 64 * 48 * 3]).unwrap(); }
        enc.finish().unwrap();

        let info = crate::video::probe::probe(&out).unwrap();
        assert_eq!((info.width, info.height), (64, 48));
        assert!(!info.has_audio);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test video::encode`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `Encoder::new`: build `ffmpeg -v error -y -f rawvideo -pix_fmt rgb24 -s WxH -r num/den -i -` then, unless `AudioMode::None`, `-i <input> -map 0:v:0 -map 1:a:0?`; `-c:v libx264 -preset veryfast -crf <crf> -pix_fmt yuv420p`; audio `aac -b:a 192k` or `copy`; `-shortest -movflags +faststart <out>`. stdin piped.
- Before copy, if `AudioMode::Copy` and the input's audio codec is not in the MP4 list, call `validate_audio_copy` and return an error naming `--audio aac`. `Encoder::new` probes the input when `Copy` is selected.
- `write`: assert length == w*h*3, `write_all`.
- `finish`: drop stdin, wait, error on non-zero status.
- `mp4_audio_copy_supported`: aac, mp3, ac3, eac3, alac.
- `validate_audio_copy`: `None` or supported codec → `Ok`; unsupported → `bail!("audio codec {codec} cannot be copied into MP4; use --audio aac")`.

- [ ] **Step 4: Run tests**

Run: `cargo test video::encode`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/video
git commit -m "feat: stream raw frames into ffmpeg with audio modes"
```

---

### Task 10: Bitmap text and glyph cache

**Files:**
- Create: `src/render/mod.rs`, `src/render/text.rs`, `src/render/glyph_cache.rs`
- Modify: `src/lib.rs` (add `pub mod render;`)

**Interfaces:**
- Produces:
  - `render::text::text_width(text: &str, scale: u32) -> u32`
  - `render::text::draw_text(canvas: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, scale: u32, text: &str, color: [u8; 3])`
  - `render::glyph_cache::GlyphCache::new(fonts: Arc<Vec<fontdue::Font>>, cell_w: u32, cell_h: u32, max_mb: usize) -> Self`
  - `GlyphCache::get(&mut self, font_index: u16, cp: u32) -> anyhow::Result<Arc<Vec<u8>>>`

- [ ] **Step 1: Write failing tests**

In `src/render/text.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_monospace() {
        assert_eq!(text_width("U+2044", 3), 6 * 6 * 3 - 3);
    }

    #[test]
    fn draw_sets_pixels() {
        let mut canvas = vec![0u8; 100 * 20 * 3];
        draw_text(&mut canvas, 100, 20, 2, 2, 3, "U+2044", [255, 255, 255]);
        assert!(canvas.chunks(3).any(|p| p == [255, 255, 255]));
        assert!(canvas.chunks(3).any(|p| p == [0, 0, 0]));
    }
}
```

In `src/render/glyph_cache.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn caches_same_glyph() {
        let Some(path) = crate::test_util::test_font_path() else { return };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let fonts = Arc::new(vec![font]);
        let mut cache = GlyphCache::new(fonts, 32, 32, 8);
        let a = cache.get(0, 'A' as u32).unwrap();
        let b = cache.get(0, 'A' as u32).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.len(), 32 * 32);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test render::text render::glyph_cache`
Expected: FAIL.

- [ ] **Step 3: Implement**

- 5×7 bitmap font table for `0-9`, `A-F`, `U`, `+`, `:`, `-`, ` `, `F`. Each glyph is 7 rows of 5 bits.
- `text_width`: `chars * 6 * scale - scale` (one blank column between glyphs, none after the last).
- `draw_text`: for each set bit, fill a `scale × scale` block; skip out-of-bounds; RGB triples.
- `GlyphCache`: `LruCache<(u16, u32), Arc<Vec<u8>>>` with capacity `max(1, max_mb * 1024 * 1024 / (cell_w * cell_h) as usize)`; `get` rasterizes `cp` at `px = cell_h as f32`, crops, stretches to `cell_w × cell_h`, caches `Arc`.
- Unknown codepoint / missing font index → `anyhow::bail!` with the codepoint in hex.

- [ ] **Step 4: Run tests**

Run: `cargo test render::text render::glyph_cache`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/render src/lib.rs
git commit -m "feat: add bitmap text and LRU glyph cache"
```

---

### Task 11: Mosaic and panel compositor

**Files:**
- Create: `src/render/panel.rs`
- Modify: `src/render/mod.rs`

**Interfaces:**
- Produces:
  - `render::Layout { Mosaic, Panel }`
  - `render::RenderConfig { grid_cols: usize, grid_rows: usize, cell_height: u32, layout: Layout, panel_width: u32, cache_mb: usize }`
  - `render::Compositor::new(config: RenderConfig, fonts: Arc<Vec<fontdue::Font>>, src_w: u32, src_h: u32) -> anyhow::Result<Self>`
  - `Compositor::canvas_size(&self) -> (u32, u32)`
  - `Compositor::compose(&mut self, frame: &[u8], matches: &[MatchResult], dict: &Dictionary, frame_no: u64) -> anyhow::Result<&[u8]>`
  - `panel::draw_panel(canvas: &mut [u8], cw: u32, ch: u32, x: u32, y: u32, w: u32, h: u32, frame: &[u8], src_w: u32, src_h: u32, dict: &Dictionary, matches: &[MatchResult])`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictHeader;
    use crate::matching::MatchResult;
    use crate::Dictionary;
    use std::sync::Arc;

    #[test]
    fn builds_even_canvas_for_4x3_on_4x3() {
        let Some(path) = crate::test_util::test_font_path() else { return };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let cfg = RenderConfig {
            grid_cols: 4, grid_rows: 3, cell_height: 240,
            layout: Layout::Mosaic, panel_width: 480, cache_mb: 16,
        };
        let c = Compositor::new(cfg, Arc::new(vec![font]), 640, 480).unwrap();
        assert_eq!(c.canvas_size(), (960, 720));
    }

    #[test]
    fn odd_source_still_gives_even_canvas() {
        let Some(path) = crate::test_util::test_font_path() else { return };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let cfg = RenderConfig {
            grid_cols: 4, grid_rows: 3, cell_height: 241,
            layout: Layout::Panel, panel_width: 481, cache_mb: 16,
        };
        let c = Compositor::new(cfg, Arc::new(vec![font]), 101, 75).unwrap();
        let (w, h) = c.canvas_size();
        assert_eq!((w % 2, h % 2), (0, 0));
    }

    #[test]
    fn composes_all_black_frame_without_panicking() {
        let Some(path) = crate::test_util::test_font_path() else { return };
        let bytes = std::fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let mut dict = Dictionary::new(DictHeader {
            version: 1, cell_feature: 16, coarse_feature: 4,
            unicode_version: "17.0".into(), fonts: vec![],
        });
        dict.push('A', 0, [255u8; 256], [255u8; 16]);
        let cfg = RenderConfig {
            grid_cols: 4, grid_rows: 3, cell_height: 24,
            layout: Layout::Mosaic, panel_width: 80, cache_mb: 8,
        };
        let mut c = Compositor::new(cfg, Arc::new(vec![font]), 64, 48).unwrap();
        let frame = vec![0u8; 64 * 48 * 3];
        let matches = vec![MatchResult { index: 0, mse: 0 }; 12];
        let out = c.compose(&frame, &matches, &dict, 0).unwrap();
        assert_eq!(out.len(), 96 * 72 * 3);
        assert!(out.chunks(3).any(|p| p[0] > 0));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test render::tests`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Cell height rounded down to even; cell width = `round(cell_h * (src_w/cols)/(src_h/rows))`, rounded down to even, minimum 2.
- Mosaic size = `cell_w*cols × cell_h*rows`; panel adds `panel_width` (even) to canvas width.
- `compose`: fill black; for each cell, get bitmap from `GlyphCache`, copy coverage into RGB (`r=g=b=v`); panel calls `draw_panel`.
- `draw_panel`: scale source frame to panel width with `resize_bilinear_rgb`, at top; then draw `U+XXXX` for each cell in 2 columns; choose text scale from available row height (min 1, max 4); draw a frame counter at the bottom if space remains.
- `compose` returns `&self.canvas`.

- [ ] **Step 4: Run tests**

Run: `cargo test render::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/render
git commit -m "feat: compose mosaic and code point panel"
```

---

### Task 12: Codepoint TSV and statistics

**Files:**
- Create: `src/stats.rs`
- Modify: `src/lib.rs` (add `pub mod stats;`)

**Interfaces:**
- Produces:
  - `stats::Stats { total_picks: u64, distinct_chars: usize, singleton_chars: usize, top: Vec<(String, u64)> }` (`Serialize`)
  - `stats::compute(codepoints: impl IntoIterator<Item = char>) -> Stats`
  - `stats::write_json(path: &Path, stats: &Stats) -> anyhow::Result<()>`
  - `stats::format_report(stats: &Stats) -> String`
  - `stats::parse_codepoint(s: &str) -> anyhow::Result<char>`
  - `stats::read_tsv(path: &Path) -> anyhow::Result<Vec<char>>`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_reference_style_stats() {
        let s = compute("aabbc".chars());
        assert_eq!(s.total_picks, 5);
        assert_eq!(s.distinct_chars, 3);
        assert_eq!(s.singleton_chars, 1);
        assert_eq!(s.top[0], ("a".to_string(), 2));
        assert_eq!(s.top[1], ("b".to_string(), 2));
    }

    #[test]
    fn parses_and_reads_tsv() {
        assert_eq!(parse_codepoint("U+2044").unwrap(), '\u{2044}');
        assert_eq!(parse_codepoint("0x41").unwrap(), 'A');
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cps.tsv");
        std::fs::write(&path, "frame\tcell\tcodepoint\tchar\tfont\tmse\n0\t0\tU+2044\t/\t0\t12\n").unwrap();
        assert_eq!(read_tsv(&path).unwrap(), vec!['\u{2044}']);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test stats`
Expected: FAIL.

- [ ] **Step 3: Implement**

- `compute`: `HashMap<char, u64>`, then top 10 sorted by `count desc, char asc`; `singleton_chars` counts entries with 1.
- `format_report`: four lines matching the spec's stats wording, thousands separators, percentages for top 10.
- `read_tsv`: skip header, take column 3 (index 2), parse.
- `write_json`: `serde_json::to_writer_pretty`.

- [ ] **Step 4: Run tests**

Run: `cargo test stats`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/stats.rs src/lib.rs
git commit -m "feat: add codepoint statistics and report"
```

---

### Task 13: CLI wiring and end-to-end pipeline

**Files:**
- Modify: `src/cli.rs`, `src/main.rs`
- Create: `tests/integration.rs`

**Interfaces:**
- Consumes: every public API above.
- Produces: subcommands `dict build`, `render`, `inspect`, `report`; `pub fn render_pipeline(...)` may stay internal to `cli.rs`.

- [ ] **Step 1: Write the failing integration test**

`tests/integration.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

fn test_font() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GLYPHMOSAIC_TEST_FONT") { return Some(p.into()); }
    ["fonts/Jigmo.ttf", "C:/Windows/Fonts/arial.ttf"]
        .iter().map(PathBuf::from).find(|p| p.exists())
}

fn make_clip(path: &Path, audio: bool) {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=96x72:r=10:d=0.5"]);
    if audio {
        cmd.args(["-f", "lavfi", "-i", "sine=frequency=440:duration=0.5", "-c:a", "aac"]);
    }
    cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-shortest"]).arg(path);
    assert!(cmd.status().unwrap().success());
}

#[test]
fn end_to_end_mosaic_and_panel() {
    let Some(font) = test_font() else { return };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.mp4");
    make_clip(&input, true);

    let opts = glyphmosaic::dict::build::BuildOptions {
        font_paths: vec![font], include_pua: false, min_ink: 1, jobs: 2,
    };
    let dict = glyphmosaic::dict::build::build(&opts).unwrap();
    let dict_path = dir.path().join("fonts.gmd");
    dict.write(&dict_path).unwrap();

    let out = dir.path().join("out.mp4");
    glyphmosaic::cli::render_for_test(
        &input, &dict_path, &out,
        glyphmosaic::render::RenderConfig {
            grid_cols: 4, grid_rows: 3, cell_height: 24,
            layout: glyphmosaic::render::Layout::Panel,
            panel_width: 80, cache_mb: 8,
        },
        8, false, 3,
    ).unwrap();

    let info = glyphmosaic::video::probe::probe(&out).unwrap();
    assert_eq!(info.width % 2, 0);
    assert_eq!(info.height % 2, 0);
    assert!(info.frame_count.unwrap_or(0) <= 5);
}

#[test]
fn no_audio_input_succeeds() {
    let Some(font) = test_font() else { return };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.mp4");
    make_clip(&input, false);
    let opts = glyphmosaic::dict::build::BuildOptions {
        font_paths: vec![font], include_pua: false, min_ink: 1, jobs: 2,
    };
    let dict = glyphmosaic::dict::build::build(&opts).unwrap();
    let dict_path = dir.path().join("fonts.gmd");
    dict.write(&dict_path).unwrap();
    let out = dir.path().join("out.mp4");
    glyphmosaic::cli::render_for_test(
        &input, &dict_path, &out,
        glyphmosaic::render::RenderConfig {
            grid_cols: 4, grid_rows: 3, cell_height: 24,
            layout: glyphmosaic::render::Layout::Mosaic,
            panel_width: 80, cache_mb: 8,
        },
        8, false, 2,
    ).unwrap();
    assert!(!glyphmosaic::video::probe::probe(&out).unwrap().has_audio);
}

#[test]
fn missing_font_reports_fix() {
    let dir = tempfile::tempdir().unwrap();
    let dict_path = dir.path().join("bad.gmd");
    let mut d = glyphmosaic::Dictionary::new(glyphmosaic::dict::DictHeader {
        version: 1, cell_feature: 16, coarse_feature: 4,
        unicode_version: "17.0".into(),
        fonts: vec![glyphmosaic::fonts::FontRecord {
            path: "nowhere.ttf".into(), name: "nowhere".into(), sha256: "00".repeat(32),
        }],
    });
    d.push('A', 0, [1u8; 256], [1u8; 16]);
    d.write(&dict_path).unwrap();
    let input = dir.path().join("in.mp4");
    make_clip(&input, false);
    let err = glyphmosaic::cli::render_for_test(
        &input, &dict_path, &dir.path().join("o.mp4"),
        glyphmosaic::render::RenderConfig {
            grid_cols: 2, grid_rows: 2, cell_height: 24,
            layout: glyphmosaic::render::Layout::Mosaic,
            panel_width: 80, cache_mb: 8,
        }, 1, false, 1,
    ).unwrap_err().to_string();
    assert!(err.contains("--fonts"), "unexpected error: {err}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test integration`
Expected: FAIL, `render_for_test` missing.

- [ ] **Step 3: Implement**

- `cli.rs`: clap derive with nested `Dict { Build { .. } }`, `Render { .. }`, `Inspect { .. }`, `Report { .. }`. All paths as `PathBuf`, font globs as `Vec<String>`.
- Add `pub fn render_for_test(input, dict_path, out, config, top_k, exact, limit) -> anyhow::Result<()>` that uses default fonts from the dictionary (no override), audio AAC, threads 0. The CLI `render` calls a parameterized `render_impl`.
- `render_impl` flow: read dict → `FontSet::load_for_dictionary` → probe + validate → `FrameReader` → optional `Encoder` → `Compositor` (using `reader.frame_dims()`) → loop (read frame, `queries_from_rgb`, `match_cells`, `compose`, write, accumulate codepoints, dump TSV if requested) → finish → stats print + JSON.
- `--no-encode`: skip encoder and output; still match and report.
- Progress: `indicatif` bar with `frame_count` or `--limit`; print final `MatchStats`.
- `inspect`: print dict version, font list + hashes, entry count, unique codepoint count, file size.
- `report`: `read_tsv` + `compute` + `format_report`.
- Error messages: font issues propagate the `--fonts` hint.

- [ ] **Step 4: Run tests**

Run: `cargo test --test integration`
Expected: PASS or skipped when no font is available. If skipped, set `GLYPHMOSAIC_TEST_FONT` and rerun.

- [ ] **Step 5: Commit**

```powershell
git add src/cli.rs src/main.rs tests
git commit -m "feat: wire CLI and end-to-end render pipeline"
```

---

### Task 14: Pinned font fetching, real dictionary, and benchmark

**Files:**
- Create: `scripts/fetch-fonts.ps1`, `scripts/fonts.lock.json`
- Modify: `README.md`

**Interfaces:**
- Produces: `fonts/` containing 9 files; a `fonts.gmd` built from them; measured per-cell matching time.

- [ ] **Step 1: Write the fetch script**

`scripts/fetch-fonts.ps1`:

```powershell
param([switch]$UpdateLock)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$fontDir = Join-Path $root "fonts"
$lockPath = Join-Path $PSScriptRoot "fonts.lock.json"
New-Item -ItemType Directory -Force -Path $fontDir | Out-Null

$files = @(
  @{ Name = "unifont-17.0.05.otf"; Url = "https://unifoundry.com/pub/unifont/unifont-17.0.05/font-builds/unifont-17.0.05.otf" },
  @{ Name = "Jigmo.zip"; Url = "https://kamichikoichi.github.io/jigmo/Jigmo-20250912.zip"; Zip = $true },
  @{ Name = "NotoSans-VF.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/<COMMIT>/ofl/notosans/NotoSans%5Bwdth,wght%5D.ttf" },
  @{ Name = "NotoSansSymbols2-Regular.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/<COMMIT>/ofl/notosanssymbols2/NotoSansSymbols2-Regular.ttf" },
  @{ Name = "NotoSansMath-Regular.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/<COMMIT>/ofl/notosansmath/NotoSansMath-Regular.ttf" },
  @{ Name = "NotoSansJP-VF.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/<COMMIT>/ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf" },
  @{ Name = "NotoSerifJP-VF.ttf"; Url = "https://raw.githubusercontent.com/google/fonts/<COMMIT>/ofl/notoserifjp/NotoSerifJP%5Bwght%5D.ttf" }
)

# 1) Resolve <COMMIT> once via the GitHub API, write it into the script.
# 2) Download to $fontDir with Invoke-WebRequest.
# 3) Expand Jigmo.zip; keep Jigmo.ttf, Jigmo2.ttf, Jigmo3.ttf.
# 4) -UpdateLock: hash every file, write scripts/fonts.lock.json as { name: { sha256 } }.
# 5) Default: compare against fonts.lock.json, re-download on mismatch, throw on failure.
```

Implementation detail: replace `<COMMIT>` with the current `google/fonts` HEAD SHA before first use and keep it fixed. Run once with `-UpdateLock`, commit the resulting `scripts/fonts.lock.json`.

- [ ] **Step 2: Run the fetch and verify**

```powershell
powershell -ExecutionPolicy Bypass -File scripts/fetch-fonts.ps1 -UpdateLock
Get-ChildItem fonts -File | Measure-Object | Select-Object -ExpandProperty Count
```

Expected: at least 9 files (7 downloads plus 3 Jigmo faces, minus the zip if removed).

- [ ] **Step 3: Build the real dictionary**

```powershell
cargo run --release -- dict build --fonts "fonts/*.ttf" --out fonts.gmd
cargo run --release -- inspect fonts.gmd
```

Expected: `inspect` reports 4 fonts and roughly 200k–270k entries. Record the exact count, build time, and file size.

- [ ] **Step 4: Benchmark matching**

Generate a 10-second synthetic 640×480 clip and run without encoding:

```powershell
ffmpeg -v error -y -f lavfi -i "testsrc2=s=640x480:r=30:d=10" -c:v libx264 -pix_fmt yuv420p demo-src.mp4
Measure-Command {
  cargo run --release -- render -i demo-src.mp4 -d fonts.gmd --no-encode --limit 300 --grid 4x3
}
```

Compute cells/sec and the projected time for 83,460 cells. Target: ≤60 s projected. If missed, check that the coarse scan is contiguous, AVX2 is active, and dedupe is working; record the numbers in README.

- [ ] **Step 5: Commit**

```powershell
git add scripts README.md
git commit -m "feat: pin font downloads and benchmark real dictionary"
```

---

### Task 15: Documentation, demo, and final verification

**Files:**
- Modify: `README.md`

**Interfaces:**
- Produces: reproducible README quick start; a generated demo MP4 (not committed); measured `--exact` agreement.

- [ ] **Step 1: Write the README**

Sections:
- What it does + one still/frame from the demo.
- Quick start (toolchain, `fetch-fonts.ps1`, `dict build`, `render`, `report`).
- The method (ink crop → stretch → 16×16/4×4 → coarse-to-fine MSE).
- Output layout and `--layout panel`.
- Stats example.
- Performance numbers from Task 14.
- Limitations: font-corpus statistics differ from the reference video; same-machine determinism; SDR/8-bit/progressive; no color emoji; no GPU yet.
- Credits and licenses (Bad Apple composition/arrangement; Unifont, Jigmo, Noto; OFL/CC0).

- [ ] **Step 2: Measure fast vs exact agreement**

```powershell
cargo run --release -- render -i demo-src.mp4 -d fonts.gmd --no-encode --limit 60 --grid 4x3 --dump-codepoints fast.tsv
cargo run --release -- render -i demo-src.mp4 -d fonts.gmd --no-encode --limit 60 --grid 4x3 --exact --dump-codepoints exact.tsv
```

Compare the codepoint columns and record the agreement percentage in README.

- [ ] **Step 3: Generate the demo**

```powershell
ffmpeg -v error -y -f lavfi -i "testsrc2=s=640x480:r=30:d=10" -f lavfi -i "sine=frequency=440:duration=10" -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest demo/synthetic.mp4
cargo run --release -- render -i demo/synthetic.mp4 -d fonts.gmd -o demo/synthetic-4x3.mp4 --grid 4x3 --layout panel --cell-height 360 --dump-codepoints demo/synthetic.tsv --stats demo/synthetic.json
cargo run --release -- report demo/synthetic.tsv
```

Expected: playable 10-second MP4 with 4×3 glyph cells and a right-hand panel; report prints totals.

- [ ] **Step 4: Full verification**

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test integration
```

Expected: all clean. Fix warnings rather than allow them.

- [ ] **Step 5: Final commit**

```powershell
git add README.md
git commit -m "docs: add reproduction guide and demo workflow"
```

---

## Self-Review Notes

- Spec coverage: input envelope (Task 7), dictionary build (Tasks 2–4), matching speed (Tasks 5–6), rendering/panel (Tasks 10–11), statistics (Task 12), CLI (Task 13), fonts/reproducibility (Tasks 3, 14), limits and docs (Task 15), tests for all review-focus items.
- The `render_for_test` helper exists specifically so integration tests do not depend on the built binary path; the CLI and tests share `render_impl`.
- `memmap2` is declared but not used in v1: keep `Dictionary::read` as a plain `Vec` read, and drop the dependency if `cargo clippy` flags it. (A dependency being unused does not warn, but removing it keeps the crate honest.)
- If `fontdue` rejects a CFF/OTF font, switch `rasterize_stretched` to `swash` in Task 2 and keep the same `Coverage` interface; every later task is unaffected.
