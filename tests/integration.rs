use std::path::{Path, PathBuf};
use std::process::Command;

fn test_font() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GLYPHMOSAIC_TEST_FONT") {
        return Some(p.into());
    }
    ["fonts/Jigmo.ttf", "C:/Windows/Fonts/arial.ttf"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

fn make_clip(path: &Path, audio: bool) {
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=96x72:r=10:d=0.5",
    ]);
    if audio {
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.5",
            "-c:a",
            "aac",
        ]);
    }
    cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-shortest"])
        .arg(path);
    assert!(cmd.status().unwrap().success());
}

#[test]
fn end_to_end_mosaic_and_panel() {
    let Some(font) = test_font() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.mp4");
    make_clip(&input, true);

    let opts = glyphmosaic::dict::build::BuildOptions {
        font_paths: vec![font],
        include_pua: false,
        min_ink: 1,
        jobs: 2,
    };
    let dict = glyphmosaic::dict::build::build(&opts).unwrap();
    let dict_path = dir.path().join("fonts.gmd");
    dict.write(&dict_path).unwrap();

    let out = dir.path().join("out.mp4");
    glyphmosaic::cli::render_for_test(
        &input,
        &dict_path,
        &out,
        glyphmosaic::render::RenderConfig {
            grid_cols: 4,
            grid_rows: 3,
            cell_height: 24,
            layout: glyphmosaic::render::Layout::Panel,
            panel_width: 80,
            cache_mb: 8,
        },
        8,
        false,
        3,
    )
    .unwrap();

    let info = glyphmosaic::video::probe::probe(&out).unwrap();
    assert_eq!(info.width % 2, 0);
    assert_eq!(info.height % 2, 0);
    assert!(info.frame_count.unwrap_or(0) <= 5);
}

#[test]
fn no_audio_input_succeeds() {
    let Some(font) = test_font() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.mp4");
    make_clip(&input, false);
    let opts = glyphmosaic::dict::build::BuildOptions {
        font_paths: vec![font],
        include_pua: false,
        min_ink: 1,
        jobs: 2,
    };
    let dict = glyphmosaic::dict::build::build(&opts).unwrap();
    let dict_path = dir.path().join("fonts.gmd");
    dict.write(&dict_path).unwrap();
    let out = dir.path().join("out.mp4");
    glyphmosaic::cli::render_for_test(
        &input,
        &dict_path,
        &out,
        glyphmosaic::render::RenderConfig {
            grid_cols: 4,
            grid_rows: 3,
            cell_height: 24,
            layout: glyphmosaic::render::Layout::Mosaic,
            panel_width: 80,
            cache_mb: 8,
        },
        8,
        false,
        2,
    )
    .unwrap();
    assert!(!glyphmosaic::video::probe::probe(&out).unwrap().has_audio);
}

#[test]
fn missing_font_reports_fix() {
    let dir = tempfile::tempdir().unwrap();
    let dict_path = dir.path().join("bad.gmd");
    let mut d = glyphmosaic::Dictionary::new(glyphmosaic::dict::DictHeader {
        version: 1,
        cell_feature: 16,
        coarse_feature: 4,
        unicode_version: "17.0".into(),
        fonts: vec![glyphmosaic::fonts::FontRecord {
            path: "nowhere.ttf".into(),
            name: "nowhere".into(),
            sha256: "00".repeat(32),
        }],
    });
    d.push('A', 0, [1u8; 256], [1u8; 16]);
    d.write(&dict_path).unwrap();
    let input = dir.path().join("in.mp4");
    make_clip(&input, false);
    let err = glyphmosaic::cli::render_for_test(
        &input,
        &dict_path,
        &dir.path().join("o.mp4"),
        glyphmosaic::render::RenderConfig {
            grid_cols: 2,
            grid_rows: 2,
            cell_height: 24,
            layout: glyphmosaic::render::Layout::Mosaic,
            panel_width: 80,
            cache_mb: 8,
        },
        1,
        false,
        1,
    )
    .err()
    .expect("expected an error")
    .to_string();
    assert!(err.contains("--fonts"), "unexpected error: {err}");
}
