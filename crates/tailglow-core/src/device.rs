// SPDX-License-Identifier: GPL-3.0-or-later
//! The device table: which mice we know, and how far we trust each.
//!
//! Only the Keychron M1 (wired, 3434:d035) is *verified*: every command was
//! captured from the vendor app driving this exact mouse and confirmed on it.
//! The other entries come from the vendor app's own device list
//! (read from its config.xml, not from traffic; `docs/PROTOCOL.md` §11): the models the same app drives through the same vendor
//! interface (MI_02) *likely* speak the same protocol, so reads are allowed by
//! default and writes need an explicit opt-in. The 4K models (MI_03) and the
//! 8K/Launcher family (0xB3/0xB5 reports) are listed only so the software can
//! name them and refuse.

use crate::lighting::Effect;
use std::fmt;
use std::ops::RangeInclusive;

/// USB vendor and product ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub vid: u16,
    pub pid: u16,
}

impl DeviceId {
    pub const fn new(vid: u16, pid: u16) -> Self {
        DeviceId { vid, pid }
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vid, self.pid)
    }
}

/// How much we trust the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportStatus {
    /// Protocol captured from the vendor app and confirmed on the hardware.
    Verified,
    /// Same vendor app and interface as a verified model; never tested.
    /// Reads are allowed; writes need `allow_untested`.
    Likely,
    /// Known to use a different protocol or interface; nothing is sent.
    Unsupported,
}

impl SupportStatus {
    pub fn label(self) -> &'static str {
        match self {
            SupportStatus::Verified => "verified",
            SupportStatus::Likely => "likely (untested)",
            SupportStatus::Unsupported => "unsupported",
        }
    }
}

/// What the lighting engine of a model accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightingCaps {
    pub effects: &'static [Effect],
    /// Raw brightness bytes the mouse accepts (the app never sends below 0x0c).
    pub brightness: RangeInclusive<u8>,
    /// Raw speed bytes (the app's slider: 0x14..=0xff).
    pub speed: RangeInclusive<u8>,
}

impl LightingCaps {
    pub fn supports(&self, effect: Effect) -> bool {
        self.effects.contains(&effect)
    }
}

/// Feature set of a model. `None`/`false` means "not supported by this
/// software"; it says nothing about the hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub lighting: Option<LightingCaps>,
    /// DPI stages, report rate, sensor options, debounce (0x40-0x43).
    pub pointer: bool,
    /// Button remapping and single-chunk macros (0x52-0x54).
    pub buttons: bool,
    /// On-board profile *display* (0x07 byte 2). Switching is never written.
    pub profiles_read: bool,
    /// Battery byte in 0x06.
    pub battery: bool,
}

const M1_CAPS: Capabilities = Capabilities {
    lighting: Some(LightingCaps {
        effects: &Effect::ALL,
        brightness: 0x0c..=0xff,
        speed: 0x14..=0xff,
    }),
    pointer: true,
    buttons: true,
    profiles_read: true,
    battery: true,
};

const NONE_CAPS: Capabilities = Capabilities {
    lighting: None,
    pointer: false,
    buttons: false,
    profiles_read: false,
    battery: false,
};

/// Which protocol family an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Keychron Mouse 1.2.1.2 "Engine": feature reports 0x51/0x52.
    Engine,
    /// 4K-polling models on MI_03: unknown protocol.
    Engine4k,
    /// Keychron Launcher / 8K: output reports 0xB3/0xB5 (different protocol).
    Launcher8k,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: DeviceId,
    /// Marketing name.
    pub name: &'static str,
    /// USB interface carrying the vendor feature reports (Linux: match the
    /// hidraw node whose parent interface has this `bInterfaceNumber`).
    pub vendor_interface: u8,
    pub status: SupportStatus,
    pub family: Family,
    pub caps: Capabilities,
    /// Where the entry comes from.
    pub evidence: &'static str,
}

