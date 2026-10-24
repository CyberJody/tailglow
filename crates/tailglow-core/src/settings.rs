// SPDX-License-Identifier: GPL-3.0-or-later
//! Versioned settings file for export/import (TOML), shared by the CLI
//! (`export`/`import`), the GUI (Open/Save) and the apply-on-plug service.
//!
//! ```toml
//! schema = 1
//!
//! [device]
//! model = "Keychron M1"
//! firmware = "1.0.0r-5"
//!
//! [lighting]
//! effect = "static"
//! brightness = 255
//! speed = 128
//! color = "#00ff00"
//!
//! [dpi]
//! stages = [400, 800, 1600, 3200, 5000]
//! active = 2
//!
//! [pointer]
//! report_rate = 1000
//! lift_off_mm = 1
//! ripple = true
//! angle_snap = false
//! motion_sync = true
//! scroll_reverse = false
//! debounce_ms = 8
//!
//! [buttons]
//! key4 = "consumer:volume-up"
//! key5 = "key:ctrl+shift+k"
//!
//! [macros.key6]
//! name = "Custom 1"
//! loop = "times:3"
//! fixed_delay_ms = 25
//! events = ["a+", "47ms", "a-"]
//! ```
//!
//! Sections are optional so a file can carry just the parts the user wants.
//! Sections were added without bumping `schema` (old files still parse);
//! `schema` only moves when a field's meaning changes.
//!
//! Everything except macros is read back from the mouse (0x07, 0x12-0x17,
//! 0x61/0x62). A key holding a macro reads back as factory (real M1: the
//! 0x61 flag stays 0) or as an unknown type, so a `[macros]` section only
//! comes from what tailglow itself assigned.
//!
//! The state file (`state.toml`, written by the CLI, GUI and tray) uses the
//! same format plus one extra table, `[sent_macros]`: a fingerprint of the
//! exact reports last sent for each macro key, so apply-on-plug and profile
//! switches skip a macro the mouse already holds ([`macro_fingerprint`]).

use std::collections::BTreeMap;

