use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub frame_count: Option<u64>,
    pub duration: Option<f64>,
    pub has_audio: bool,
    pub pix_fmt: String,
    pub field_order: String,
    pub color_transfer: Option<String>,
    pub audio_codec: Option<String>,
    pub sar_num: u32,
    pub sar_den: u32,
    pub rotation: i32,
}

fn parse_rate(s: &str) -> Option<(u32, u32)> {
    let (num, den) = s.split_once('/')?;
    let num: u32 = num.parse().ok()?;
    let den: u32 = den.parse().ok()?;
    (num != 0 && den != 0).then_some((num, den))
}

fn parse_sar(s: Option<&str>) -> (u32, u32) {
    s.and_then(|s| s.split_once(':'))
        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
        .filter(|&(a, b)| a != 0 && b != 0)
        .unwrap_or((1, 1))
}

fn rotation_of(stream: &Value) -> i32 {
    let mut rotation = stream
        .get("side_data_list")
        .and_then(|v| v.as_array())
        .and_then(|list| {
            list.iter()
                .find_map(|item| item.get("rotation").and_then(|v| v.as_f64()))
        });
    if rotation.is_none() {
        rotation = stream
            .pointer("/tags/rotate")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
    }
    let rounded = rotation.unwrap_or(0.0).round() as i64;
    let normalized = rounded.rem_euclid(360);
    match normalized {
        45..=134 => 90,
        135..=224 => 180,
        225..=314 => 270,
        _ => 0,
    }
}

fn parse_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

fn parse_f64(value: Option<&Value>) -> Option<f64> {
    value.and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

pub fn probe(path: &Path) -> Result<VideoInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .with_context(|| "failed to run ffprobe (is ffmpeg installed?)")?;
    if !output.status.success() {
        bail!(
            "ffprobe failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let json: Value = serde_json::from_slice(&output.stdout).context("invalid ffprobe JSON")?;
    let streams = json
        .get("streams")
        .and_then(|s| s.as_array())
        .ok_or_else(|| anyhow::anyhow!("ffprobe returned no streams"))?;

    let video = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"))
        .ok_or_else(|| anyhow::anyhow!("input has no video stream"))?;
    let audio = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"));

    let width = video
        .get("width")
        .and_then(|v| v.as_u64())
        .context("video stream has no width")? as u32;
    let height = video
        .get("height")
        .and_then(|v| v.as_u64())
        .context("video stream has no height")? as u32;
    let (fps_num, fps_den) = video
        .get("avg_frame_rate")
        .and_then(|v| v.as_str())
        .and_then(parse_rate)
        .or_else(|| {
            video
                .get("r_frame_rate")
                .and_then(|v| v.as_str())
                .and_then(parse_rate)
        })
        .unwrap_or((30, 1));

    let duration = parse_f64(video.get("duration"))
        .or_else(|| parse_f64(json.pointer("/format/duration")));
    let mut frame_count = parse_u64(video.get("nb_frames"));
    if frame_count.is_none() {
        if let Some(d) = duration {
            frame_count = Some((d * fps_num as f64 / fps_den as f64).round() as u64);
        }
    }

    let (sar_num, sar_den) = parse_sar(video.get("sample_aspect_ratio").and_then(|v| v.as_str()));
    Ok(VideoInfo {
        width,
        height,
        fps_num,
        fps_den,
        frame_count,
        duration,
        has_audio: audio.is_some(),
        pix_fmt: video
            .get("pix_fmt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        field_order: video
            .get("field_order")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        color_transfer: video
            .get("color_transfer")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        audio_codec: audio
            .and_then(|a| a.get("codec_name"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        sar_num,
        sar_den,
        rotation: rotation_of(video),
    })
}

pub fn validate_input(info: &VideoInfo) -> Result<()> {
    let pix = info.pix_fmt.to_ascii_lowercase();
    let hdr = matches!(
        info.color_transfer.as_deref(),
        Some("smpte2084") | Some("arib-std-b67")
    );
    if hdr || pix.contains("10le") || pix.contains("10be") || pix.contains("12") || pix.contains("p010")
    {
        bail!("HDR/10-bit input is not supported; convert to 8-bit SDR first");
    }
    if info.field_order != "progressive" && info.field_order != "unknown" {
        bail!("interlaced input is not supported; deinterlace first");
    }
    Ok(())
}

pub fn display_dims(
    width: u32,
    height: u32,
    sar_num: u32,
    sar_den: u32,
    rotation: i32,
) -> (u32, u32) {
    let (sn, sd) = if sar_num == 0 || sar_den == 0 {
        (1, 1)
    } else {
        (sar_num, sar_den)
    };
    let mut w = ((width as u64 * sn as u64 + sd as u64 / 2) / sd as u64) as u32;
    w = (w & !1).max(2);
    let mut h = (height & !1).max(2);
    if rotation.rem_euclid(180) == 90 {
        std::mem::swap(&mut w, &mut h);
    }
    (w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_clip(path: &std::path::Path) {
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=96x72:r=10:d=0.5",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
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
            width: 96,
            height: 72,
            fps_num: 10,
            fps_den: 1,
            frame_count: Some(5),
            duration: Some(0.5),
            has_audio: false,
            pix_fmt: "yuv420p10le".into(),
            field_order: "progressive".into(),
            color_transfer: None,
            audio_codec: None,
            sar_num: 1,
            sar_den: 1,
            rotation: 0,
        };
        assert!(validate_input(&info).unwrap_err().to_string().contains("10-bit"));
        info.pix_fmt = "yuv420p".into();
        info.field_order = "tt".into();
        assert!(validate_input(&info)
            .unwrap_err()
            .to_string()
            .contains("interlaced"));
    }

    #[test]
    fn display_dims_normalizes_sar_and_rotation() {
        assert_eq!(display_dims(720, 480, 1, 1, 0), (720, 480));
        assert_eq!(display_dims(720, 480, 2, 1, 0), (1440, 480));
        assert_eq!(display_dims(720, 480, 1, 1, 90), (480, 720));
        assert_eq!(display_dims(720, 480, 2, 1, 90), (480, 1440));
    }
}
