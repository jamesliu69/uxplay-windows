//! Raw AAC-ELD access-unit decoding for AirPlay screen mirroring.

/// AAC-ELD output parameters. AirPlay uses the ELD core without low-delay SBR.
#[derive(Debug, Clone)]
pub(crate) struct AacEldConfig {
    pub(crate) sample_rate: u32,
    pub(crate) num_channels: u8,
    pub(crate) frame_length: usize,
}

pub(crate) struct AacEldDecoder {
    decoder: rusty_aac::AacDecoder,
    config: AacEldConfig,
}

impl AacEldDecoder {
    pub(crate) fn new(config: AacEldConfig) -> Option<Self> {
        let frequency_index = match config.sample_rate {
            44100 => 4,
            48000 => 3,
            _ => return None,
        };
        if !(1..=2).contains(&config.num_channels) || !matches!(config.frame_length, 480 | 512) {
            return None;
        }
        // MPEG-4 AudioSpecificConfig: escaped object type 39 (31, 7),
        // frequency index, channel configuration, frameLengthFlag, no
        // resilience/SBR/extensions, epConfig=0. For 44100/2/480: F8 E8 50 00.
        let bits: u32 = (31 << 27)
            | (7 << 21)
            | (frequency_index << 17)
            | (u32::from(config.num_channels) << 13)
            | (u32::from(config.frame_length == 480) << 12);
        let decoder = rusty_aac::AacDecoder::with_config_bytes(&bits.to_be_bytes()).ok()?;
        Some(Self { decoder, config })
    }

    pub(crate) fn decode(&mut self, payload: &[u8]) -> Option<Vec<f32>> {
        let decoded = self.decoder.decode(payload, None).ok()?;
        if decoded.sample_rate != self.config.sample_rate
            || decoded.channels != u16::from(self.config.num_channels)
            || decoded.samples.len()
                != self.config.frame_length * usize::from(self.config.num_channels)
            || decoded.samples.iter().any(|sample| !sample.is_finite())
        {
            return None;
        }
        Some(decoded.samples)
    }
}