use crate::buttons::{ButtonAction, Key, Macro};
use crate::lighting::Lighting;
use crate::mouse::{ButtonMap, Snapshot};
use crate::pointer::{Debounce, Dpi, LiftOff, ReportRate, SensorSettings};
use crate::protocol::Command;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings file uses schema {found}; this build understands up to {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("settings file is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("could not serialize settings: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SettingsDevice {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
}

/// `[pointer]`: report rate, sensor options and debounce in one table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerConfig {
    pub report_rate: ReportRate,
    pub lift_off_mm: LiftOff,
    pub ripple: bool,
    pub angle_snap: bool,
    pub motion_sync: bool,
    pub scroll_reverse: bool,
    pub debounce_ms: Debounce,
}

impl PointerConfig {
    pub fn sensor(&self) -> SensorSettings {
        SensorSettings {
            lift_off: self.lift_off_mm,
            ripple: self.ripple,
            angle_snap: self.angle_snap,
            motion_sync: self.motion_sync,
            scroll_reverse: self.scroll_reverse,
        }
    }

    pub fn new(report_rate: ReportRate, sensor: SensorSettings, debounce: Debounce) -> Self {
        PointerConfig {
            report_rate,
            lift_off_mm: sensor.lift_off,
            ripple: sensor.ripple,
            angle_snap: sensor.angle_snap,
            motion_sync: sensor.motion_sync,
            scroll_reverse: sensor.scroll_reverse,
            debounce_ms: debounce,
        }
    }
}

impl Default for PointerConfig {
    fn default() -> Self {
        PointerConfig::new(
            ReportRate::Hz1000,
            SensorSettings::FACTORY,
            Debounce::FACTORY,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<SettingsDevice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<Lighting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<Dpi>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<PointerConfig>,
    /// Only keys that differ from the factory map need to be listed; a key
    /// set to `"default"` is reset.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub buttons: BTreeMap<Key, ButtonAction>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub macros: BTreeMap<Key, Macro>,
    /// State file only: key → [`macro_fingerprint`] of the macro tailglow
    /// last sent to that key. Never needed in hand-written files.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sent_macros: BTreeMap<Key, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            schema: Settings::SCHEMA,
            device: None,
            lighting: None,
            dpi: None,
            pointer: None,
            buttons: BTreeMap::new(),
            macros: BTreeMap::new(),
            sent_macros: BTreeMap::new(),
        }
    }
}

/// Stable fingerprint of a macro assignment: FNV-1a 64 over the exact two
/// reports tailglow sends (`52 54 …` events, `52 53 …` name). Stable across
/// builds and platforms (unlike `std`'s hasher), so it can live in a file.
pub fn macro_fingerprint(key: Key, m: &Macro) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for cmd in [
        Command::SetMacroEvents {
            key,
            macro_: m.clone(),
        },
        Command::SetMacroName {
            key,
            name: m.name.clone(),
        },
    ] {
        for b in cmd.encode() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("fnv1a64:{h:016x}")
}

/// Whether a macro tailglow assigned to `key` can still be on the mouse,
/// judged from the read-back: a factory read (the real M1 does not flag macro
/// keys) or an undecodable type (what a macro key may read as) keeps it; any
/// decodable mapping means something replaced it.
pub fn macro_may_remain(map: &ButtonMap, key: Key) -> bool {
    matches!(
        map.actions.get(&key),
        None | Some(ButtonAction::Default) | Some(ButtonAction::Raw { .. })
    )
}

impl Settings {
    pub const SCHEMA: u32 = 1;

    pub fn with_lighting(lighting: Lighting) -> Self {
        Settings {
            lighting: Some(lighting),
            ..Default::default()
        }
    }

    /// Everything the mouse reported, as a settings file. Keys the mouse
    /// flags as remapped are listed; others are omitted (factory).
    pub fn from_snapshot(snap: &Snapshot) -> Self {
        let p = &snap.info.pointer;
        Settings {
            schema: Settings::SCHEMA,
            device: Some(SettingsDevice {
                model: snap.info.model_name().to_string(),
                firmware: Some(snap.info.firmware.version.clone()),
            }),
            lighting: Some(snap.lighting.current()),
            dpi: Some(p.dpi),
            pointer: Some(PointerConfig::new(
                p.report_rate().unwrap_or(ReportRate::Hz1000),
                p.sensor,
                p.debounce,
            )),
            buttons: snap.buttons.actions.clone(),
            macros: BTreeMap::new(),
            sent_macros: BTreeMap::new(),
        }
    }

    /// The mouse's settings plus the macros `remembered` (a previous state
    /// file) says tailglow assigned, for keys where they can still be on the
    /// mouse ([`macro_may_remain`]); their fingerprints come along. Used for
    /// the state file; exports clear `sent_macros` afterwards.
    pub fn from_snapshot_with_macros(snap: &Snapshot, remembered: &Settings) -> Self {
        let mut s = Settings::from_snapshot(snap);
        for (k, m) in &remembered.macros {
            if macro_may_remain(&snap.buttons, *k) {
                s.buttons.remove(k);
                s.macros.insert(*k, m.clone());
                if let Some(fp) = remembered.sent_macros.get(k) {
                    s.sent_macros.insert(*k, fp.clone());
                }
            }
        }
        s
    }

    /// Forgets the macro on `key` (after the key was remapped or reset).
    pub fn forget_macro(&mut self, key: Key) {
        self.macros.remove(&key);
        self.sent_macros.remove(&key);
    }

    /// Forgets every macro (after a button or factory reset).
    pub fn forget_macros(&mut self) {
        self.macros.clear();
        self.sent_macros.clear();
    }

    /// Records a successful apply of `applied` in this state: remapped keys
    /// lose their macro, and macros sent (or skipped as already present) are
    /// remembered with their fingerprints.
    pub fn record_applied(&mut self, applied: &Settings) {
        for k in applied.buttons.keys() {
            self.forget_macro(*k);
        }
        for (k, m) in &applied.macros {
            self.buttons.remove(k);
            self.macros.insert(*k, m.clone());
            self.sent_macros.insert(*k, macro_fingerprint(*k, m));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lighting.is_none()
            && self.dpi.is_none()
            && self.pointer.is_none()
            && self.buttons.is_empty()
            && self.macros.is_empty()
    }

    pub fn to_toml(&self) -> Result<String, SettingsError> {
        Ok(toml::to_string(self)?)
    }

    pub fn from_toml(text: &str) -> Result<Self, SettingsError> {
        let s: Settings = toml::from_str(text)?;
        if s.schema > Settings::SCHEMA {
            return Err(SettingsError::SchemaTooNew {
                found: s.schema,
                supported: Settings::SCHEMA,
            });
        }
        Ok(s)
    }

    /// The parts of `self` that differ from `current` (what the mouse has).
    /// Used by apply-on-plug and per-app profiles to write only differences.
    /// Lighting compares only what the mouse shows ([`Lighting::visible`]).
    /// Macros cannot be read back and are always carried over; the caller
    /// skips the ones whose fingerprint matches the state file.
    pub fn diff_against(&self, current: &Settings) -> Settings {
        let mut out = Settings::default();
        if let Some(l) = self.lighting {
            if !current.lighting.is_some_and(|c| c.same_visible(&l)) {
                out.lighting = Some(l);
            }
        }
        if let Some(d) = self.dpi {
            if current.dpi != Some(d) {
                out.dpi = Some(d);
            }
        }
        if let Some(p) = self.pointer {
            if current.pointer != Some(p) {
                out.pointer = Some(p);
            }
        }
        for (k, a) in &self.buttons {
            let cur = current
                .buttons
                .get(k)
                .cloned()
                .unwrap_or(ButtonAction::Default);
            let want = if *a == ButtonAction::Default {
                ButtonAction::Default
            } else {
                a.clone()
            };
            let cur_eff = if cur == ButtonAction::Default {
                k.factory_action()
            } else {
                cur
            };
            let want_eff = if want == ButtonAction::Default {
                k.factory_action()
            } else {
                want.clone()
            };
            if cur_eff != want_eff {
                out.buttons.insert(*k, want);
            }
        }
        out.macros = self.macros.clone();
        out
    }

    /// Read-back comparison after an apply: every readable part of `self`
    /// that `actual` (from a fresh snapshot) does not match, described for
    /// the user. Macros are skipped: the mouse cannot report them.
    pub fn mismatches(&self, actual: &Settings) -> Vec<String> {
        let d = self.diff_against(actual);
        let mut out = Vec::new();
        if let Some(l) = d.lighting {
            out.push(format!(
                "lighting: wanted {}, mouse reports {}",
                describe_lighting(&l),
                actual
                    .lighting
                    .map(|a| describe_lighting(&a))
                    .unwrap_or_else(|| "nothing".into())
            ));
        }
        if let Some(want) = d.dpi {
            out.push(format!(
                "dpi: wanted {:?} active {}, mouse reports {}",
                want.stages,
                want.active + 1,
                actual
                    .dpi
                    .map(|a| format!("{:?} active {}", a.stages, a.active + 1))
                    .unwrap_or_else(|| "nothing".into())
            ));
        }
        if let Some(want) = d.pointer {
            out.push(format!(
                "pointer: wanted {want:?}, mouse reports {:?}",
                actual.pointer
            ));
        }
        for (k, a) in &d.buttons {
            let got = actual
                .buttons
                .get(k)
                .cloned()
                .unwrap_or(ButtonAction::Default);
            out.push(format!("{}: wanted {a}, mouse reports {got}", k.name()));
        }
        out
    }
}

fn describe_lighting(l: &Lighting) -> String {
    let mut s = l.effect.name().to_string();
    if l.effect.has_color() {
        s.push_str(&format!(" {}", l.color));
    }
    if l.effect.has_brightness_and_speed() {
        s.push_str(&format!(
            " brightness {} speed {}",
            l.brightness.raw(),
            l.speed.raw()
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buttons::{ConsumerFunction, LoopMode, MacroEvent, MouseFunction};
    use crate::lighting::{Brightness, Effect, Rgb, Speed};

    #[test]
    fn round_trip() {
        let mut buttons = BTreeMap::new();
        buttons.insert(
            Key::Key4,
            ButtonAction::Consumer(ConsumerFunction::VolumeUp),
        );
        buttons.insert(Key::Key5, ButtonAction::Mouse(MouseFunction::Middle));
        let mut macros = BTreeMap::new();
        macros.insert(
            Key::Key6,
            Macro {
                name: "Custom 1".into(),
                loop_mode: LoopMode::Times(3),
                fixed_delay_ms: 25,
                events: vec![
                    MacroEvent::KeyDown(4),
                    MacroEvent::Delay(47),
                    MacroEvent::KeyUp(4),
                ],
            },
        );
        let s = Settings {
            schema: 1,
            device: Some(SettingsDevice {
                model: "Keychron M1".into(),
                firmware: Some("1.0.0r-5".into()),
            }),
            lighting: Some(Lighting {
                effect: Effect::Breathing,
                brightness: Brightness::new(0x85),
                speed: Speed::new(0x80),
                color: Rgb::new(0, 0x0c, 0xff),
            }),
            dpi: Some(Dpi {
                stages: [450, 800, 1600, 3200, 5000],
                active: 3,
            }),
            pointer: Some(PointerConfig {
                report_rate: ReportRate::Hz500,
                debounce_ms: Debounce(11),
                scroll_reverse: true,
                ..PointerConfig::default()
            }),
            buttons,
            macros,
            sent_macros: BTreeMap::new(),
        };
        let text = s.to_toml().unwrap();
        assert!(text.starts_with("schema = 1\n"), "{text}");
        assert!(
            text.contains("[lighting]\neffect = \"breathing\""),
            "{text}"
        );
        assert!(text.contains("color = \"#000cff\""), "{text}");
        assert!(
            text.contains("stages = [450, 800, 1600, 3200, 5000]"),
            "{text}"
        );
        assert!(text.contains("report_rate = 500"), "{text}");
        assert!(text.contains("lift_off_mm = 1"), "{text}");
        assert!(text.contains("key4 = \"consumer:volume-up\""), "{text}");
        assert!(text.contains("[macros.key6]"), "{text}");
        assert!(text.contains("loop = \"times:3\""), "{text}");
        assert!(
            text.contains("events = [\"a+\", \"47ms\", \"a-\"]"),
            "{text}"
        );
        assert_eq!(Settings::from_toml(&text).unwrap(), s);
    }

    #[test]
    fn minimal_file_and_defaults() {
        let s = Settings::from_toml("schema = 1\n[lighting]\neffect = \"snake\"\n").unwrap();
        let l = s.lighting.unwrap();
        assert_eq!(l.effect, Effect::Snake);
        assert_eq!(l.brightness, Brightness::MAX);
        assert_eq!(l.color, Rgb::WHITE);
        assert!(s.device.is_none() && s.dpi.is_none() && s.buttons.is_empty());
        let s =
            Settings::from_toml("schema = 1\n[buttons]\n\"scroll-up\" = \"disabled\"\n").unwrap();
        assert_eq!(s.buttons[&Key::ScrollUp], ButtonAction::Disabled);
    }

    #[test]
    fn rejects_future_schema_and_garbage() {
        assert!(matches!(
            Settings::from_toml("schema = 2\n"),
            Err(SettingsError::SchemaTooNew {
                found: 2,
                supported: 1
            })
        ));
        assert!(matches!(
            Settings::from_toml("schema = 1\n[lighting]\neffect = \"disco\"\n"),
            Err(SettingsError::Parse(_))
        ));
        assert!(matches!(
            Settings::from_toml("schema = 1\n[lighting]\neffect = \"static\"\ncolor = \"red\"\n"),
            Err(SettingsError::Parse(_))
        ));
        assert!(Settings::from_toml("schema = 1\n[pointer]\nreport_rate = 250\n").is_err());
        assert!(Settings::from_toml("schema = 1\n[buttons]\nkey4 = \"teleport\"\n").is_err());
        assert!(Settings::from_toml("not toml at all [[[").is_err());
    }

    #[test]
    fn diff_only_reports_changes() {
        let mut current = Settings {
            lighting: Some(Lighting::static_color(Rgb::GREEN)),
            dpi: Some(Dpi::FACTORY),
            pointer: Some(PointerConfig::default()),
            ..Default::default()
        };
        current
            .buttons
            .insert(Key::Key5, ButtonAction::Mouse(MouseFunction::Middle));

        let mut want = current.clone();
        want.lighting = Some(Lighting::static_color(Rgb::RED));
        want.buttons.insert(Key::Key4, ButtonAction::Disabled);
        // key5 explicitly back to factory (Backward) via "default".
        want.buttons.insert(Key::Key5, ButtonAction::Default);
        // key2 listed as its factory function: no write needed.
        want.buttons
            .insert(Key::Key2, ButtonAction::Mouse(MouseFunction::Right));

        let d = want.diff_against(&current);
        assert_eq!(d.lighting, Some(Lighting::static_color(Rgb::RED)));
        assert!(d.dpi.is_none() && d.pointer.is_none());
        assert_eq!(d.buttons.len(), 2, "{:?}", d.buttons);
        assert_eq!(d.buttons[&Key::Key4], ButtonAction::Disabled);
        assert_eq!(d.buttons[&Key::Key5], ButtonAction::Default);
        assert!(want.diff_against(&want).lighting.is_none());
        // The stored color of an effect without one does not count.
        let rainbow = Lighting::default();
        let cur = Settings::with_lighting(Lighting {
            color: Rgb::RED,
            ..rainbow
        });
        assert!(Settings::with_lighting(rainbow)
            .diff_against(&cur)
            .lighting
            .is_none());
        assert!(Settings::with_lighting(rainbow).mismatches(&cur).is_empty());
    }

    #[test]
    fn mismatches_describe_what_did_not_land() {
        let want = Settings {
            lighting: Some(Lighting::static_color(Rgb::RED)),
            dpi: Some(Dpi::FACTORY),
            ..Default::default()
        };
        let mut got = want.clone();
        assert!(want.mismatches(&got).is_empty());
        got.lighting = Some(Lighting::static_color(Rgb::GREEN));
        got.dpi = Some(Dpi {
            active: 0,
            ..Dpi::FACTORY
        });
        let m = want.mismatches(&got);
        assert_eq!(m.len(), 2, "{m:?}");
        assert!(
            m[0].contains("static #ff0000") && m[0].contains("#00ff00"),
            "{m:?}"
        );
        assert!(m[1].starts_with("dpi:"), "{m:?}");
    }

    #[test]
    fn macro_fingerprints_and_state_bookkeeping() {
        let m = Macro {
            name: "Custom 1".into(),
            loop_mode: LoopMode::Times(3),
            fixed_delay_ms: 25,
            events: vec![MacroEvent::KeyDown(4), MacroEvent::KeyUp(4)],
        };
        let fp = macro_fingerprint(Key::Key6, &m);
        assert!(fp.starts_with("fnv1a64:") && fp.len() == 24, "{fp}");
        assert_eq!(fp, macro_fingerprint(Key::Key6, &m.clone()));
        assert_ne!(fp, macro_fingerprint(Key::Key7, &m));
        let renamed = Macro {
            name: "Custom 2".into(),
            ..m.clone()
        };
        assert_ne!(fp, macro_fingerprint(Key::Key6, &renamed));

        let mut state = Settings::default();
        let mut applied = Settings::default();
        applied.macros.insert(Key::Key6, m.clone());
        state.record_applied(&applied);
        assert_eq!(state.sent_macros[&Key::Key6], fp);
        let text = state.to_toml().unwrap();
        assert!(text.contains("[sent_macros]"), "{text}");
        assert_eq!(Settings::from_toml(&text).unwrap(), state);

        // A later button write on key6 forgets the macro and its fingerprint.
        let mut remap = Settings::default();
        remap.buttons.insert(Key::Key6, ButtonAction::Disabled);
        state.record_applied(&remap);
        assert!(state.macros.is_empty() && state.sent_macros.is_empty());
    }
}
