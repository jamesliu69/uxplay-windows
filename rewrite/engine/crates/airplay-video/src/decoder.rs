//! Decoder abstraction; backends are wired in by `airplayd`.

use crate::EncodedFrame;

/// A decoded-video sink. Implementations decode `EncodedFrame`s (H.264/H.265
/// Annex-B or NAL lists) and present them somewhere (window, texture, null).
pub trait Decoder: Send {
    /// Prepare the decoder for a codec (`h264`/`h265`) and optional SPS/PPS.
    fn configure(&mut self, codec: &str, extradata: Option<&[u8]>) -> Result<(), DecodeError>;

    /// Submit one encoded frame.
    fn submit(&mut self, frame: &EncodedFrame) -> Result<(), DecodeError>;

    /// Release backend resources.
    fn flush(&mut self) -> Result<(), DecodeError>;
}

/// Errors produced by decoder backends.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("decoder not configured")]
    NotConfigured,
    #[error("backend error: {0}")]
    Backend(String),
}

/// Dependency-free no-op decoder (default). Accepts and discards frames;
/// useful for tests and for headless builds.
#[derive(Debug, Default)]
pub struct NoopDecoder {
    frames: u64,
}

impl NoopDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frames_seen(&self) -> u64 {
        self.frames
    }
}

impl Decoder for NoopDecoder {
    fn configure(&mut self, _codec: &str, _extradata: Option<&[u8]>) -> Result<(), DecodeError> {
        Ok(())
    }

    fn submit(&mut self, _frame: &EncodedFrame) -> Result<(), DecodeError> {
        self.frames += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DecodeError> {
        Ok(())
    }
}

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg {
    //! FFmpeg-based decoding backend (libavcodec -> D3D11VA/OpenGL output).
    //!
    //! TODO(next milestone): link `ffmpeg-sys`/system FFmpeg, wrap
    //! `AVCodecContext` with H.264/H.265 Annex-B parsing, expose zero-copy
    //! D3D11 textures for the renderer. Not implemented yet; requires system
    //! dependencies so the default build stays dependency-free.
    use super::{DecodeError, Decoder};
    use crate::EncodedFrame;

    pub struct FfmpegDecoder;

    impl FfmpegDecoder {
        pub fn new() -> Result<Self, DecodeError> {
            Err(DecodeError::Backend(
                "ffmpeg backend not yet implemented".into(),
            ))
        }
    }

    impl Decoder for FfmpegDecoder {
        fn configure(
            &mut self,
            _codec: &str,
            _extradata: Option<&[u8]>,
        ) -> Result<(), DecodeError> {
            Err(DecodeError::Backend(
                "ffmpeg backend not yet implemented".into(),
            ))
        }

        fn submit(&mut self, _frame: &EncodedFrame) -> Result<(), DecodeError> {
            Err(DecodeError::Backend(
                "ffmpeg backend not yet implemented".into(),
            ))
        }

        fn flush(&mut self) -> Result<(), DecodeError> {
            Ok(())
        }
    }
}

#[cfg(all(test, feature = "ffmpeg"))]
mod ffmpeg_tests {
    use super::ffmpeg::FfmpegDecoder;

    #[test]
    fn ffmpeg_backend_reports_unimplemented() {
        let d = FfmpegDecoder::new();
        assert!(d.is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_decoder_counts_frames() {
        let mut d = NoopDecoder::new();
        d.configure("h264", None).unwrap();
        d.submit(&EncodedFrame {
            timestamp: 1,
            nal_units: vec![vec![0x65]],
            is_idr: true,
        })
        .unwrap();
        d.submit(&EncodedFrame {
            timestamp: 2,
            nal_units: vec![vec![0x41]],
            is_idr: false,
        })
        .unwrap();
        d.flush().unwrap();
        assert_eq!(d.frames_seen(), 2);
    }
}
