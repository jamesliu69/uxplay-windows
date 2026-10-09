//! FFmpeg H.264 decoder, isolated from the network and window message pump.

use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tracing::{info, warn};

pub(crate) struct DecodedFrame {
    pub pixels: Vec<u32>,
    pub width: usize,
    pub height: usize,
    pub decoded_at: Instant,
}

#[derive(Clone, Default)]
pub(crate) struct DecoderControl(Arc<Mutex<Option<Child>>>);

impl DecoderControl {
    pub fn stop(&self) {
        // Do not hold the lock while waiting for the process or reader thread.
        let child = self.0.lock().unwrap().take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(crate) struct FfmpegDecoder {
    input: Option<ChildStdin>,
    control: DecoderControl,
    readers: Vec<JoinHandle<()>>,
}

impl FfmpegDecoder {
    pub fn start(mut on_frame: impl FnMut(DecodedFrame) + Send + 'static) -> io::Result<Self> {
        let path = ffmpeg_path()?;
        let mut command = Command::new(&path);
        command.args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-nostats",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-fpsprobesize",
            "0",
            "-threads",
            "2",
            "-thread_type",
            "slice",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-an",
            "-sn",
            "-dn",
            "-c:v",
            "ppm",
            "-pix_fmt",
            "rgb24",
            "-fps_mode",
            "passthrough",
            "-noautoscale",
            "-flush_packets",
            "1",
            "-f",
            "image2pipe",
            "pipe:1",
        ]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn()?;
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        info!(path = %path.display(), pid = child.id(), "FFmpeg mirror decoder started");
        let control = DecoderControl(Arc::new(Mutex::new(Some(child))));
        let output = thread::spawn(move || {
            let mut reader = BufReader::with_capacity(256 * 1024, stdout);
            let mut report = Instant::now();
            let mut pictures = 0u64;
            loop {
                match read_picture(&mut reader) {
                    Ok(Some(frame)) => {
                        pictures += 1;
                        let (width, height) = (frame.width, frame.height);
                        on_frame(frame);
                        if report.elapsed() >= Duration::from_secs(5) {
                            info!(
                                pictures,
                                width,
                                height,
                                fps = pictures as f64 / report.elapsed().as_secs_f64(),
                                "FFmpeg decode statistics"
                            );
                            pictures = 0;
                            report = Instant::now();
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        warn!(%error, "FFmpeg output reader stopped");
                        break;
                    }
                }
            }
        });
        let diagnostics = thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => warn!(message = %line, "FFmpeg decoder diagnostic"),
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            input: Some(input),
            control,
            readers: vec![output, diagnostics],
        })
    }

    pub fn control(&self) -> DecoderControl {
        self.control.clone()
    }

    pub fn write(&mut self, annex_b: &[u8], complete_picture: bool) -> io::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "FFmpeg input closed"))?;
        input.write_all(annex_b)?;
        if complete_picture {
            // The transport supplies complete access units. A trailing AUD
            // lets FFmpeg's elementary-stream parser release the current AU
            // immediately instead of waiting for another phone frame.
            input.write_all(&[0, 0, 0, 1, 9, 0xf0])?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn finish(mut self) -> io::Result<()> {
        drop(self.input.take());
        let child = self.control.0.lock().unwrap().take();
        let status = child.map(|mut child| child.wait()).transpose()?;
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        if status.is_some_and(|status| !status.success()) {
            return Err(io::Error::other("FFmpeg failed while draining video"));
        }
        Ok(())
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        self.control.stop();
        drop(self.input.take());
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn ffmpeg_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("UXPLAY_FFMPEG_PATH") {
        return Ok(PathBuf::from(path));
    }
    let beside_engine = std::env::current_exe()?.with_file_name("ffmpeg.exe");
    if beside_engine.is_file() {
        return Ok(beside_engine);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "FFmpeg runtime missing: place ffmpeg.exe beside airplayd.exe, or set UXPLAY_FFMPEG_PATH",
    ))
}

fn read_picture(reader: &mut impl BufRead) -> io::Result<Option<DecodedFrame>> {
    let Some(magic) = read_token(reader)? else {
        return Ok(None);
    };
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid FFmpeg PPM frame header",
        )
    };
    if magic != "P6" {
        return Err(invalid());
    }
    let mut number = || -> io::Result<usize> {
        read_token(reader)?
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())
    };
    let width = number()?;
    let height = number()?;
    let maximum = number()?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 || maximum != 255 {
        return Err(invalid());
    }
    let mut rgb = vec![0u8; width * height * 3];
    reader.read_exact(&mut rgb)?;
    let pixels = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .map(|rgb| (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]))
        .collect();
    Ok(Some(DecodedFrame {
        pixels,
        width,
        height,
        decoded_at: Instant::now(),
    }))
}

fn read_token(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut token = Vec::new();
    loop {
        let mut byte = [0];
        if reader.read(&mut byte)? == 0 {
            return if token.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete PPM header",
                ))
            };
        }
        if byte[0].is_ascii_whitespace() {
            if !token.is_empty() {
                break;
            }
        } else {
            token.push(byte[0]);
            if token.len() > 32 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "oversized PPM header token",
                ));
            }
        }
    }
    String::from_utf8(token)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-ASCII PPM header"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_stream_preserves_pixels_and_dynamic_dimensions() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[255, 0, 0, 0, 255, 0]);
        data.extend_from_slice(b"P6\n1 2\n255\n");
        data.extend_from_slice(&[0, 0, 255, 10, 20, 30]);
        let mut reader = std::io::Cursor::new(data);
        let first = read_picture(&mut reader).unwrap().unwrap();
        assert_eq!((first.width, first.height), (2, 1));
        assert_eq!(first.pixels, [0xff0000, 0x00ff00]);
        let second = read_picture(&mut reader).unwrap().unwrap();
        assert_eq!((second.width, second.height), (1, 2));
        assert_eq!(second.pixels, [0x0000ff, 0x0a141e]);
        assert!(read_picture(&mut reader).unwrap().is_none());
    }

    #[test]
    fn rejects_invalid_dimensions_and_incomplete_pixel_data() {
        for data in [
            &b"P6\n9000 1\n255\n"[..],
            &b"P6\n0 1\n255\n"[..],
            &b"P6\n2 1\n255\n\0\0\0"[..],
        ] {
            assert!(read_picture(&mut std::io::Cursor::new(data)).is_err());
        }
    }
}
