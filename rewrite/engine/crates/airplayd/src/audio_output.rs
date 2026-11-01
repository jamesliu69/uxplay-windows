//! System audio output for decoded AirPlay PCM.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use shairplay::{AudioFormat, AudioHandler, AudioSession};
use tracing::{info, warn};

const MAX_BUFFER_MILLIS: usize = 750;
const PREFILL_BUFFER_MILLIS: usize = 60;
const TRANSITION_MILLIS: usize = 5;
const OUTPUT_RECOVERY_INTERVAL: Duration = Duration::from_millis(500);

/// 開啟輸出裝置前 ring 暫用的取樣率；僅影響等待期間的緩衝尺寸，
/// 實際裝置開啟成功時會以 reconfigure 改成協商格式。
const PENDING_SAMPLE_RATE: usize = 48_000;

/// 與 shairplay 協商、供後續所有（重新）開啟沿用的輸出格式。PCM 進到
/// ring 的速率在接收器生命週期內固定不變（resample 在 shairplay 端），
/// 裝置切換時只能以同一格式重開，由 Windows WASAPI shared mode 轉換成
/// 新裝置的 mix format，不能改用新裝置自己的格式。
#[derive(Clone)]
struct NegotiatedFormat {
    config: cpal::StreamConfig,
    sample_format: SampleFormat,
}

/// 目前是否已有可用的 OS 輸出串流；None 表示仍在等待裝置出現。
struct OpenedOutput {
    /// 保存中的裝置串流；值本身不被讀取，但必須存活到被替換為止
    /// （drop 即停止輸出），因此保留欄位並允許未讀取。
    #[allow(dead_code)]
    stream: cpal::Stream,
    /// 開串流時使用的取樣率；用於偵測 AirPlay PCM 速率變動。
    sample_rate: u32,
    /// 串流所在裝置的識別（DeviceId 或裝置名）；用於偵測預設裝置被切換。
    device_key: String,
}

/// Keeps the OS audio stream alive and exposes the handler used by shairplay.
pub struct AudioOutputRuntime {
    handler: Arc<SystemAudioHandler>,
    _output_worker: OutputWorker,
    sample_rate: Option<u32>,
    channels: Option<u8>,
}

