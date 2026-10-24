// SPDX-License-Identifier: GPL-3.0-or-later
//! Pointer settings: DPI stages, report rate, sensor options, debounce
//! (PROTOCOL.md §4 "0x07" and §6).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// DPI table: five stages plus the active one (*verified*, command 0x40).
///
/// Settings files deserialize through [`Dpi::validate`], so a file cannot
/// carry a stage outside 100-26000, off the 50-DPI grid, or a sixth stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "DpiFile")]
pub struct Dpi {
    /// Five stages in DPI, 100..=26000, multiples of 50.
    pub stages: [u16; Dpi::STAGE_COUNT],
    /// Index 0-4 of the active stage.
    pub active: u8,
}

impl Dpi {
    pub const STAGE_COUNT: usize = 5;
    /// Slider ends in the vendor app (*verified*: 0x0064 and 0x6590).
    pub const MIN: u16 = 100;
    pub const MAX: u16 = 26000;
    /// Typed values are floored to a multiple of this (*verified*: 1234 → 1200).
    pub const STEP: u16 = 50;

    /// The vendor app's "Reset DPI" values (*verified*).
    pub const FACTORY: Dpi = Dpi {
        stages: [400, 800, 1600, 3200, 5000],
        active: 2,
    };

    /// Stage indicator colors the app shows (fixed, not sent to the mouse).
    pub const INDICATOR_COLORS: [&'static str; 5] = ["white", "green", "blue", "yellow", "red"];

    /// Applies the app's rules: clamp into range and floor to a multiple of 50.
    pub fn normalize_value(dpi: u16) -> u16 {
        let d = dpi.clamp(Self::MIN, Self::MAX);
        d - d % Self::STEP
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.active as usize >= Self::STAGE_COUNT {
            return Err(format!("active stage {} is not 0-4", self.active));
        }
        for (i, s) in self.stages.iter().enumerate() {
            if !(Self::MIN..=Self::MAX).contains(s) {
                return Err(format!(
                    "stage {} = {s} is outside {}-{}",
                    i + 1,
                    Self::MIN,
                    Self::MAX
                ));
            }
            if s % Self::STEP != 0 {
                return Err(format!(
                    "stage {} = {s} is not a multiple of {}",
                    i + 1,
                    Self::STEP
                ));
            }
        }
        Ok(())
    }

    pub fn active_dpi(&self) -> u16 {
        self.stages[(self.active as usize).min(Self::STAGE_COUNT - 1)]
    }
}

impl Default for Dpi {
    fn default() -> Self {
        Dpi::FACTORY
    }
}

/// The on-disk shape of [`Dpi`], validated before it becomes one.
#[derive(Deserialize)]
struct DpiFile {
    stages: [u16; Dpi::STAGE_COUNT],
    active: u8,
}

impl TryFrom<DpiFile> for Dpi {
    type Error = String;
    fn try_from(f: DpiFile) -> Result<Dpi, String> {
        let d = Dpi {
            stages: f.stages,
            active: f.active,
        };
        d.validate()?;
        Ok(d)
    }
}

/// USB report rate (*verified*, command 0x41). The index is what the mouse
/// stores (0x07 slot bits 4-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReportRate {
    Hz125 = 0,
    Hz500 = 1,
    Hz1000 = 2,
}

impl ReportRate {
    pub const ALL: [ReportRate; 3] = [ReportRate::Hz125, ReportRate::Hz500, ReportRate::Hz1000];
    /// The rate table the app sends after the index: `7d 00 f4 01 e8 03`.
    pub const TABLE: [u16; 3] = [125, 500, 1000];

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(i: u8) -> Option<ReportRate> {
        ReportRate::ALL.get(i as usize).copied()
    }

    pub fn hz(self) -> u16 {
        Self::TABLE[self as usize]
    }

    pub fn from_hz(hz: u16) -> Option<ReportRate> {
        ReportRate::ALL.iter().copied().find(|r| r.hz() == hz)
    }
}

impl fmt::Display for ReportRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.hz())
    }
}

impl FromStr for ReportRate {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let n: u16 = s
            .trim()
            .trim_end_matches("Hz")
            .trim_end_matches("hz")
            .trim()
            .parse()
            .map_err(|_| format!("'{s}' is not a report rate"))?;
        ReportRate::from_hz(n)
            .ok_or_else(|| format!("report rate must be 125, 500 or 1000 (got {n})"))
    }
}