impl Model {
    /// True when reads may be attempted (verified or likely).
    pub fn readable(&self) -> bool {
        self.status != SupportStatus::Unsupported
    }

    /// True when writes are allowed given the user's opt-in.
    pub fn writable(&self, allow_untested: bool) -> bool {
        match self.status {
            SupportStatus::Verified => true,
            SupportStatus::Likely => allow_untested,
            SupportStatus::Unsupported => false,
        }
    }
}

/// Keychron USB vendor ID (*verified* from the device descriptor).
pub const KEYCHRON_VID: u16 = 0x3434;

const fn engine(
    pid: u16,
    name: &'static str,
    status: SupportStatus,
    vendor_interface: u8,
    evidence: &'static str,
) -> Model {
    Model {
        id: DeviceId::new(KEYCHRON_VID, pid),
        name,
        vendor_interface,
        status,
        family: Family::Engine,
        caps: M1_CAPS,
        evidence,
    }
}

const fn unsupported(
    pid: u16,
    name: &'static str,
    family: Family,
    iface: u8,
    evidence: &'static str,
) -> Model {
    Model {
        id: DeviceId::new(KEYCHRON_VID, pid),
        name,
        vendor_interface: iface,
        status: SupportStatus::Unsupported,
        family,
        caps: NONE_CAPS,
        evidence,
    }
}

const APP_CFG: &str = "Keychron Mouse 1.2.1.2 config.xml (MI_02); same app as the M1, no capture";
const APP_CFG_RX: &str = "Keychron Mouse 1.2.1.2 config.xml: 2.4 GHz receiver on MI_01; no capture";
const APP_CFG_4K: &str = "Keychron Mouse 1.2.1.2 config.xml: 4K model on MI_03; protocol unknown";
const LAUNCHER: &str =
    "Keychron Launcher family: 0xB3/0xB5 output reports (chron_tui, keychron-tray-rs); not this protocol";

/// Keychron M1, wired over its USB cable (*verified*, sessions 1 and 2).
pub const KEYCHRON_M1_WIRED: Model = engine(
    0xd035,
    "Keychron M1",
    SupportStatus::Verified,
    2,
    "testdata/session1.pcap, testdata/session2.pcap; confirmed on the mouse",
);

/// Every model we know about, verified first.
pub static MODELS: &[Model] = &[
    KEYCHRON_M1_WIRED,
    // Likely: same app, same vendor interface.
    engine(0xd032, "Keychron M3", SupportStatus::Likely, 2, APP_CFG),
    engine(0xd033, "Keychron M3", SupportStatus::Likely, 2, APP_CFG),
    engine(
        0xd036,
        "Keychron M3 mini",
        SupportStatus::Likely,
        2,
        APP_CFG,
    ),
    engine(
        0xd03e,
        "Keychron M3 mini",
        SupportStatus::Likely,
        2,
        APP_CFG,
    ),
    engine(0xd03b, "Keychron M2", SupportStatus::Likely, 2, APP_CFG),
    engine(
        0xd03d,
        "Keychron M2 mini",
        SupportStatus::Likely,
        2,
        APP_CFG,
    ),
    engine(0xd03f, "Keychron M6", SupportStatus::Likely, 2, APP_CFG),
    engine(0xd044, "Keychron M7", SupportStatus::Likely, 2, APP_CFG),
    engine(
        0xd031,
        "Keychron 2.4 GHz receiver (Type-C)",
        SupportStatus::Likely,
        1,
        APP_CFG_RX,
    ),
    engine(
        0xd030,
        "Keychron 2.4 GHz receiver (Type-A)",
        SupportStatus::Likely,
        1,
        APP_CFG_RX,
    ),
    // Unknown protocol: 4K models on MI_03.
    unsupported(
        0xd037,
        "Keychron M3 mini 4K",
        Family::Engine4k,
        3,
        APP_CFG_4K,
    ),
    unsupported(0xd03c, "Keychron M3 4K", Family::Engine4k, 3, APP_CFG_4K),
    unsupported(
        0xd041,
        "Keychron M3 mini 4K",
        Family::Engine4k,
        3,
        APP_CFG_4K,
    ),
    unsupported(0xd045, "Keychron M2 4K", Family::Engine4k, 3, APP_CFG_4K),
    unsupported(0xd046, "Keychron M6 4K", Family::Engine4k, 3, APP_CFG_4K),
    unsupported(
        0xd040,
        "Keychron M4 / M4 4K",
        Family::Engine4k,
        3,
        "linux-hardware.org id; app config entry truncated",
    ),
    unsupported(
        0xd038,
        "Keychron 4K receiver",
        Family::Engine4k,
        3,
        APP_CFG_4K,
    ),
    unsupported(
        0xd043,
        "Keychron 4K receiver",
        Family::Engine4k,
        3,
        APP_CFG_4K,
    ),
    // Different protocol family.
    unsupported(0xd050, "Keychron M3 8K", Family::Launcher8k, 4, LAUNCHER),
    unsupported(0xd048, "Keychron M5 8K", Family::Launcher8k, 4, LAUNCHER),
    unsupported(0xd049, "Keychron M6 8K", Family::Launcher8k, 4, LAUNCHER),
    unsupported(
        0xd028,
        "Keychron Ultra-Link 8K receiver",
        Family::Launcher8k,
        4,
        LAUNCHER,
    ),
];

