//! System audio output for decoded AirPlay PCM.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use shairplay::{AudioFormat, AudioHandler, AudioSession};
use tracing::{info, warn};

const MAX_BUFFER_MILLIS: usize = 750;

/// Keeps the OS audio stream alive and exposes the handler used by shairplay.
pub struct AudioOutputRuntime {
    handler: Arc<SystemAudioHandler>,
    _stream: Option<cpal::Stream>,
    sample_rate: Option<u32>,
    channels: Option<u8>,
}

impl AudioOutputRuntime {
    pub fn start() -> Self {
        let host = cpal::default_host();
        let Some(device) = host.default_output_device() else {
            warn!("No default audio output device; AirPlay audio will be discarded");
            return Self::disabled();
        };

        let config = match device.default_output_config() {
            Ok(config) => config,
            Err(error) => {
                warn!(%error, "Could not query the default audio output format; AirPlay audio will be discarded");
                return Self::disabled();
            }
        };

        let sample_rate = config.sample_rate();
        let channels = config.channels();
        let max_samples = (sample_rate as usize * MAX_BUFFER_MILLIS / 1000) * channels as usize;
        let ring = Arc::new(Mutex::new(AudioRing::new(max_samples.max(1))));
        let stream_config: cpal::StreamConfig = config.clone().into();

        let stream = match config.sample_format() {
            SampleFormat::I8 => build_output_stream::<i8>(&device, &stream_config, ring.clone()),
            SampleFormat::I16 => build_output_stream::<i16>(&device, &stream_config, ring.clone()),
            SampleFormat::I32 => build_output_stream::<i32>(&device, &stream_config, ring.clone()),
            SampleFormat::I64 => build_output_stream::<i64>(&device, &stream_config, ring.clone()),
            SampleFormat::U8 => build_output_stream::<u8>(&device, &stream_config, ring.clone()),
            SampleFormat::U16 => build_output_stream::<u16>(&device, &stream_config, ring.clone()),
            SampleFormat::U32 => build_output_stream::<u32>(&device, &stream_config, ring.clone()),
            SampleFormat::U64 => build_output_stream::<u64>(&device, &stream_config, ring.clone()),
            SampleFormat::F32 => build_output_stream::<f32>(&device, &stream_config, ring.clone()),
            SampleFormat::F64 => build_output_stream::<f64>(&device, &stream_config, ring.clone()),
            _ => {
                warn!(format = ?config.sample_format(), "Unsupported system audio sample format");
                return Self::disabled();
            }
        };

        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                warn!(%error, "Could not start the default audio output device; AirPlay audio will be discarded");
                return Self::disabled();
            }
        };
        if let Err(error) = stream.play() {
            warn!(%error, "Could not start the default audio output device; AirPlay audio will be discarded");
            return Self::disabled();
        }

        let device_name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "default output".to_string());
        info!(
            device = %device_name,
            sample_rate,
            channels,
            "System audio output ready"
        );

        Self {
            handler: Arc::new(SystemAudioHandler {
                ring,
                gain_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
                output_channels: usize::from(channels),
            }),
            _stream: Some(stream),
            sample_rate: Some(sample_rate),
            channels: u8::try_from(channels).ok(),
        }
    }

    pub fn handler(&self) -> Arc<dyn AudioHandler> {
        self.handler.clone()
    }

    pub fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }

    pub fn channels(&self) -> Option<u8> {
        self.channels
    }

    fn disabled() -> Self {
        Self {
            handler: Arc::new(SystemAudioHandler {
                ring: Arc::new(Mutex::new(AudioRing::new(1))),
                gain_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
                output_channels: 2,
            }),
            _stream: None,
            sample_rate: None,
            channels: None,
        }
    }
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Arc<Mutex<AudioRing>>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let Ok(mut ring) = ring.lock() else {
                for sample in data.iter_mut() {
                    *sample = T::from_sample(0.0);
                }
                return;
            };

            for sample in data.iter_mut() {
                *sample = T::from_sample(ring.pop_sample());
            }
        },
        |error| warn!(%error, "System audio output error"),
        None,
    )
}

struct AudioRing {
    samples: VecDeque<f32>,
    max_samples: usize,
    session_id: u64,
}

impl AudioRing {
    fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
            session_id: 0,
        }
    }

    fn push_samples(&mut self, input: &[f32], gain: f32) {
        if input.len() >= self.max_samples {
            self.samples.clear();
            self.samples.extend(
                input[input.len() - self.max_samples..]
                    .iter()
                    .map(|sample| sample * gain),
            );
            return;
        }

        let overflow = self
            .samples
            .len()
            .saturating_add(input.len())
            .saturating_sub(self.max_samples);
        if overflow > 0 {
            self.samples.drain(..overflow);
        }
        self.samples
            .extend(input.iter().map(|sample| sample * gain));
    }

    fn pop_sample(&mut self) -> f32 {
        self.samples.pop_front().unwrap_or(0.0)
    }

    fn clear(&mut self) {
        self.samples.clear();
    }
}

struct SystemAudioHandler {
    ring: Arc<Mutex<AudioRing>>,
    gain_bits: Arc<AtomicU32>,
    output_channels: usize,
}

impl AudioHandler for SystemAudioHandler {
    fn audio_init(&self, format: AudioFormat) -> Box<dyn AudioSession> {
        let session_id = if let Ok(mut ring) = self.ring.lock() {
            ring.clear();
            ring.session_id = ring.session_id.wrapping_add(1);
            ring.session_id
        } else {
            0
        };
        info!(
            sample_rate = format.sample_rate,
            channels = format.channels,
            bits = format.bits,
            "AirPlay audio stream established"
        );
        Box::new(SystemAudioSession {
            ring: self.ring.clone(),
            gain_bits: self.gain_bits.clone(),
            source_channels: usize::from(format.channels),
            output_channels: self.output_channels,
            session_id,
        })
    }