impl Serialize for ReportRate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(self.hz())
    }
}

impl<'de> Deserialize<'de> for ReportRate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let hz = u16::deserialize(d)?;
        ReportRate::from_hz(hz).ok_or_else(|| {
            serde::de::Error::custom(format!("report rate {hz} is not 125/500/1000"))
        })
    }
}

/// Lift-off distance (*verified*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiftOff {
    Mm1 = 1,
    Mm2 = 2,
}

impl LiftOff {
    pub fn from_byte(b: u8) -> Option<LiftOff> {
        match b {
            1 => Some(LiftOff::Mm1),
            2 => Some(LiftOff::Mm2),
            _ => None,
        }
    }

    pub fn mm(self) -> u8 {
        self as u8
    }
}

impl Serialize for LiftOff {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.mm())
    }
}

impl<'de> Deserialize<'de> for LiftOff {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mm = u8::deserialize(d)?;
        LiftOff::from_byte(mm)
            .ok_or_else(|| serde::de::Error::custom("lift-off must be 1 or 2 (mm)"))
    }
}

/// Command 0x42 (*verified*): `51 42 LO RP AS MS 00 SD`, each field 1 = first
/// option, 2 = second. All fields are always sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SensorSettings {
    #[serde(rename = "lift_off_mm")]
    pub lift_off: LiftOff,
    pub ripple: bool,
    pub angle_snap: bool,
    pub motion_sync: bool,
    pub scroll_reverse: bool,
}

impl SensorSettings {
    /// Factory state (*verified*: flags byte 0x35 and the app's Settings page).
    pub const FACTORY: SensorSettings = SensorSettings {
        lift_off: LiftOff::Mm1,
        ripple: true,
        angle_snap: false,
        motion_sync: true,
        scroll_reverse: false,
    };

    /// The six payload bytes of the 0x42 write.
    pub fn payload(&self) -> [u8; 6] {
        let onoff = |on: bool| if on { 1 } else { 2 };
        [
            self.lift_off.mm(),
            onoff(self.ripple),
            onoff(self.angle_snap),
            onoff(self.motion_sync),
            0x00,
            if self.scroll_reverse { 2 } else { 1 },
        ]
    }

    pub fn from_payload(p: &[u8]) -> Option<SensorSettings> {
        if p.len() < 6 {
            return None;
        }
        let flag = |b: u8| match b {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        Some(SensorSettings {
            lift_off: LiftOff::from_byte(p[0])?,
            ripple: flag(p[1])?,
            angle_snap: flag(p[2])?,
            motion_sync: flag(p[3])?,
            scroll_reverse: !flag(p[5])?,
        })
    }

    /// Decodes the `FF` byte of the 0x07 reply (*verified* bit layout; bit 5
    /// is the factory-state flag and is ignored).
    pub fn from_flags(ff: u8) -> SensorSettings {
        SensorSettings {
            lift_off: if ff & 0x03 == 2 {
                LiftOff::Mm2
            } else {
                LiftOff::Mm1
            },
            ripple: ff & 0x04 != 0,
            angle_snap: ff & 0x08 != 0,
            motion_sync: ff & 0x10 != 0,
            scroll_reverse: ff & 0x40 != 0,
        }
    }
}

impl Default for SensorSettings {
    fn default() -> Self {
        SensorSettings::FACTORY
    }
}

/// Debounce time in ms (*verified*, command 0x43). The app showed 8 by
/// default and stepped by 1; only 8-11 were captured.
///
/// Two ranges:
/// - [`Debounce::OBSERVED_MIN`]..=[`Debounce::OBSERVED_MAX`] (8-11): seen on
///   the wire from the vendor app; written without an opt-in.
/// - [`Debounce::MIN`]..=[`Debounce::MAX`] (1-30): the hard limit this
///   software will ever send. Values outside 8-11 were never captured, so
///   [`crate::Mouse::apply_debounce`] refuses them unless the user opted in
///   to untested writes (`--allow-untested` / the GUI checkbox).
///
/// Settings files deserialize through [`Debounce::new`] (hard limit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Debounce(pub u8);

impl Debounce {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 30;
    /// Smallest value captured from the vendor app (its default).
    pub const OBSERVED_MIN: u8 = 8;
    /// Largest value captured (three "+" clicks from 8).
    pub const OBSERVED_MAX: u8 = 11;
    pub const FACTORY: Debounce = Debounce(8);