/// Looks a VID:PID up in [`MODELS`].
pub fn lookup(id: DeviceId) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m1_is_verified_and_fully_capable() {
        let m = lookup(DeviceId::new(0x3434, 0xd035)).expect("M1");
        assert_eq!(m.name, "Keychron M1");
        assert_eq!(m.vendor_interface, 2);
        assert_eq!(m.status, SupportStatus::Verified);
        let l = m.caps.lighting.as_ref().unwrap();
        assert_eq!(l.effects.len(), 7);
        assert_eq!(*l.brightness.start(), 0x0c);
        assert_eq!(*l.speed.start(), 0x14);
        assert!(l.supports(Effect::Christmas));
        assert!(m.caps.pointer && m.caps.buttons && m.caps.profiles_read);
        assert!(m.writable(false));
    }

    #[test]
    fn likely_models_need_opt_in_and_unsupported_never_write() {
        let m3 = lookup(DeviceId::new(0x3434, 0xd032)).unwrap();
        assert_eq!(m3.status, SupportStatus::Likely);
        assert!(m3.readable());
        assert!(!m3.writable(false));
        assert!(m3.writable(true));
        let rx = lookup(DeviceId::new(0x3434, 0xd031)).unwrap();
        assert_eq!(rx.vendor_interface, 1);
        let m3_4k = lookup(DeviceId::new(0x3434, 0xd03c)).unwrap();
        assert_eq!(m3_4k.status, SupportStatus::Unsupported);
        assert!(!m3_4k.readable());
        assert!(!m3_4k.writable(true));
        assert!(m3_4k.caps.lighting.is_none());
        let m3_8k = lookup(DeviceId::new(0x3434, 0xd050)).unwrap();
        assert_eq!(m3_8k.family, Family::Launcher8k);
        assert!(!m3_8k.writable(true));
    }

    #[test]
    fn unknown_ids_are_rejected_and_ids_are_unique() {
        assert!(lookup(DeviceId::new(0x3434, 0x0001)).is_none());
        assert!(lookup(DeviceId::new(0x046d, 0xc08b)).is_none());
        assert_eq!(DeviceId::new(0x3434, 0xd035).to_string(), "3434:d035");
        let mut ids: Vec<_> = MODELS.iter().map(|m| m.id).collect();
        let n = ids.len();
        ids.sort_by_key(|i| (i.vid, i.pid));
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate PID in MODELS");
        assert_eq!(
            MODELS
                .iter()
                .filter(|m| m.status == SupportStatus::Verified)
                .count(),
            1
        );
    }
}
