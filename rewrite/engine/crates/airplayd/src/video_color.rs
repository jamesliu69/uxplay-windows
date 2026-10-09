//! H.264 VUI color information and stride-aware YUV conversion.

use h264_reader::nal::{sps::SeqParameterSet, Nal, RefNal};
use openh264::formats::YUVSource;
use tracing::{info, warn};
use yuv::{YuvPlanarImage, YuvRange, YuvStandardMatrix};

#[derive(Clone, Copy, Debug)]
pub struct VideoColor {
    range: YuvRange,
    matrix: YuvStandardMatrix,
}

impl Default for VideoColor {
    fn default() -> Self {
        Self {
            range: YuvRange::Limited,
            matrix: YuvStandardMatrix::Bt601,
        }
    }
}

impl VideoColor {
    /// Returns None when the packet contains no valid SPS. Keep the previous
    /// stream's color information until a new SPS explicitly replaces it.
    pub fn from_annex_b(annex_b: &[u8]) -> Option<Self> {
        let mut color = None;
        for unit in openh264::nal_units(annex_b) {
            let unit = unit
                .strip_prefix(&[0, 0, 0, 1])
                .or_else(|| unit.strip_prefix(&[0, 0, 1]))
                .unwrap_or(unit);
            if unit.first().is_none_or(|byte| byte & 31 != 7) {
                continue;
            }
            let nal = RefNal::new(unit, &[], true);
            let Ok(sps) = SeqParameterSet::from_bits(nal.rbsp_bits()) else {
                warn!("could not parse H.264 SPS color information");
                continue;
            };
            let signal = sps
                .vui_parameters
                .as_ref()
                .and_then(|vui| vui.video_signal_type.as_ref());
            let range = if signal.is_some_and(|signal| signal.video_full_range_flag) {
                YuvRange::Full
            } else {
                YuvRange::Limited
            };
            let coefficients = signal
                .and_then(|signal| signal.colour_description.as_ref())
                .map(|description| description.matrix_coefficients);
            let matrix = match coefficients {
                Some(1) => YuvStandardMatrix::Bt709,
                Some(4) => YuvStandardMatrix::Fcc,
                Some(7) => YuvStandardMatrix::Smpte240,
                Some(9) => YuvStandardMatrix::Bt2020,
                None | Some(2 | 5 | 6) => YuvStandardMatrix::Bt601,
                Some(other) => {
                    warn!(
                        coefficients = other,
                        "unsupported H.264 color matrix; using BT.601"
                    );
                    YuvStandardMatrix::Bt601
                }
            };
            info!(profile=?sps.profile_idc, level=sps.level_idc,
                dimensions=?sps.pixel_dimensions(), fps=?sps.fps(), ?signal, ?range, ?matrix,
                "Mirror H264 format");
            color = Some(Self { range, matrix });
        }
        color
    }

    pub fn to_pixels(self, frame: &impl YUVSource) -> Result<Vec<u32>, yuv::YuvError> {
        let (width, height) = frame.dimensions();
        let (y_stride, u_stride, v_stride) = frame.strides();
        let image = YuvPlanarImage {
            y_plane: frame.y(),
            y_stride: y_stride as u32,
            u_plane: frame.u(),
            u_stride: u_stride as u32,
            v_plane: frame.v(),
            v_stride: v_stride as u32,
            width: width as u32,
            height: height as u32,
        };
        let mut pixels = vec![0u32; width * height];
        // BGRA bytes are 0xAARRGGBB u32 values on Windows x64 / ARM64.
        // Write directly into the output allocation, avoiding the RGB copy.
        yuv::yuv420_to_bgra(
            &image,
            bytemuck::cast_slice_mut(&mut pixels),
            (width * 4) as u32,
            self.range,
            self.matrix,
        )?;
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::formats::YUVSlices;

    #[test]
    fn full_and_limited_range_keep_neutral_levels_with_padded_rows() {
        let y = [
            0, 16, 64, 128, 235, 255, 99, 99, 0, 16, 64, 128, 235, 255, 99, 99,
        ];
        let u = [128, 128, 128, 99];
        let v = [128, 128, 128, 99];
        let frame = YUVSlices::new((&y, &u, &v), (6, 2), (8, 4, 4));
        for (range, expected) in [
            (YuvRange::Full, [0u8, 16, 64, 128, 235, 255]),
            (YuvRange::Limited, [0u8, 0, 56, 130, 255, 255]),
        ] {
            let color = VideoColor {
                range,
                matrix: YuvStandardMatrix::Bt709,
            };
            let pixels = color.to_pixels(&frame).unwrap();
            assert_eq!(pixels.len(), 12);
            for (pixel, expected) in pixels.into_iter().zip(expected.into_iter().cycle()) {
                for channel in [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8] {
                    assert!(
                        channel.abs_diff(expected) <= 1,
                        "neutral {expected}: got {channel}"
                    );
                }
            }
        }
    }
}