    /// Accepts the hard limit 1-30 ms. Whether a value may be written
    /// without an opt-in is [`Debounce::is_observed`].
    pub fn new(ms: u8) -> Result<Debounce, String> {
        if (Self::MIN..=Self::MAX).contains(&ms) {
            Ok(Debounce(ms))
        } else {
            Err(format!(
                "debounce {ms} ms is outside {}-{} ms, the most Tailglow will ever send (only {}-{} were captured from the vendor app)",
                Self::MIN,
                Self::MAX,
                Self::OBSERVED_MIN,
                Self::OBSERVED_MAX
            ))
        }
    }

    pub fn ms(self) -> u8 {
        self.0
    }

    /// True for 8-11 ms, the values seen on the wire.
    pub fn is_observed(self) -> bool {
        (Self::OBSERVED_MIN..=Self::OBSERVED_MAX).contains(&self.0)
    }

    /// Why a value outside 8-11 needs the opt-in (shown by CLI and GUI).
    pub fn untested_warning(self) -> String {
        format!(
            "debounce {} ms was never captured: the vendor app was only seen sending {}-{} ms, and the mouse's real limits are unknown (a value it does not expect could make clicks unreliable)",
            self.0,
            Self::OBSERVED_MIN,
            Self::OBSERVED_MAX
        )
    }
}

impl<'de> Deserialize<'de> for Debounce {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Debounce::new(u8::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Default for Debounce {
    fn default() -> Self {
        Debounce::FACTORY
    }
}

/// What the mouse stores in `0x0b` resets (*verified*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetKind {
    /// `51 0b 01 01`: all button functions.
    Buttons,
    /// `51 0b 01 04`: lighting data (back to Rainbow, red, full, 0x80).
    Lighting,
    /// `51 0b aa`: everything.
    Factory,
}

impl ResetKind {
    pub fn payload(self) -> &'static [u8] {
        match self {
            ResetKind::Buttons => &[0x01, 0x01],
            ResetKind::Lighting => &[0x01, 0x04],
            ResetKind::Factory => &[0xaa],
        }
    }

