//! mDNS advertisement for the AirPlay receiver.
//!
//! Publishes `_airplay._tcp` and `_raop._tcp` service instances with the TXT
//! records expected by AirPlay senders (iPhone/iPad/Mac). Implemented on top
//! of the pure-Rust `mdns-sd` crate so no Bonjour/mDNSResponder service is
//! required.
//!
//! TXT record contents follow `libuxplay`'s `lib/dnssd.c`
//! (`dnssd_register_airplay` / `dnssd_register_raop`) and `lib/dnssdint.h`
//! exactly.
//!
//! License: GPL-3.0-or-later.

use std::collections::HashMap;

use mdns_sd::{ServiceDaemon, ServiceInfo};
use thiserror::Error;
use tracing::info;

/// Model string advertised to senders.
pub const ADVERTISED_MODEL: &str = "AppleTV3,2";
/// Protocol version advertised to senders.
pub const ADVERTISED_VERSION: &str = "220.68";
/// AirPlay feature bits (UxPlay `FEATURES_1`, bit 27 "legacy pairing" ON).
pub const FEATURES_1: u32 = 0x5A7F_FEE6;
/// Second 32 bits of features (UxPlay `FEATURES_2`).
pub const FEATURES_2: u32 = 0x0;
/// Fixed `pi` value advertised by UxPlay.
pub const AIRPLAY_PI: &str = "2e388006-13ba-4041-9a67-25dd4a43d536";
/// Default RTSP/mirroring port (`_airplay._tcp`).
pub const DEFAULT_AIRPLAY_PORT: u16 = 7100;
/// Default audio (RAOP) port (`_raop._tcp`).
pub const DEFAULT_RAOP_PORT: u16 = 7000;

/// Errors from the mDNS advertisement layer.
#[derive(Debug, Error)]
pub enum MdnsError {
    /// The mdns-sd daemon could not be created.
    #[error("mdns daemon unavailable: {0}")]
    Daemon(#[from] mdns_sd::Error),
    /// A hardware address was malformed.
    #[error("invalid hardware address: {0}")]
    BadHwAddr(String),
}

/// Configuration for one advertisement session.
#[derive(Debug, Clone)]
pub struct AdvertiserConfig {
    /// Device name shown in the sender's AirPlay picker.
    pub name: String,
    /// 6-byte hardware address used for `deviceid` derivation.
    pub hw_addr: [u8; 6],
    /// Hex-encoded Ed25519 public key (`pk` TXT value).
    pub pk_hex: String,
    /// Port for `_airplay._tcp` (mirroring/RTSP).
    pub airplay_port: u16,
    /// Port for `_raop._tcp` (audio).
    pub raop_port: u16,
}

impl Default for AdvertiserConfig {
    fn default() -> Self {
        Self {
            name: "uxplay-rs".into(),
            hw_addr: [0x48, 0x5d, 0x35, 0x8b, 0x9a, 0x20],
            pk_hex: String::new(),
            airplay_port: DEFAULT_AIRPLAY_PORT,
            raop_port: DEFAULT_RAOP_PORT,
        }
    }
}

impl AdvertiserConfig {
    /// `deviceid` value: lowercase `aa:bb:...` (cf. `utils_hwaddr_airplay`).
    pub fn device_id(&self) -> String {
        self.hw_addr
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    /// Service-instance prefix for RAOP: UPPERCASE hex without separators
    /// (cf. `utils_hwaddr_raop`); instance name is `<HEX>@<name>`.
    pub fn raop_instance_name(&self) -> String {
        let hex: String = self.hw_addr.iter().map(|b| format!("{b:02X}")).collect();
        format!("{hex}@{}", self.name)
    }

    /// Shared `features` TXT value: `0x%X,0x%X`.
    pub fn features_txt() -> String {
        format!("0x{FEATURES_1:X},0x{FEATURES_2:X}")
    }
}

/// TXT properties for `_airplay._tcp` (cf. `dnssd_register_airplay`).
pub fn airplay_txt(cfg: &AdvertiserConfig) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("deviceid".into(), cfg.device_id());
    m.insert("features".into(), AdvertiserConfig::features_txt());
    // pin_pw = 0: no pin/password.
    m.insert("pw".into(), "false".into());
    m.insert("flags".into(), "0x4".into());
    m.insert("model".into(), ADVERTISED_MODEL.into());
    m.insert("pk".into(), cfg.pk_hex.clone());
    m.insert("pi".into(), AIRPLAY_PI.into());
    m.insert("srcvers".into(), ADVERTISED_VERSION.into());
    m.insert("vv".into(), "2".into());
    m
}

/// TXT properties for `_raop._tcp` (cf. `dnssd_register_raop`).
pub fn raop_txt(cfg: &AdvertiserConfig) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ch".into(), "2".into());
    m.insert("cn".into(), "0,1,2,3".into());
    m.insert("da".into(), "true".into());
    m.insert("et".into(), "0,3,5".into());
    m.insert("vv".into(), "2".into());
    m.insert("ft".into(), AdvertiserConfig::features_txt());
    m.insert("am".into(), ADVERTISED_MODEL.into());
    m.insert("md".into(), "0,1,2".into());
    m.insert("rhd".into(), "5.6.0.0".into());
    // pin_pw = 0: no pin/password.
    m.insert("pw".into(), "false".into());
    m.insert("sf".into(), "0x4".into());
    m.insert("sr".into(), "44100".into());
    m.insert("ss".into(), "16".into());
    m.insert("sv".into(), "false".into());
    m.insert("tp".into(), "UDP".into());
    m.insert("txtvers".into(), "1".into());
    m.insert("vs".into(), ADVERTISED_VERSION.into());
    m.insert("vn".into(), "65537".into());
    m.insert("pk".into(), cfg.pk_hex.clone());
    m
}

