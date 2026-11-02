use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

use crate::video::{display_dims, VideoInfo};

pub struct FrameReader {
    pub info: VideoInfo,
    child: Child,
    stdout: BufReader<ChildStdout>,
    frame_size: usize,
    frame_w: u32,
    frame_h: u32,
    finished: bool,
    stderr: Option<JoinHandle<Vec<String>>>,
}

impl FrameReader {
    pub fn open(path: &Path, info: VideoInfo) -> Result<Self> {
        let (frame_w, frame_h) = display_dims(
            info.width,
            info.height,
            info.sar_num,
            info.sar_den,
            info.rotation,
        );
        let filter = format!(
            "fps={}/{},scale={frame_w}:{frame_h},setsar=1",
            info.fps_num, info.fps_den
        );
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-xerror", "-err_detect", "explode", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-vf"])
            .arg(&filter)
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| "failed to start ffmpeg (is ffmpeg installed?)")?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture ffmpeg stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture ffmpeg stderr"))?;
        let stderr = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut lines = Vec::new();
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("{line}");
                lines.push(line);
            }
            lines
        });

        Ok(Self {
            info,
            child,
            stdout: BufReader::new(stdout),
            frame_size: frame_w as usize * frame_h as usize * 3,
            frame_w,
            frame_h,
            finished: false,
            stderr: Some(stderr),
        })
    }

    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    pub fn frame_dims(&self) -> (u32, u32) {
        (self.frame_w, self.frame_h)
    }

    pub fn read_frame(&mut self, buf: &mut [u8]) -> Result<bool> {
        if buf.len() != self.frame_size {
            bail!(
                "frame buffer is {} bytes, expected {}",
                buf.len(),
                self.frame_size
            );
        }
        let mut filled = 0usize;
        while filled < buf.len() {
            match self.stdout.read(&mut buf[filled..]) {
                Ok(0) => {
                    if filled != 0 {
                        bail!("truncated frame: got {filled} of {} bytes", buf.len());
                    }
                    if self.finished {
                        return Ok(false);
                    }
                    self.finished = true;
                    let status = self
                        .child
                        .wait()
                        .context("failed to wait for ffmpeg decoder")?;
                    let lines = self
                        .stderr
                        .take()
                        .map(|handle| handle.join().unwrap_or_default())
                        .unwrap_or_default();
                    let failed = !status.success()
                        || lines.iter().any(|line| {
                            let lower = line.to_lowercase();
                            lower.contains("premature") || lower.contains("error")
                        });
                    if failed {
                        bail!("ffmpeg decoder failed ({}): {}", status, lines.join(" | "));
                    }
                    return Ok(false);
                }
                Ok(n) => filled += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("failed to read frame from ffmpeg"),
            }
        }
        Ok(true)
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.stderr.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::probe::probe;

    #[test]
    fn reads_expected_frame_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=64x48:r=10:d=0.5",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(ok.success());
        let info = probe(&path).unwrap();
        let mut reader = FrameReader::open(&path, info).unwrap();
        let mut buf = vec![0u8; reader.frame_size()];
        let mut n = 0;
        while reader.read_frame(&mut buf).unwrap() {
            n += 1;
        }
        assert_eq!(n, 5);
    }

    #[test]
    fn handles_odd_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=101x75:r=10:d=0.5",
                "-c:v",
                "ffv1",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(ok.success());
        let info = probe(&path).unwrap();
        let mut reader = FrameReader::open(&path, info).unwrap();
        assert_eq!(reader.frame_dims(), (100, 74));
        let mut buf = vec![0u8; reader.frame_size()];
        let mut n = 0;
        while reader.read_frame(&mut buf).unwrap() {
            n += 1;
        }
        assert_eq!(n, 5);
    }

    #[test]
    fn reports_decoder_failure_on_truncated_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=64x64:r=10:d=2",
                "-c:v",
                "ffv1",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(ok.success());
        let info = probe(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let bad = dir.path().join("bad.mkv");
        std::fs::write(&bad, &bytes[..bytes.len() / 3]).unwrap();

        let mut reader = FrameReader::open(&bad, info).unwrap();
        let mut buf = vec![0u8; reader.frame_size()];
        let mut failed = false;
        loop {
            match reader.read_frame(&mut buf) {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed, "truncated input must not look like a clean EOF");
    }
}