    pub fn from_payload(p: &[u8]) -> Option<ResetKind> {
        match p {
            [0x01, 0x01] => Some(ResetKind::Buttons),
            [0x01, 0x04] => Some(ResetKind::Lighting),
            [0xaa] => Some(ResetKind::Factory),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ResetKind::Buttons => "buttons",
            ResetKind::Lighting => "lighting",
            ResetKind::Factory => "factory",
        }
    }
}

impl FromStr for ResetKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "buttons" | "button" => Ok(ResetKind::Buttons),
            "lighting" | "light" | "lights" | "rgb" => Ok(ResetKind::Lighting),
            "factory" | "all" => Ok(ResetKind::Factory),
            _ => Err(format!("'{s}' is not buttons, lighting or factory")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_rules_from_capture() {
        assert_eq!(Dpi::normalize_value(1234), 1200);
        assert_eq!(Dpi::normalize_value(450), 450);
        assert_eq!(Dpi::normalize_value(0), 100);
        assert_eq!(Dpi::normalize_value(30000), 26000);
        assert!(Dpi::FACTORY.validate().is_ok());
        assert!(Dpi {
            stages: [50, 800, 1600, 3200, 5000],
            active: 0
        }
        .validate()
        .is_err());
        assert!(Dpi {
            active: 5,
            ..Dpi::FACTORY
        }
        .validate()
        .is_err());
        assert!(Dpi {
            stages: [410, 800, 1600, 3200, 5000],
            active: 0
        }
        .validate()
        .is_err());
        assert_eq!(Dpi::FACTORY.active_dpi(), 1600);
    }

    #[test]
    fn files_go_through_the_constructors() {
        #[derive(Deserialize)]
        struct F {
            dpi: Dpi,
            debounce: Debounce,
        }
        let ok: F = toml::from_str(
            "debounce = 9\n[dpi]\nstages = [400, 800, 1600, 3200, 5000]\nactive = 2\n",
        )
        .unwrap();
        assert_eq!(ok.dpi, Dpi::FACTORY);
        assert_eq!(ok.debounce.ms(), 9);
        for bad in [
            "debounce = 0\n[dpi]\nstages = [400, 800, 1600, 3200, 5000]\nactive = 2\n",
            "debounce = 31\n[dpi]\nstages = [400, 800, 1600, 3200, 5000]\nactive = 2\n",
            "debounce = 8\n[dpi]\nstages = [40, 800, 1600, 3200, 5000]\nactive = 2\n",
            "debounce = 8\n[dpi]\nstages = [400, 800, 1600, 3200, 30000]\nactive = 2\n",
            "debounce = 8\n[dpi]\nstages = [410, 800, 1600, 3200, 5000]\nactive = 2\n",
            "debounce = 8\n[dpi]\nstages = [400, 800, 1600, 3200, 5000]\nactive = 5\n",
        ] {
            assert!(toml::from_str::<F>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn report_rate() {
        assert_eq!("1000".parse::<ReportRate>().unwrap(), ReportRate::Hz1000);
        assert_eq!("500Hz".parse::<ReportRate>().unwrap(), ReportRate::Hz500);
        assert!("250".parse::<ReportRate>().is_err());
        assert_eq!(ReportRate::from_index(2), Some(ReportRate::Hz1000));
        assert_eq!(ReportRate::from_index(3), None);
    }

    #[test]
    fn sensor_flags_match_capture() {
        // Factory 0x35: lift-off 1 mm, ripple on, angle snap off, motion sync on, scroll forward.
        assert_eq!(SensorSettings::from_flags(0x35), SensorSettings::FACTORY);
        // 0x4a: lift-off 2 mm, ripple off, angle snap on, motion sync off, scroll reverse.
        assert_eq!(
            SensorSettings::from_flags(0x4a),
            SensorSettings {
                lift_off: LiftOff::Mm2,
                ripple: false,
                angle_snap: true,
                motion_sync: false,
                scroll_reverse: true,
            }
        );
        // 0x59 (0101 1001): lift-off 1, ripple off, angle snap on, motion sync on, reverse.
        let s = SensorSettings::from_flags(0x59);
        assert_eq!(s.lift_off, LiftOff::Mm1);
        assert!(!s.ripple && s.angle_snap && s.motion_sync && s.scroll_reverse);
        // 0x1d: lift-off 1, ripple on, angle snap on, motion sync on, forward.
        let s = SensorSettings::from_flags(0x1d);
        assert!(s.ripple && s.angle_snap && s.motion_sync && !s.scroll_reverse);
        // Writes: factory → 51 42 01 01 02 01 00 01
        assert_eq!(SensorSettings::FACTORY.payload(), [1, 1, 2, 1, 0, 1]);
        let all_second = SensorSettings::from_payload(&[2, 2, 2, 2, 0, 2]).unwrap();
        assert_eq!(all_second.payload(), [2, 2, 2, 2, 0, 2]);
        assert!(all_second.scroll_reverse && !all_second.ripple);
        assert!(SensorSettings::from_payload(&[3, 1, 1, 1, 0, 1]).is_none());
    }

    #[test]
    fn debounce_and_reset() {
        assert_eq!(Debounce::new(8).unwrap().ms(), 8);
        assert!(Debounce::new(0).is_err());
        assert!(Debounce::new(31).is_err());
        for ms in 8..=11 {
            assert!(Debounce::new(ms).unwrap().is_observed(), "{ms}");
        }
        assert!(!Debounce::new(7).unwrap().is_observed());
        assert!(!Debounce::new(12).unwrap().is_observed());
        assert!(!Debounce::new(1).unwrap().is_observed());
        assert!(Debounce(1).untested_warning().contains("8-11"));
        assert_eq!(ResetKind::Factory.payload(), &[0xaa]);
        assert_eq!(ResetKind::from_payload(&[1, 4]), Some(ResetKind::Lighting));
        assert_eq!("buttons".parse::<ResetKind>().unwrap(), ResetKind::Buttons);
    }
}
