use anyhow::{bail, Context, Result};
use std::io::{BufReader, ErrorKind, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use crate::video::{display_dims, VideoInfo};

pub struct FrameReader {
    pub info: VideoInfo,
    child: Child,
    stdout: BufReader<ChildStdout>,
    frame_size: usize,
    frame_w: u32,
    frame_h: u32,
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
            "fps={}/{},scale=trunc(iw*sar/2)*2:ih,setsar=1",
            info.fps_num, info.fps_den
        );
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-vf"])
            .arg(&filter)
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| "failed to start ffmpeg (is ffmpeg installed?)")?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture ffmpeg stdout"))?;

        Ok(Self {
            info,
            child,
            stdout: BufReader::new(stdout),
            frame_size: frame_w as usize * frame_h as usize * 3,
            frame_w,
            frame_h,
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
                    if filled == 0 {
                        return Ok(false);
                    }
                    bail!("truncated frame: got {filled} of {} bytes", buf.len());
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
}
