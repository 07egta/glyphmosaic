use anyhow::{bail, Context, Result};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    Aac,
    Copy,
    None,
}

#[derive(Clone, Copy, Debug)]
pub struct EncodeOptions {
    pub crf: u8,
    pub audio: AudioMode,
    pub fps_num: u32,
    pub fps_den: u32,
}

pub fn mp4_audio_copy_supported(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "aac" | "mp3" | "ac3" | "eac3" | "alac"
    )
}

pub fn validate_audio_copy(codec: Option<&str>) -> Result<()> {
    match codec {
        Some(c) if !mp4_audio_copy_supported(c) => {
            bail!("audio codec {c} cannot be copied into MP4; use --audio aac")
        }
        _ => Ok(()),
    }
}

pub struct Encoder {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    frame_size: usize,
    finished: bool,
}

impl Encoder {
    pub fn new(
        out: &Path,
        input: &Path,
        width: u32,
        height: u32,
        opts: &EncodeOptions,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("output dimensions must be non-zero");
        }
        if width % 2 != 0 || height % 2 != 0 {
            bail!("H.264 output dimensions must be even, got {width}x{height}");
        }
        if opts.audio == AudioMode::Copy {
            let info = crate::video::probe::probe(input)?;
            validate_audio_copy(info.audio_codec.as_deref())?;
        }

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s",
        ])
        .arg(format!("{width}x{height}"))
        .args(["-r"])
        .arg(format!("{}/{}", opts.fps_num, opts.fps_den))
        .args(["-i", "-"]);

        if opts.audio != AudioMode::None {
            cmd.arg("-i").arg(input);
            cmd.args(["-map", "0:v:0", "-map", "1:a:0?"]);
        }

        cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-crf"])
            .arg(opts.crf.to_string())
            .args(["-pix_fmt", "yuv420p"]);

        match opts.audio {
            AudioMode::Aac => {
                cmd.args(["-c:a", "aac", "-b:a", "192k"]);
            }
            AudioMode::Copy => {
                cmd.args(["-c:a", "copy"]);
            }
            AudioMode::None => {}
        }

        cmd.args(["-shortest", "-movflags", "+faststart"])
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .with_context(|| "failed to start ffmpeg (is ffmpeg installed?)")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture ffmpeg stdin"))?;
        Ok(Self {
            child,
            stdin: Some(BufWriter::with_capacity(1 << 20, stdin)),
            frame_size: width as usize * height as usize * 3,
            finished: false,
        })
    }

    pub fn write(&mut self, rgb: &[u8]) -> Result<()> {
        if rgb.len() != self.frame_size {
            bail!(
                "frame buffer is {} bytes, expected {}",
                rgb.len(),
                self.frame_size
            );
        }
        self.stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg stdin is already closed"))?
            .write_all(rgb)
            .context("failed to write frame to ffmpeg")
    }

    pub fn finish(mut self) -> Result<()> {
        if let Some(mut writer) = self.stdin.take() {
            writer.flush().context("failed to flush ffmpeg stdin")?;
        }
        let status = self.child.wait().context("failed to wait for ffmpeg")?;
        self.finished = true;
        if !status.success() {
            bail!("ffmpeg exited with {status}");
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

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
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x48:r=10:d=0.5",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&input)
            .status()
            .unwrap();
        assert!(ok.success());

        let out = dir.path().join("out.mp4");
        let opts = EncodeOptions {
            crf: 18,
            audio: AudioMode::None,
            fps_num: 10,
            fps_den: 1,
        };
        let mut enc = Encoder::new(&out, &input, 64, 48, &opts).unwrap();
        for _ in 0..5 {
            enc.write(&vec![0u8; 64 * 48 * 3]).unwrap();
        }
        enc.finish().unwrap();

        let info = crate::video::probe::probe(&out).unwrap();
        assert_eq!((info.width, info.height), (64, 48));
        assert!(!info.has_audio);
    }
}
