use std::path::PathBuf;

pub fn test_font_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GLYPHMOSAIC_TEST_FONT") {
        return Some(PathBuf::from(p));
    }
    [
        "C:/Windows/Fonts/arial.ttf",
        "C:/Windows/Fonts/segoeui.ttf",
        "fonts/Jigmo.ttf",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}