/// A live advertisement of both AirPlay services.
pub struct Advertiser {
    daemon: ServiceDaemon,
    airplay_fullname: String,
    raop_fullname: String,
}

impl Advertiser {
    /// Register `_airplay._tcp` and `_raop._tcp` for `cfg`.
    pub fn start(cfg: &AdvertiserConfig) -> Result<Self, MdnsError> {
        let daemon = ServiceDaemon::new()?;
        let host = hostname();
        let airplay = ServiceInfo::new(
            "_airplay._tcp.local.",
            &cfg.name,
            &host,
            "",
            cfg.airplay_port,
            airplay_txt(cfg),
        )?;
        let raop = ServiceInfo::new(
            "_raop._tcp.local.",
            &cfg.raop_instance_name(),
            &host,
            "",
            cfg.raop_port,
            raop_txt(cfg),
        )?;
        let airplay_fullname = airplay.get_fullname().to_string();
        let raop_fullname = raop.get_fullname().to_string();
        daemon.register(airplay)?;
        info!(service = %airplay_fullname, port = cfg.airplay_port, "advertising _airplay._tcp");
        daemon.register(raop)?;
        info!(service = %raop_fullname, port = cfg.raop_port, "advertising _raop._tcp");
        Ok(Self {
            daemon,
            airplay_fullname,
            raop_fullname,
        })
    }

    /// Full service names, for diagnostics.
    pub fn service_names(&self) -> (&str, &str) {
        (&self.airplay_fullname, &self.raop_fullname)
    }

    /// Unregister both services and stop the daemon.
    pub fn shutdown(self) -> Result<(), MdnsError> {
        self.daemon.unregister(&self.airplay_fullname)?;
        self.daemon.unregister(&self.raop_fullname)?;
        self.daemon.shutdown()?;
        info!("mDNS advertisement stopped");
        Ok(())
    }
}

/// Best-effort local hostname for the SRV target.
fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(|h| format!("{h}.local."))
        .unwrap_or_else(|_| "uxplay-rs.local.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let cfg = AdvertiserConfig::default();
        assert!(!cfg.name.is_empty());
        assert_eq!(cfg.airplay_port, 7100);
        assert_eq!(cfg.raop_port, 7000);
    }

    #[test]
    fn device_id_formats_match_uxplay_utils() {
        let cfg = AdvertiserConfig {
            hw_addr: [0x48, 0x5D, 0x35, 0x8B, 0x9A, 0x20],
            ..Default::default()
        };
        assert_eq!(cfg.device_id(), "48:5d:35:8b:9a:20");
        assert_eq!(cfg.raop_instance_name(), "485D358B9A20@uxplay-rs");
    }

    #[test]
    fn features_txt_matches_uxplay() {
        assert_eq!(AdvertiserConfig::features_txt(), "0x5A7FFEE6,0x0");
    }

    #[test]
    fn airplay_txt_has_required_keys() {
        let cfg = AdvertiserConfig::default();
        let txt = airplay_txt(&cfg);
        for key in [
            "deviceid", "features", "pw", "flags", "model", "pk", "pi", "srcvers", "vv",
        ] {
            assert!(txt.contains_key(key), "missing {key}");
        }
        assert_eq!(txt["features"], "0x5A7FFEE6,0x0");
        assert_eq!(txt["model"], "AppleTV3,2");
        assert_eq!(txt["srcvers"], "220.68");
        assert_eq!(txt["flags"], "0x4");
        assert_eq!(txt["pi"], AIRPLAY_PI);
    }

    #[test]
    fn raop_txt_has_required_keys() {
        let cfg = AdvertiserConfig::default();
        let txt = raop_txt(&cfg);
        for key in [
            "ch", "cn", "da", "et", "vv", "ft", "am", "md", "rhd", "pw", "sf", "sr", "ss", "sv",
            "tp", "txtvers", "vs", "vn", "pk",
        ] {
            assert!(txt.contains_key(key), "missing {key}");
        }
        assert_eq!(txt["cn"], "0,1,2,3");
        assert_eq!(txt["et"], "0,3,5");
        assert_eq!(txt["sr"], "44100");
        assert_eq!(txt["tp"], "UDP");
        assert_eq!(txt["vn"], "65537");
        // pk must be identical across both services.
        assert_eq!(txt["pk"], airplay_txt(&cfg)["pk"]);
    }

    #[test]
    fn start_shutdown_round_trip() {
        let mut cfg = AdvertiserConfig::default();
        cfg.airplay_port = 17100;
        cfg.raop_port = 17000;
        let adv = Advertiser::start(&cfg).expect("local mDNS registration works offline");
        let (airplay, raop) = adv.service_names();
        assert!(airplay.contains("_airplay._tcp"));
        assert!(raop.contains("_raop._tcp"));
        adv.shutdown().expect("shutdown succeeds");
    }
}