    fn on_volume(&self, volume: f32) {
        let gain = if volume <= -144.0 {
            0.0
        } else {
            10.0_f32.powf(volume.clamp(-30.0, 0.0) / 20.0)
        };
        self.gain_bits.store(gain.to_bits(), Ordering::Release);
    }

    fn on_client_connected(&self, addr: &str) {
        info!(%addr, "AirPlay client connected");
    }

    fn on_client_disconnected(&self, addr: &str) {
        info!(%addr, "AirPlay client disconnected");
    }

    fn on_error(&self, error: &shairplay::ShairplayError) {
        warn!(%error, "AirPlay connection error");
    }
}

struct SystemAudioSession {
    ring: Arc<Mutex<AudioRing>>,
    gain_bits: Arc<AtomicU32>,
    source_channels: usize,
    output_channels: usize,
    session_id: u64,
}

impl AudioSession for SystemAudioSession {
    fn audio_process(&mut self, samples: &[f32]) {
        if self.source_channels == 0 || self.output_channels == 0 {
            return;
        }
        let gain = f32::from_bits(self.gain_bits.load(Ordering::Acquire));
        if let Ok(mut ring) = self.ring.lock() {
            if ring.session_id != self.session_id {
                return;
            }
            if self.source_channels == self.output_channels {
                let complete = samples.len() / self.source_channels * self.source_channels;
                ring.push_samples(&samples[..complete], gain);
            } else {
                let mut output = vec![0.0; self.output_channels];
                for frame in samples.chunks_exact(self.source_channels) {
                    output.fill(0.0);
                    if self.output_channels == 1 {
                        output[0] = frame.iter().sum::<f32>() / self.source_channels as f32;
                    } else if self.source_channels == 1 {
                        output[0] = frame[0];
                        output[1] = frame[0];
                    } else {
                        let shared = self.source_channels.min(self.output_channels);
                        output[..shared].copy_from_slice(&frame[..shared]);
                    }
                    ring.push_samples(&output, gain);
                }
            }
        }
    }

    fn audio_flush(&mut self) {
        if let Ok(mut ring) = self.ring.lock() {
            if ring.session_id == self.session_id {
                ring.clear();
            }
        }
    }
}

impl Drop for SystemAudioSession {
    fn drop(&mut self) {
        self.audio_flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(output_channels: usize) -> SystemAudioHandler {
        SystemAudioHandler {
            ring: Arc::new(Mutex::new(AudioRing::new(100))),
            gain_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            output_channels,
        }
    }

    fn format(channels: u8) -> AudioFormat {
        AudioFormat {
            codec: shairplay::AudioCodec::Pcm,
            bits: 32,
            channels,
            sample_rate: 48000,
        }
    }

    fn drain(handler: &SystemAudioHandler) -> Vec<f32> {
        handler.ring.lock().unwrap().samples.drain(..).collect()
    }

    #[test]
    fn stereo_keeps_frame_order_and_silences_additional_device_channels() {
        let handler = handler(6);
        let mut session = handler.audio_init(format(2));
        session.audio_process(&[0.25, -0.5, 0.75, -1.0]);
        assert_eq!(
            drain(&handler),
            [0.25, -0.5, 0.0, 0.0, 0.0, 0.0, 0.75, -1.0, 0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn mono_duplicates_left_right_and_stereo_downmixes_to_mono() {
        let stereo_handler = handler(2);
        let mut session = stereo_handler.audio_init(format(1));
        session.audio_process(&[0.25, -0.75]);
        assert_eq!(drain(&stereo_handler), [0.25, 0.25, -0.75, -0.75]);

        let mono_handler = handler(1);
        let mut session = mono_handler.audio_init(format(2));
        session.audio_process(&[0.25, 0.75, -0.75, 0.25]);
        assert_eq!(drain(&mono_handler), [0.5, -0.25]);
    }

    #[test]
    fn volume_and_session_boundaries_discard_stale_samples() {
        let handler = handler(2);
        let mut first = handler.audio_init(format(2));
        first.audio_process(&[0.25, -0.5]);
        first.audio_flush();
        assert!(drain(&handler).is_empty());
        first.audio_process(&[0.25, -0.5]);
        drop(first);
        assert!(drain(&handler).is_empty());

        handler.ring.lock().unwrap().push_samples(&[0.5, 0.5], 1.0);
        let mut second = handler.audio_init(format(2));
        assert!(drain(&handler).is_empty());
        handler.on_volume(-20.0);
        second.audio_process(&[1.0, -1.0]);
        assert_eq!(drain(&handler), [0.1, -0.1]);
        handler.on_volume(-144.0);
        second.audio_process(&[1.0, -1.0]);
        assert_eq!(drain(&handler), [0.0, 0.0]);
    }

    #[test]
    fn replacing_session_ignores_old_packets_and_old_session_cleanup() {
        let handler = handler(2);
        let mut old = handler.audio_init(format(2));
        let mut current = handler.audio_init(format(2));
        current.audio_process(&[0.25, -0.5]);
        old.audio_process(&[1.0, 1.0]);
        old.audio_flush();
        drop(old);
        assert_eq!(drain(&handler), [0.25, -0.5]);
    }
}