impl AudioOutputRuntime {
    pub fn start() -> Self {
        // 裝置可能隨時熱插入，開啟失敗不再永久停用音訊：ring 先以暫用
        // 格式建立，OutputWorker 會持續重試，成功後把 ring 重設為協商
        // 格式。session 的聲道映射在每次收包時讀 ring 現值，因此
        // reconfigure 之後新舊 session 都能對上目前的輸出裝置。
        let ring = Arc::new(Mutex::new(AudioRing::new(PENDING_SAMPLE_RATE, 2)));
        let negotiated = default_device_format();
        // 尚沒有 AirPlay session 時，PCM 速率以預設裝置（或暫用值）為準；
        // 第一個 session 建立後 worker 會自動對齊其宣布的速率。
        let pcm_rate = Arc::new(AtomicU32::new(
            negotiated
                .as_ref()
                .map(|format| format.config.sample_rate)
                .unwrap_or(PENDING_SAMPLE_RATE as u32),
        ));
        let handler = Arc::new(SystemAudioHandler {
            ring: ring.clone(),
            gain_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            pcm_rate: pcm_rate.clone(),
        });
        let needs_rebuild = Arc::new(AtomicBool::new(true));

        // 第一次開啟成功時固定協商格式：shairplay 之後供給的 PCM 速率
        // 與聲道即以此為準，裝置熱插入／切換都沿用同一份。
        let opened = negotiated
            .as_ref()
            .and_then(|format| {
                open_output_on_default(format, default_device(), &ring, &needs_rebuild).ok()
            });
        let sample_rate = negotiated.as_ref().map(|format| format.config.sample_rate);
        let channels = negotiated
            .as_ref()
            .and_then(|format| u8::try_from(format.config.channels).ok());

        let _output_worker = OutputWorker::start(
            opened,
            negotiated,
            ring,
            needs_rebuild,
            pcm_rate,
        );
        Self {
            handler,
            _output_worker,
            sample_rate,
            channels,
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

    /// Added software buffering to compensate on the video presentation path.
    /// Device/transport latency is separate; disabled audio needs no holdback.
    pub fn buffer_delay(&self) -> Duration {
        buffer_delay_for(self.sample_rate)
    }
}

/// 依「目前是否已有輸出裝置」計算影片端的音訊緩衝補償；尚未有裝置
/// 時不補償，避免影片白白延遲。
fn buffer_delay_for(sample_rate: Option<u32>) -> Duration {
    if sample_rate.is_some() {
        Duration::from_millis(PREFILL_BUFFER_MILLIS as u64)
    } else {
        Duration::ZERO
    }
}

/// Owns the stream outside its callback so an invalidated device can be dropped
/// and reopened without joining the callback thread from inside that callback.
struct OutputWorker {
    shutdown: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl OutputWorker {
    fn start(
        opened: Option<OpenedOutput>,
        negotiated: Option<NegotiatedFormat>,
        ring: Arc<Mutex<AudioRing>>,
        needs_rebuild: Arc<AtomicBool>,
        pcm_rate: Arc<AtomicU32>,
    ) -> Self {
        let (shutdown, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut opened = opened;
            // 尚未固定協商格式（啟動時沒有裝置）時，每次成功開啟的
            // 裝置格式會成為之後沿用的對象；固定之後裝置切換一律以
            // 同一格式重開，僅取樣率跟隨 AirPlay PCM 實際供給速率。
            let mut negotiated = negotiated;
            let mut failed_attempts = 0_u64;
            while matches!(
                receiver.recv_timeout(OUTPUT_RECOVERY_INTERVAL),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                let current_device = default_device();
                if negotiated.is_none() {
                    negotiated = default_device_format();
                }
                let Some(base) = negotiated.clone() else {
                    needs_rebuild.store(true, Ordering::Release);
                    failed_attempts = throttle_retry(
                        failed_attempts,
                        "no default audio output device yet",
                    );
                    continue;
                };
                let target = align_sample_rate(&base, pcm_rate.load(Ordering::Acquire));
                let refresh = refresh_reason(
                    opened.as_ref().map(|opened| opened.device_key.as_str()),
                    current_device.as_ref().and_then(device_key).as_deref(),
                    opened
                        .as_ref()
                        .is_some_and(|opened| opened.sample_rate != target.config.sample_rate),
                    &needs_rebuild,
                );
                if refresh == RefreshReason::None {
                    failed_attempts = 0;
                    continue;
                }
                // 純速率對齊不丟棄待播 PCM：先以目標速率重算 ring 計時；
                // 之後 open_output_on_default 成功時會做完整的 reconfigure。
                if refresh == RefreshReason::RateRealigned {
                    if let Ok(mut ring_guard) = ring.lock() {
                        ring_guard.retune_rate(target.config.sample_rate as usize);
                    }
                }
                let result = recover_output(&mut opened, || {
                    open_output_on_default(&target, current_device.clone(), &ring, &needs_rebuild)
                });
                match result {
                    Ok(()) => {
                        failed_attempts = 0;
                        match refresh {
                            RefreshReason::DeviceChanged => {
                                info!("Default audio output device changed; stream moved");
                            }
                            RefreshReason::RateRealigned => {
                                info!(
                                    sample_rate = target.config.sample_rate,
                                    "Output sample rate realigned to the AirPlay stream"
                                );
                            }
                            _ => {}
                        }
                    }
                    Err(error) => {
                        failed_attempts = throttle_retry(
                            failed_attempts,
                            &format!("could not reopen audio output: {error}"),
                        );
                    }
                }
            }
        });
        Self {
            shutdown,
            thread: Some(thread),
        }
    }
}

/// 把協商格式的取樣率換成 AirPlay PCM 實際速率；聲道數與取樣格式沿用
/// 裝置值。WASAPI shared mode 會把此格式轉換成裝置的 mix format，
/// 因此串流速率對齊 PCM 供給速率即可，不需要在應用層重取樣。
fn align_sample_rate(base: &NegotiatedFormat, pcm_rate: u32) -> NegotiatedFormat {
    NegotiatedFormat {
        config: cpal::StreamConfig {
            channels: base.config.channels,
            buffer_size: base.config.buffer_size,
            sample_rate: pcm_rate,
        },
        sample_format: base.sample_format,
    }
}

/// 裝置缺席期間的節流重試記錄：每 20 次失敗（約 10 秒）回報一次，
/// 避免洗版 GUI 記錄，並回傳遞增後的失敗次數。
fn throttle_retry(failed_attempts: u64, message: &str) -> u64 {
    if failed_attempts.is_multiple_of(20) {
        warn!("{message}; retrying automatically");
    }
    failed_attempts.wrapping_add(1)
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshReason {
    /// 串流運作中、速率已對齊、預設裝置也未變：不需要任何動作。
    None,
    /// 預設裝置被切換：把串流搬到新裝置上繼續播。
    DeviceChanged,
    /// AirPlay session 宣布的 PCM 速率與串流不同：以新速率重開串流，
    /// 避免 ring 消耗速率與 PCM 供給速率長期失配。
    RateRealigned,
    /// needs_rebuild 已被串流錯誤或初始失敗設定：需要（重新）開啟。
    Rebuild,
}

/// 決定這個週期要不要重建輸出串流。優先順序：串流錯誤（Rebuild）>
/// 速率失配（RateRealigned）> 預設裝置切換（DeviceChanged）。裝置識別
/// 任一無法取得時保守地不切換裝置。
fn refresh_reason(
    current: Option<&str>,
    default: Option<&str>,
    rate_realigned_needed: bool,
    needs_rebuild: &AtomicBool,
) -> RefreshReason {
    if needs_rebuild.load(Ordering::Acquire) {
        return RefreshReason::Rebuild;
    }
    if rate_realigned_needed {
        return RefreshReason::RateRealigned;
    }
    let Some(current) = current else {
        return RefreshReason::None;
    };
    match default {
        Some(default) if default != current => RefreshReason::DeviceChanged,
        _ => RefreshReason::None,
    }
}

/// 目前系統預設輸出裝置；連續兩次查詢到不同裝置時回 None，代表預設
/// 裝置正在切換，這個週期不開串流。
fn default_device() -> Option<cpal::Device> {
    let host = cpal::default_host();
    let first = host.default_output_device()?;
    let second = host.default_output_device()?;
    (device_key(&first)? == device_key(&second)?).then_some(second)
}

/// 預設輸出裝置的格式；連續兩次查詢一致才回傳，避免在裝置切換瞬間
/// 讀到新舊交錯的資訊。
fn default_device_format() -> Option<NegotiatedFormat> {
    let device = default_device()?;
    let first = device.default_output_config().ok()?;
    let second = device.default_output_config().ok()?;
    if first.sample_rate() != second.sample_rate()
        || first.channels() != second.channels()
        || first.sample_format() != second.sample_format()
    {
        return None;
    }
    Some(NegotiatedFormat {
        config: second.clone().into(),
        sample_format: second.sample_format(),
    })
}

/// 裝置的穩定識別字串；DeviceId 在應用程式重啟間保持穩定，適合偵測
/// 「Windows 預設輸出裝置被使用者切換」，無法取得時退回裝置名稱。
fn device_key(device: &cpal::Device) -> Option<String> {
    device
        .id()
        .map(|id| id.to_string())
        .ok()
        .or_else(|| device.description().map(|d| d.name().to_string()).ok())
}

/// 以固定的協商格式在指定裝置上開啟輸出串流，並把 ring 重設為該格式。
/// 成功時清除 needs_rebuild；任何失敗都設為 true，讓 OutputWorker 之後
/// 持續重試，音訊不會因為啟動當下沒有裝置而被永久丟棄。
fn open_output_on_default(
    format: &NegotiatedFormat,
    device: Option<cpal::Device>,
    ring: &Arc<Mutex<AudioRing>>,
    needs_rebuild: &Arc<AtomicBool>,
) -> anyhow::Result<OpenedOutput> {
    let result = open_output_on_default_inner(format, device, ring, needs_rebuild);
    needs_rebuild.store(result.is_err(), Ordering::Release);
    result
}

fn open_output_on_default_inner(
    format: &NegotiatedFormat,
    device: Option<cpal::Device>,
    ring: &Arc<Mutex<AudioRing>>,
    needs_rebuild: &Arc<AtomicBool>,
) -> anyhow::Result<OpenedOutput> {
    let device = device
        .or_else(default_device)
        .ok_or_else(|| anyhow::anyhow!("no default audio output device"))?;
    let device_key = device_key(&device)
        .ok_or_else(|| anyhow::anyhow!("default audio output device has no stable id"))?;

    let stream = open_output_stream(
        &device,
        &format.config,
        format.sample_format,
        ring.clone(),
        needs_rebuild.clone(),
    )
    .map_err(|error| anyhow::anyhow!("could not start the default audio output device: {error}"))?;

    // 串流實際開起來之後，才把 ring 換成協商的取樣率／聲道數。
    // reconfigure 會清空蓄積的 PCM，並保留 session id。
    if let Ok(mut ring) = ring.lock() {
        ring.reconfigure(
            format.config.sample_rate as usize,
            usize::from(format.config.channels),
        );
    }
    let device_name = device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| "default output".to_string());
    info!(
        device = %device_name,
        sample_rate = format.config.sample_rate,
        channels = format.config.channels,
        prefill_ms = PREFILL_BUFFER_MILLIS,
        "System audio output ready"
    );
    Ok(OpenedOutput {
        stream,
        sample_rate: format.config.sample_rate,
        device_key,
    })
}

impl Drop for OutputWorker {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn open_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: SampleFormat,
    ring: Arc<Mutex<AudioRing>>,
    needs_rebuild: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream> {
    let stream = match sample_format {
        SampleFormat::I8 => build_output_stream::<i8>(device, config, ring, needs_rebuild),
        SampleFormat::I16 => build_output_stream::<i16>(device, config, ring, needs_rebuild),
        SampleFormat::I32 => build_output_stream::<i32>(device, config, ring, needs_rebuild),
        SampleFormat::I64 => build_output_stream::<i64>(device, config, ring, needs_rebuild),
        SampleFormat::U8 => build_output_stream::<u8>(device, config, ring, needs_rebuild),
        SampleFormat::U16 => build_output_stream::<u16>(device, config, ring, needs_rebuild),
        SampleFormat::U32 => build_output_stream::<u32>(device, config, ring, needs_rebuild),
        SampleFormat::U64 => build_output_stream::<u64>(device, config, ring, needs_rebuild),
        SampleFormat::F32 => build_output_stream::<f32>(device, config, ring, needs_rebuild),
        SampleFormat::F64 => build_output_stream::<f64>(device, config, ring, needs_rebuild),
        _ => anyhow::bail!("Unsupported system audio sample format: {sample_format:?}"),
    }?;
    stream.play()?;
    Ok(stream)
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Arc<Mutex<AudioRing>>,
    needs_rebuild: Arc<AtomicBool>,
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

            ring.render(data);
        },
        move |error| record_output_error(error, &needs_rebuild),
        None,
    )
}

fn record_output_error(error: cpal::StreamError, needs_rebuild: &AtomicBool) {
    if !matches!(error, cpal::StreamError::BufferUnderrun) {
        needs_rebuild.store(true, Ordering::Release);
    }
    warn!(%error, "System audio output error");
}

fn recover_output<S, E>(
    opened: &mut Option<S>,
    open: impl FnOnce() -> Result<S, E>,
) -> Result<(), E> {
    // 先丟棄舊串流再開新的；ring 的清空與格式重設由 open_default_output
    // 成功時的 reconfigure 完成（舊 PCM 的取樣率／聲道已不適用新裝置）。
    drop(opened.take());
    match open() {
        Ok(replacement) => {
            *opened = Some(replacement);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

struct AudioRing {
    samples: VecDeque<f32>,
    max_samples: usize,
    channels: usize,
    prefill_samples: usize,
    playing: bool,
    transition_frames: usize,
    transition_remaining: usize,
    transition_origin: Vec<f32>,
    last_frame: Vec<f32>,
    session_id: u64,
}

impl AudioRing {
    fn new(sample_rate: usize, channels: usize) -> Self {
        let channels = channels.max(1);
        let max_frames = (sample_rate * MAX_BUFFER_MILLIS / 1000).max(1);
        let prefill_frames = (sample_rate * PREFILL_BUFFER_MILLIS)
            .div_ceil(1000)
            .max(1)
            .min(max_frames);
        let max_samples = max_frames * channels;
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
            channels,
            prefill_samples: prefill_frames * channels,
            playing: false,
            transition_frames: (sample_rate * TRANSITION_MILLIS / 1000).max(1),
            transition_remaining: 0,
            transition_origin: vec![0.0; channels],
            last_frame: vec![0.0; channels],
            session_id: 0,
        }
    }

    /// 依 PCM 實際速率調整緩衝計時（容量/prefill 以毫秒換算）；不清空、
    /// 不改聲道數、不動 session id。緩衝內樣本按 frame 計數，速率變化
    /// 只改變每毫秒消耗的樣本數，不需丟棄已排入的 PCM。
    fn retune_rate(&mut self, sample_rate: usize) {
        self.max_samples = (sample_rate * MAX_BUFFER_MILLIS / 1000).max(1) * self.channels;
        self.prefill_samples = (sample_rate * PREFILL_BUFFER_MILLIS)
            .div_ceil(1000)
            .max(1)
            .saturating_mul(self.channels)
            .min(self.max_samples);
        self.transition_frames = (sample_rate * TRANSITION_MILLIS / 1000).max(1);
    }

    fn reconfigure(&mut self, sample_rate: usize, channels: usize) {
        let session_id = self.session_id;
        *self = AudioRing::new(sample_rate, channels);
        // 換輸出裝置屬於同一條 AirPlay 連線的延續；保留 session id，
        // 讓重新協商前的舊 session 封包仍照常被丟棄。
        self.session_id = session_id;
    }

    fn push_samples(&mut self, input: &[f32], gain: f32) {
        // Only queue complete device frames so overflow cannot swap channels.
        let complete = input.len() / self.channels * self.channels;
        let input = &input[..complete];
        if input.len() >= self.max_samples {
            self.samples.clear();
            if self.playing {
                self.begin_transition();
            }
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
            if self.playing {
                self.begin_transition();
            }
        }
        self.samples
            .extend(input.iter().map(|sample| sample * gain));
    }

    fn begin_transition(&mut self) {
        self.transition_origin.copy_from_slice(&self.last_frame);
        self.transition_remaining = self.transition_frames;
    }

    fn render<T: SizedSample + FromSample<f32>>(&mut self, output: &mut [T]) {
        let mut frames = output.chunks_exact_mut(self.channels);
        for frame in frames.by_ref() {
            // Starting and recovering both need headroom. Otherwise each late
            // packet would repeatedly alternate between PCM and hard silence.
            if !self.playing && self.samples.len() >= self.prefill_samples {
                self.playing = true;
                self.begin_transition();
            }
            if self.playing && self.samples.len() < self.channels {
                self.playing = false;
                self.begin_transition();
            }

            let blend = if self.transition_remaining > 0 {
                (self.transition_frames - self.transition_remaining + 1) as f32
                    / self.transition_frames as f32
            } else {
                1.0
            };
            for (channel, output_sample) in frame.iter_mut().enumerate() {
                let input = if self.playing {
                    self.samples.pop_front().unwrap_or(0.0)
                } else {
                    0.0
                };
                // A short ramp also bridges an overflow jump to newer PCM.
                let sample = self.transition_origin[channel] * (1.0 - blend) + input * blend;
                *output_sample = T::from_sample(sample);
                self.last_frame[channel] = sample;
            }
            self.transition_remaining = self.transition_remaining.saturating_sub(1);
        }
        for sample in frames.into_remainder() {
            *sample = T::from_sample(0.0);
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.playing = false;
        self.transition_remaining = 0;
        self.transition_origin.fill(0.0);
        self.last_frame.fill(0.0);
    }
}

struct SystemAudioHandler {
    ring: Arc<Mutex<AudioRing>>,
    gain_bits: Arc<AtomicU32>,
    /// 最近一次 AirPlay session 宣布的 PCM 取樣率（Hz）。worker 據此把
    /// 串流取樣率對齊 PCM 供給速率，避免 ring 消耗與供給長期失配。
    pcm_rate: Arc<AtomicU32>,
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
        // 記下 shairplay 實際供給的 PCM 速率；裝置串流的速率對齊由
        // OutputWorker 背景處理，收包路徑不做任何裝置操作。
        self.pcm_rate.store(format.sample_rate, Ordering::Release);
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
    session_id: u64,
}

impl AudioSession for SystemAudioSession {
    fn audio_process(&mut self, samples: &[f32]) {
        let gain = f32::from_bits(self.gain_bits.load(Ordering::Acquire));
        if let Ok(mut ring) = self.ring.lock() {
            if ring.session_id != self.session_id {
                return;
            }
            // 輸出聲道數在裝置熱插入／切換時會變動，不能沿用建立 session
            // 當下的快取，需以 ring 現值做單／多聲道映射。
            let output_channels = ring.channels;
            if self.source_channels == 0 || output_channels == 0 {
                return;
            }
            if self.source_channels == output_channels {
                let complete = samples.len() / self.source_channels * self.source_channels;
                ring.push_samples(&samples[..complete], gain);
            } else {
                let mut output = vec![0.0; output_channels];
                for frame in samples.chunks_exact(self.source_channels) {
                    output.fill(0.0);
                    if output_channels == 1 {
                        output[0] = frame.iter().sum::<f32>() / self.source_channels as f32;
                    } else if self.source_channels == 1 {
                        output[0] = frame[0];
                        output[1] = frame[0];
                    } else {
                        let shared = self.source_channels.min(output_channels);
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
            ring: Arc::new(Mutex::new(AudioRing::new(48000, output_channels))),
            gain_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            pcm_rate: Arc::new(AtomicU32::new(48_000)),
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

    fn render_stereo(ring: &mut AudioRing, frames: usize) -> Vec<f32> {
        let mut output = vec![0.0; frames * 2];
        ring.render(&mut output);
        output
    }

    #[test]
    fn invalidated_output_reopens_and_current_session_can_play_again() {
        struct DeviceStream(Arc<AtomicBool>);
        impl Drop for DeviceStream {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let handler = handler(2);
        let mut session = handler.audio_init(format(2));
        handler.on_volume(-20.0);
        session.audio_process(&[1.0, -1.0].repeat(2880));
        let old_dropped = Arc::new(AtomicBool::new(false));
        let mut stream = Some(DeviceStream(old_dropped.clone()));

        // 模擬裝置失效後 worker 的重建路徑：丟棄舊串流、重設 ring 格式
        // （清空蓄積 PCM、保留 session id），再以新串流繼續。
        handler
            .ring
            .lock()
            .unwrap()
            .reconfigure(48000, 2);
        recover_output(&mut stream, || {
            assert!(old_dropped.load(Ordering::Acquire));
            Ok::<_, ()>(DeviceStream(Arc::new(AtomicBool::new(false))))
        })
        .unwrap();
        assert!(old_dropped.load(Ordering::Acquire));
        assert!(stream.is_some());
        let mut ring = handler.ring.lock().unwrap();
        assert_eq!(render_stereo(&mut ring, 10), vec![0.0; 20]);
        drop(ring);

        // Existing AirPlay session and volume survive; old queued PCM does not.
        session.audio_process(&[0.5, -0.5].repeat(2880));
        let resumed = render_stereo(&mut handler.ring.lock().unwrap(), 300);
        assert_eq!(&resumed[598..], &[0.05, -0.05]);
    }

    #[test]
    fn output_recovery_retries_after_device_is_temporarily_absent() {
        let mut stream = Some(1);
        assert_eq!(
            recover_output(&mut stream, || Err("no device")),
            Err("no device")
        );
        assert!(stream.is_none());
        // 裝置缺席期間蓄積的 backlog 由成功路徑的 reconfigure 清空。
        recover_output(&mut stream, || Ok::<_, &str>(2)).unwrap();
        assert_eq!(stream, Some(2));
    }

    #[test]
    fn healthy_output_and_brief_underrun_keep_the_existing_stream() {
        let needs_rebuild = AtomicBool::new(false);
        record_output_error(cpal::StreamError::BufferUnderrun, &needs_rebuild);
        assert!(!needs_rebuild.load(Ordering::Acquire));
        record_output_error(cpal::StreamError::StreamInvalidated, &needs_rebuild);
        assert!(needs_rebuild.load(Ordering::Acquire));
    }

    #[test]
    fn refresh_reason_orders_rebuild_then_rate_then_device_change() {
        let needs_rebuild = AtomicBool::new(true);
        assert_eq!(
            refresh_reason(None, Some("wasapi:a"), true, &needs_rebuild),
            RefreshReason::Rebuild
        );
        needs_rebuild.store(false, Ordering::Release);
        assert_eq!(
            refresh_reason(Some("wasapi:a"), Some("wasapi:a"), false, &needs_rebuild),
            RefreshReason::None
        );
        // 速率失配優先於裝置切換：兩者都會重建串流，但速率對齊先做。
        assert_eq!(
            refresh_reason(Some("wasapi:a"), Some("wasapi:b"), true, &needs_rebuild),
            RefreshReason::RateRealigned
        );
        assert_eq!(
            refresh_reason(Some("wasapi:a"), Some("wasapi:b"), false, &needs_rebuild),
            RefreshReason::DeviceChanged
        );
        // 任一識別無法取得時保守不切換。
        assert_eq!(
            refresh_reason(Some("wasapi:a"), None, false, &needs_rebuild),
            RefreshReason::None
        );
        assert_eq!(
            refresh_reason(None, Some("wasapi:a"), false, &needs_rebuild),
            RefreshReason::None
        );
    }

    #[test]
    fn align_sample_rate_follows_pcm_and_keeps_device_channels() {
        let base = NegotiatedFormat {
            config: cpal::StreamConfig {
                channels: 2,
                buffer_size: cpal::BufferSize::Default,
                sample_rate: 48_000,
            },
            sample_format: SampleFormat::F32,
        };
        let aligned = align_sample_rate(&base, 44_100);
        assert_eq!(aligned.config.sample_rate, 44_100);
        assert_eq!(aligned.config.channels, 2);
        assert_eq!(aligned.sample_format, SampleFormat::F32);
    }

    #[test]
    fn retune_rate_resizes_timing_without_clearing_pending_samples() {
        let mut ring = AudioRing::new(48_000, 2);
        ring.push_samples(&[0.5, -0.5].repeat(480), 1.0); // 10 ms
        ring.retune_rate(96_000);
        assert_eq!(ring.channels, 2);
        assert_eq!(ring.samples.len(), 960);
        // 容量與 prefill 依新速率重算。
        assert_eq!(ring.max_samples, 96_000 * MAX_BUFFER_MILLIS / 1000 * 2);
        assert_eq!(
            ring.prefill_samples,
            96_000 * PREFILL_BUFFER_MILLIS / 1000 * 2
        );
    }

    #[test]
    fn reconfigure_resizes_buffer_keeps_session_id_and_clears_backlog() {
        let handler = handler(2);
        let mut session = handler.audio_init(format(2));
        session.audio_process(&[0.25, -0.5].repeat(3600)); // 1.8 s @48k
        {
            let mut ring = handler.ring.lock().unwrap();
            assert!(!ring.samples.is_empty());
            ring.reconfigure(44_100, 6);
            assert_eq!(ring.channels, 6);
            assert_eq!(ring.max_samples, 44_100 * MAX_BUFFER_MILLIS / 1000 * 6);
            assert!(ring.samples.is_empty());
        }
        // 同一 session 的封包仍被接受，並依新聲道數（6）映射。
        session.audio_process(&[0.5, -0.5, 0.5, -0.5]);
        assert_eq!(
            drain(&handler),
            [0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.5, -0.5, 0.0, 0.0, 0.0, 0.0]
        );
        // reconfigure 保留 session id：換新 session 後，舊 session 的封包
        // 仍照常被拒絕，不會因為格式重設而復活。
        let mut stale = session;
        let mut _current = handler.audio_init(format(2));
        assert!(drain(&handler).is_empty());
        stale.audio_process(&[1.0, 1.0]);
        assert!(drain(&handler).is_empty());
    }

    #[test]
    #[ignore = "requires a real default system audio output device"]
    fn real_audio_device_consumes_pcm_after_output_recovery() {
        let ring = Arc::new(Mutex::new(AudioRing::new(PENDING_SAMPLE_RATE, 2)));
        let pcm_rate = Arc::new(AtomicU32::new(PENDING_SAMPLE_RATE as u32));
        let handler = SystemAudioHandler {
            ring: ring.clone(),
            gain_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            pcm_rate: pcm_rate.clone(),
        };
        let mut session = handler.audio_init(AudioFormat {
            codec: shairplay::AudioCodec::Pcm,
            bits: 32,
            channels: 2,
            sample_rate: 48_000,
        });
        let needs_rebuild = Arc::new(AtomicBool::new(true));
        // 以正式的開啟路徑（含 ring reconfigure）取得串流，再交給 worker。
        let negotiated = default_device_format().unwrap();
        let opened = open_output_on_default(
            &negotiated,
            default_device(),
            &ring,
            &needs_rebuild,
        )
        .unwrap();
        let config = negotiated.config.clone();
        assert!(!needs_rebuild.load(Ordering::Acquire));
        assert_eq!(ring.lock().unwrap().channels, usize::from(config.channels));
        let worker = OutputWorker::start(
            Some(opened),
            Some(negotiated),
            ring.clone(),
            needs_rebuild.clone(),
            pcm_rate,
        );
        // Inject the same error reported by Windows, then reopen a real WASAPI
        // stream. This does not unplug or change the user's output endpoint.
        // One frame cannot reach prefill, so only recovery can clear it.
        session.audio_process(&vec![0.0; usize::from(config.channels)]);
        record_output_error(cpal::StreamError::DeviceNotAvailable, &needs_rebuild);
        assert!(needs_rebuild.load(Ordering::Acquire));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !ring.lock().unwrap().samples.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not recover output"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let sample_count = ring.lock().unwrap().prefill_samples * 4;
        // Silent PCM proves callbacks drain the existing AirPlay session
        // without playing a test tone over the user's current media.
        session.audio_process(&vec![0.0; sample_count]);
        while !ring.lock().unwrap().playing {
            assert!(
                std::time::Instant::now() < deadline,
                "reopened output did not start playback"
            );
            thread::sleep(Duration::from_millis(10));
        }
        while !ring.lock().unwrap().samples.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "reopened output did not consume PCM"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!needs_rebuild.load(Ordering::Acquire));
        drop(worker);
    }

    #[test]
    fn audio_without_a_device_does_not_delay_video() {
        assert_eq!(buffer_delay_for(None), Duration::ZERO);
        assert_eq!(
            buffer_delay_for(Some(48_000)),
            Duration::from_millis(PREFILL_BUFFER_MILLIS as u64)
        );
    }

    #[test]
    fn startup_waits_for_prefill_without_consuming_short_packets() {
        // 1 kHz stereo: 750 ms capacity, with 60 ms needed before playback.
        let mut ring = AudioRing::new(1000, 2);
        ring.push_samples(&[0.5, -0.5].repeat(10), 1.0);
        assert_eq!(render_stereo(&mut ring, 10), vec![0.0; 20]);
        assert_eq!(ring.samples.len(), 20);

        ring.push_samples(&[0.5, -0.5].repeat(50), 1.0);
        let output = render_stereo(&mut ring, 6);
        assert!(output[0] > 0.0 && output[0] < 0.5);
        assert_eq!(&output[10..], &[0.5, -0.5]);
    }

    #[test]
    fn prefill_absorbs_a_short_packet_delay_without_silence() {
        let mut ring = AudioRing::new(1000, 2);
        ring.push_samples(&[0.5, -0.5].repeat(60), 1.0);
        render_stereo(&mut ring, 20);
        // A packet expected after 10 ms arrives after 30 ms instead.
        let delayed = render_stereo(&mut ring, 30);
        assert!(delayed.chunks_exact(2).all(|frame| frame == [0.5, -0.5]));
        ring.push_samples(&[0.5, -0.5].repeat(40), 1.0);
        assert_eq!(render_stereo(&mut ring, 10), [0.5, -0.5].repeat(10));
    }

    #[test]
    fn production_rate_packet_jitter_keeps_playback_continuous() {
        let mut ring = AudioRing::new(48000, 2);
        let packet = [0.5, -0.5].repeat(480); // 10 ms of PCM
        for tick in 0..200 {
            // Deliver one packet every 10 ms, except one arrives 5 ms late.
            if (tick % 2 == 0 && tick != 40) || tick == 41 {
                ring.push_samples(&packet, 1.0);
            }
            let output = render_stereo(&mut ring, 240); // 5 ms OS callback
            if tick >= 12 {
                assert!(
                    output.chunks_exact(2).all(|frame| frame == [0.5, -0.5]),
                    "unexpected dropout at callback {tick}"
                );
            }
        }
    }

    #[test]
    fn oversized_input_retains_complete_multichannel_frames() {
        let mut ring = AudioRing::new(1000, 6);
        let frame = [0.1, -0.2, 0.3, -0.4, 0.5, -0.6];
        let mut input = frame.repeat(800);
        input.extend_from_slice(&[0.9, 0.9]); // incomplete trailing frame
        ring.push_samples(&input, 1.0);
        assert_eq!(ring.samples.len(), 750 * 6);
        let mut output = vec![0.0; 6 * 6];
        ring.render(&mut output);
        assert_eq!(&output[30..], &frame);
    }

    #[test]
    fn underrun_fades_out_instead_of_jumping_to_zero() {
        let mut ring = AudioRing::new(1000, 2);
        ring.push_samples(&[0.5, -0.5].repeat(60), 1.0);
        render_stereo(&mut ring, 60);
        let tail = render_stereo(&mut ring, 6);
        assert!(tail[0] > 0.0 && tail[0] < 0.5);
        assert!(tail.chunks_exact(2).all(|frame| frame[0] == -frame[1]));
        assert!(tail
            .chunks_exact(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>()
            .windows(2)
            .all(|pair| pair[0] >= pair[1]));
        assert_eq!(&tail[10..], &[0.0, 0.0]);
    }

    #[test]
    fn underrun_waits_for_refill_instead_of_playing_each_late_packet() {
        let mut ring = AudioRing::new(1000, 2);
        ring.push_samples(&[0.5, -0.5].repeat(60), 1.0);
        render_stereo(&mut ring, 65);
        ring.push_samples(&[0.5, -0.5].repeat(10), 1.0);
        assert_eq!(render_stereo(&mut ring, 10), vec![0.0; 20]);
        assert_eq!(ring.samples.len(), 20);
        ring.push_samples(&[0.5, -0.5].repeat(50), 1.0);
        let resumed = render_stereo(&mut ring, 6);
        assert!(resumed[0] > 0.0 && resumed[0] < 0.5);
        assert_eq!(&resumed[10..], &[0.5, -0.5]);
    }

    #[test]
    fn overflow_crossfades_without_an_abrupt_stereo_polarity_jump() {
        let mut ring = AudioRing::new(1000, 2);
        ring.push_samples(&[0.5, -0.5].repeat(750), 1.0);
        render_stereo(&mut ring, 10);
        ring.push_samples(&[-0.5, 0.5].repeat(750), 1.0);
        assert_eq!(ring.samples.len(), 1500);
        let output = render_stereo(&mut ring, 6);
        assert!((output[0] - 0.5).abs() <= 0.25);
        assert!(output.chunks_exact(2).all(|frame| frame[0] == -frame[1]));
        assert_eq!(&output[10..], &[-0.5, 0.5]);
    }

    #[test]
    fn flush_discards_transition_audio_and_requires_a_new_prefill() {
        let mut ring = AudioRing::new(1000, 2);
        ring.push_samples(&[0.5, -0.5].repeat(60), 1.0);
        render_stereo(&mut ring, 10);
        ring.clear();
        assert_eq!(render_stereo(&mut ring, 10), vec![0.0; 20]);
        ring.push_samples(&[0.25, -0.25].repeat(10), 1.0);
        assert_eq!(render_stereo(&mut ring, 10), vec![0.0; 20]);
        assert_eq!(ring.samples.len(), 20);
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
