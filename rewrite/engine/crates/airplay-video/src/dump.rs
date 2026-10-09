//! `.h264`/`.h265` Annex-B file dumping for capture-based testing.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::{AnnexbStream, EncodedFrame};

/// Writes received frames as an Annex-B elementary stream, for later
/// analysis with `ffprobe`/`ffmpeg` or byte comparison in tests.
pub struct H264FileDumper {
    path: std::path::PathBuf,
    file: Option<File>,
    annexb: AnnexbStream,
    frames: u64,
}

impl H264FileDumper {
    /// Create (or truncate) the dump file lazily on the first frame.
    pub fn new(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Fail fast on unwritable targets even though writing is lazy.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path,
            file: None,
            annexb: AnnexbStream::new(),
            frames: 0,
        })
    }

    /// Append a frame to the dump.
    pub fn write_frame(&mut self, frame: &EncodedFrame) -> std::io::Result<()> {
        if self.file.is_none() {
            self.file = Some(File::create(&self.path)?);
        }
        self.annexb.push_frame(frame);
        let bytes = self.annexb.take();
        let file = self.file.as_mut().expect("file created above");
        file.write_all(&bytes)?;
        self.frames += 1;
        Ok(())
    }

    /// Number of frames written so far.
    pub fn frames_written(&self) -> u64 {
        self.frames
    }

    /// Flush to disk.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ts: u32, nals: &[&[u8]]) -> EncodedFrame {
        EncodedFrame {
            timestamp: ts,
            nal_units: nals.iter().map(|n| n.to_vec()).collect(),
            is_idr: true,
        }
    }

    #[test]
    fn writes_annexb_stream() {
        let dir = std::env::temp_dir().join(format!("airplay-video-test-{}", std::process::id()));
        let path = dir.join("capture.h264");
        let mut dumper = H264FileDumper::new(&path).unwrap();
        dumper
            .write_frame(&frame(1, &[&[0x67, 0x42], &[0x68, 0xce]]))
            .unwrap();
        dumper.write_frame(&frame(2, &[&[0x65, 0x88]])).unwrap();
        dumper.flush().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes,
            vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88,]
        );
        assert_eq!(dumper.frames_written(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
