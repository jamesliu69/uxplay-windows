//! Video (screen mirroring) support for AirPlay 2.
//!
//! The library receives encrypted H.264/H.265 video packets, decrypts them,
//! and delivers raw NAL units to the application via [`VideoSession`].
//! The application is responsible for decoding and rendering.

use bytes::Bytes;

/// Classification of a video packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketKind {
    /// AVC (H.264) decoder configuration record.
    AvcC,
    /// HEVC (H.265) decoder configuration record.
    HvcC,
    /// Encoded video payload (decrypted by the library).
    Payload,
    /// Auxiliary binary plist data.
    Plist,
    /// Unknown packet type.
    Other(u16),
}

/// A decrypted video packet delivered to the application.
#[derive(Debug)]
pub struct VideoPacket {
    /// Packet classification.
    pub kind: PacketKind,
    /// Presentation timestamp (NTP-based, in stream time units).
    pub timestamp: u64,
    /// Packet payload (raw NAL units for Payload, config bytes for AvcC/HvcC).
    pub payload: Bytes,
}

/// Factory for creating video sessions. Implement this to receive video data.
pub trait VideoHandler: Send + Sync + 'static {
    /// Virtual display dimensions and maximum frame rate advertised to senders.
    fn display_config(&self) -> (u32, u32, u32) {
        (
            super::config::MIRRORING_WIDTH as u32,
            super::config::MIRRORING_HEIGHT as u32,
            super::config::MIRRORING_FPS as u32,
        )
    }

    /// Called when a new video stream is established.
    fn video_init(&self) -> Box<dyn VideoSession>;
}

/// Per-stream video session receiving decrypted video packets.
///
/// Created by [`VideoHandler::video_init`]. Dropped when the stream ends.
pub trait VideoSession: Send + Sync {
    /// Called for each decrypted video packet.
    fn on_video(&mut self, packet: VideoPacket);

    /// Receivers with a bounded decode queue can await capacity without blocking
    /// a runtime worker or discarding interdependent compressed frames.
    fn on_video_async(
        &mut self,
        packet: VideoPacket,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.on_video(packet) })
    }

    /// Called when the video stream ends (client disconnected or error).
    fn on_video_end(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioFormat, AudioHandler, AudioSession, RaopServer};
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    struct TestVideo;
    impl VideoHandler for TestVideo {
        fn display_config(&self) -> (u32, u32, u32) {
            (1280, 720, 30)
        }
        fn video_init(&self) -> Box<dyn VideoSession> {
            Box::new(TestVideo)
        }
    }
    impl VideoSession for TestVideo {
        fn on_video(&mut self, _packet: VideoPacket) {}
    }
    struct SilentAudio;
    impl AudioHandler for SilentAudio {
        fn audio_init(&self, _format: AudioFormat) -> Box<dyn AudioSession> {
            Box::new(SilentAudio)
        }
    }
    impl AudioSession for SilentAudio {
        fn audio_process(&mut self, _samples: &[f32]) {}
    }

    #[tokio::test]
    async fn get_info_advertises_selected_display_dimensions_and_fps() {
        let mut receiver = RaopServer::builder()
            .port(0)
            .name("Display config test")
            .video_handler(Arc::new(TestVideo))
            .build(Arc::new(SilentAudio))
            .unwrap();
        receiver.start().await.unwrap();
        let port = receiver.service_info().port;
        let mut connection = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        connection
            .write_all(b"GET /info RTSP/1.0\r\nCSeq: 1\r\n\r\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(connection);
        let body = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("Content-Length")
                {
                    content_length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).await.unwrap();
            body
        })
        .await;
        drop(reader);
        receiver.stop().await;
        let info = plist::Value::from_reader(std::io::Cursor::new(body.unwrap())).unwrap();
        let display = &info.as_dictionary().unwrap()["displays"]
            .as_array()
            .unwrap()[0];
        let display = display.as_dictionary().unwrap();
        assert_eq!(display["widthPixels"].as_unsigned_integer(), Some(1280));
        assert_eq!(display["heightPixels"].as_unsigned_integer(), Some(720));
        assert_eq!(display["maxFPS"].as_unsigned_integer(), Some(30));
    }

    #[tokio::test]
    async fn unsupported_audio_setup_returns_error_instead_of_false_success() {
        let mut receiver = RaopServer::builder()
            .port(0)
            .name("Unsupported audio test")
            .build(Arc::new(SilentAudio))
            .unwrap();
        receiver.start().await.unwrap();
        let port = receiver.service_info().port;
        let mut connection = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let stream = plist::Dictionary::from_iter([
            ("type".to_string(), plist::Value::Integer(96u32.into())),
            ("ct".to_string(), plist::Value::Integer(255u32.into())),
        ]);
        let setup = plist::Dictionary::from_iter([(
            "streams".to_string(),
            plist::Value::Array(vec![plist::Value::Dictionary(stream)]),
        )]);
        let mut body = Vec::new();
        plist::to_writer_binary(&mut body, &setup).unwrap();
        let headers = format!(
            "SETUP /stream RTSP/1.0\r\nCSeq: 7\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        connection.write_all(headers.as_bytes()).await.unwrap();
        connection.write_all(&body).await.unwrap();
        let mut reader = BufReader::new(connection);
        let mut headers = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
        })
        .await
        .unwrap();
        drop(reader);
        receiver.stop().await;
        assert!(
            headers.starts_with("RTSP/1.0 415 Unsupported Media Type\r\n"),
            "{headers}"
        );
        assert!(headers.contains("CSeq: 7\r\n"));
    }
}
