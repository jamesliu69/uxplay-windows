//! NTP timestamp helpers (cf. `libuxplay`'s `lib/raop_ntp.c`).
//!
//! AirPlay sync uses 64-bit NTP timestamps: upper 32 bits are whole seconds
//! since 1900-01-01, lower 32 bits are the fractional second.

/// Nanoseconds per second (cf. `SECOND_IN_NSECS` in `raop_ntp.c`).
pub const SECOND_IN_NSECS: u64 = 1_000_000_000;
/// Seconds between the NTP epoch (1900) and the Unix epoch (1970).
pub const NTP_UNIX_OFFSET_SECS: u64 = 2_208_988_800;

/// A 64-bit NTP timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct NtpTimestamp(pub u64);

impl NtpTimestamp {
    /// Split into (whole seconds, 32-bit fraction).
    pub fn parts(self) -> (u32, u32) {
        ((self.0 >> 32) as u32, (self.0 & 0xffff_ffff) as u32)
    }

    /// Build from (whole seconds, fraction).
    pub fn from_parts(secs: u32, frac: u32) -> Self {
        Self((u64::from(secs) << 32) | u64::from(frac))
    }

    /// Whole milliseconds since the NTP epoch.
    pub fn as_millis(self) -> u64 {
        let (secs, frac) = self.parts();
        u64::from(secs) * 1_000 + (u64::from(frac) * 1_000 >> 32)
    }

    /// Build from milliseconds since the NTP epoch.
    pub fn from_millis(ms: u64) -> Self {
        let secs = (ms / 1_000) as u32;
        let frac = (((ms % 1_000) << 32) / 1_000) as u32;
        Self::from_parts(secs, frac)
    }

    /// Build from a Unix-epoch millisecond timestamp.
    pub fn from_unix_millis(unix_ms: u64) -> Self {
        Self::from_millis(unix_ms + NTP_UNIX_OFFSET_SECS * 1_000)
    }

    /// Difference `self - other` in milliseconds (saturates at 0).
    pub fn saturating_sub_ms(self, other: Self) -> u64 {
        self.as_millis().saturating_sub(other.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_round_trip() {
        let t = NtpTimestamp::from_parts(0x1234_5678, 0x8000_0000);
        assert_eq!(t.parts(), (0x1234_5678, 0x8000_0000));
    }

    #[test]
    fn half_second_fraction_is_500ms() {
        let t = NtpTimestamp::from_parts(100, 0x8000_0000);
        assert_eq!(t.as_millis(), 100_500);
    }

    #[test]
    fn millis_round_trip_within_1ms() {
        for ms in [0u64, 1, 999, 1_000, 1_700_000_000_123] {
            let back = NtpTimestamp::from_millis(ms).as_millis();
            assert!((back as i64 - ms as i64).abs() <= 1, "ms={ms} back={back}");
        }
    }

    #[test]
    fn unix_epoch_maps_to_ntp_offset() {
        let t = NtpTimestamp::from_unix_millis(0);
        assert_eq!(t.parts().0 as u64, NTP_UNIX_OFFSET_SECS);
    }

    #[test]
    fn saturating_sub_clamps() {
        let a = NtpTimestamp::from_millis(100);
        let b = NtpTimestamp::from_millis(200);
        assert_eq!(a.saturating_sub_ms(b), 0);
        assert_eq!(b.saturating_sub_ms(a), 100);
    }
}
