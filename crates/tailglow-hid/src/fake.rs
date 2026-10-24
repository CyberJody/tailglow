// SPDX-License-Identifier: GPL-3.0-or-later
//! `FakeMouse`: a [`Transport`] that answers queries like the real M1 and
//! remembers every write.
//!
//! The built-in reply sets are session 1 (`testdata/session1.pcap`, factory
//! state) and session 2's baseline (the settings read at its start: static green,
//! stage 3 active). [`FakeMouse::from_pcap`] seeds from any capture.
//!
//! The fake is **stateful**: lighting writes (0x22-0x28) update the 0x12-0x17
//! tables, 0x40-0x43 update the 0x07 reply, 0x52 updates the 0x61 flags and
//! 0x62 mappings, and 0x0b resets behave like the captured resets. That lets
//! the CLI/GUI "set then read back" paths be tested without hardware. The
//! real mouse's behaviour for never-captured opcodes is unknown; those get an
//! echo frame.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use tailglow_core::buttons::{ButtonAction, Key};
use tailglow_core::pointer::ResetKind;
use tailglow_core::protocol::{
    self, opcode, ButtonFlags, ButtonMapping, Command, LightingState, ProtocolError,
    LONG_REPORT_ID, LONG_REPORT_LEN, REPORT_ID, REPORT_LEN,
};
use tailglow_core::{Transport, TransportError};

use crate::capture::{Capture, CaptureError};

/// Session-1 replies, verbatim from the capture (factory state, see PROTOCOL.md).
const SESSION1_REPLIES: &[&str] = &[
    "51 06 01 00 34 34 35 d0 00 01 00 64 02 00 00 00 00 00 00 00 00",
    "51 07 00 60 60 40 90 01 20 03 40 06 80 0c 88 13 35 05 08 00 00",
    "51 12 01 06 03 ff ff cf cf cd cd cf cd 00 00 00 00 00 00 00 00",
    "51 13 ff ff ff ff ff ff 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 17 80 80 80 80 80 80 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 14 ff ff ff ff ff ff 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 15 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 16 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 04 09 31 2e 30 2e 30 72 2d 35 00 00 00 00 00 00 00 00 00 00",
];

/// Session-2 baseline (read at 22:35:58): factory plus static
/// green with Static selected and USB slot 0x22 (stage 3 active, 1000 Hz).
const SESSION2_REPLIES: &[&str] = &[
    "51 06 01 00 34 34 35 d0 00 01 00 64 02 00 00 00 00 00 00 00 00",
    "51 07 00 22 60 40 90 01 20 03 40 06 80 0c 88 13 35 05 08 00 00",
    "51 12 01 06 01 ff ff cf cf cd cd cf cd 00 00 00 00 00 00 00 00",
    "51 13 ff ff ff ff ff ff 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 17 80 80 80 80 80 80 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 14 00 ff ff ff ff ff 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 15 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 16 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
    "51 04 09 31 2e 30 2e 30 72 2d 35 00 00 00 00 00 00 00 00 00 00",
];

pub struct FakeMouse {
    /// Replies for the 0x51 opcodes that are not modelled (0x04, 0x06, …).
    replies: BTreeMap<u8, Vec<u8>>,
    /// Current 0x07 reply (updated by 0x40-0x43).
    pointer: [u8; REPORT_LEN],
    lighting: Option<LightingState>,
    buttons: BTreeMap<Key, ButtonAction>,
    /// Keys the fake has received a macro for (read back as type 0x0a, a
    /// placeholder: the real read-back of a macro key was never captured).
    macros: BTreeMap<Key, String>,
    /// Reply armed by the last query write, returned by the next GET_REPORT.
    pending: Option<Vec<u8>>,
    writes: Vec<Vec<u8>>,
    record_to: Option<PathBuf>,
    label: String,
}

fn parse_set(lines: &[&str]) -> BTreeMap<u8, Vec<u8>> {
    lines
        .iter()
        .map(|s| {
            let v = protocol::from_hex(s).expect("constant");
            (v[1], v)
        })
        .collect()
}

impl FakeMouse {
    fn from_replies(mut replies: BTreeMap<u8, Vec<u8>>, label: String) -> FakeMouse {
        let mut pointer = [0u8; REPORT_LEN];
        if let Some(p) = replies.remove(&opcode::POINTER) {
            pointer[..p.len().min(REPORT_LEN)].copy_from_slice(&p[..p.len().min(REPORT_LEN)]);
        }
        let get = |m: &BTreeMap<u8, Vec<u8>>, op: u8| -> Option<Vec<u8>> {
            m.get(&op).map(|v| {
                let mut r = v.clone();
                r.resize(REPORT_LEN, 0);
                r
            })
        };
        let lighting = match (
            get(&replies, opcode::LIGHT_EFFECT),
            get(&replies, opcode::LIGHT_BRIGHTNESS),
            get(&replies, opcode::LIGHT_SPEED),
            get(&replies, opcode::LIGHT_RED),
            get(&replies, opcode::LIGHT_GREEN),
            get(&replies, opcode::LIGHT_BLUE),
        ) {
            (Some(e), Some(b), Some(s), Some(r), Some(g), Some(bl)) => {
                LightingState::assemble(&e, &b, &s, &r, &g, &bl).ok()
            }
            _ => None,
        };
        for op in [
            opcode::LIGHT_EFFECT,
            opcode::LIGHT_BRIGHTNESS,
            opcode::LIGHT_SPEED,
            opcode::LIGHT_RED,
            opcode::LIGHT_GREEN,
            opcode::LIGHT_BLUE,
        ] {
            if lighting.is_some() {
                replies.remove(&op);
            }
        }
        FakeMouse {
            replies,
            pointer,
            lighting,
            buttons: BTreeMap::new(),
            macros: BTreeMap::new(),
            pending: None,
            writes: Vec::new(),
            record_to: None,
            label,
        }
    }

    /// The M1 as it answered in session 1 (factory state).
    pub fn session1() -> FakeMouse {
        FakeMouse::from_replies(
            parse_set(SESSION1_REPLIES),
            "fake mouse (session 1 replies)".into(),
        )
    }

    /// The M1 as it answered at the start of session 2 (the baseline).
    pub fn session2() -> FakeMouse {
        FakeMouse::from_replies(
            parse_set(SESSION2_REPLIES),
            "fake mouse (session 2 baseline)".into(),
        )
    }

    pub fn from_capture(cap: &Capture) -> FakeMouse {
        FakeMouse::from_replies(
            cap.replies.clone(),
            format!("fake mouse (capture, usb device {})", cap.devnum),
        )
    }

    pub fn from_pcap(path: impl AsRef<Path>) -> Result<FakeMouse, CaptureError> {
        let cap = Capture::load(path.as_ref())?;
        let mut fake = FakeMouse::from_capture(&cap);
        fake.label = format!("fake mouse ({})", path.as_ref().display());
        Ok(fake)
    }

    /// Append every write to `path`, one hex line per report.
    pub fn record_to(mut self, path: impl Into<PathBuf>) -> FakeMouse {
        self.record_to = Some(path.into());
        self
    }

    /// Replaces the reply for one unmodelled 0x51 opcode (e.g. 0x06 for a
    /// low battery, or a different VID:PID).
    pub fn set_reply(&mut self, opcode: u8, reply: Vec<u8>) {
        self.replies.insert(opcode, reply);
    }

    /// Pre-sets a button mapping (as if the mouse already had it).
    pub fn set_button(&mut self, key: Key, action: ButtonAction) {
        if action == ButtonAction::Default {
            self.buttons.remove(&key);
        } else {
            self.buttons.insert(key, action);
        }
    }

    /// Every report written so far, in order.
    pub fn writes(&self) -> &[Vec<u8>] {
        &self.writes
    }

    /// Writes decoded as commands (queries included).
    pub fn commands(&self) -> Vec<Result<Command, ProtocolError>> {
        self.writes.iter().map(|w| Command::decode(w)).collect()
    }

    /// Only the non-query writes that decode, in order.
    pub fn lighting_writes(&self) -> Vec<Command> {
        self.commands()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|c| !c.is_query())
            .collect()
    }

    pub fn clear_writes(&mut self) {
        self.writes.clear();
    }

    /// Current lighting tables (what 0x12-0x17 would return).
    pub fn lighting_state(&self) -> Option<&LightingState> {
        self.lighting.as_ref()
    }

    /// Parses a recording written via [`FakeMouse::record_to`].
    pub fn read_recording(path: impl AsRef<Path>) -> std::io::Result<Vec<Vec<u8>>> {
        let text = std::fs::read_to_string(path)?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                protocol::from_hex(l)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })
            .collect()
    }

    // ----- state model -----------------------------------------------------

    fn reply_short(&self, op: u8) -> Vec<u8> {
        let mut r = match op {
            opcode::POINTER => self.pointer.to_vec(),
            opcode::LIGHT_EFFECT
            | opcode::LIGHT_BRIGHTNESS
            | opcode::LIGHT_SPEED
            | opcode::LIGHT_RED
            | opcode::LIGHT_GREEN
            | opcode::LIGHT_BLUE => match &self.lighting {
                Some(l) => {
                    let idx = match op {
                        opcode::LIGHT_EFFECT => 0,
                        opcode::LIGHT_BRIGHTNESS => 1,
                        opcode::LIGHT_SPEED => 2,
                        opcode::LIGHT_RED => 3,
                        opcode::LIGHT_GREEN => 4,
                        _ => 5,
                    };
                    l.replies()[idx].to_vec()
                }
                None => self.replies.get(&op).cloned().unwrap_or_default(),
            },
            other => self.replies.get(&other).cloned().unwrap_or_default(),
        };
        if r.is_empty() {
            r = vec![0u8; REPORT_LEN];
            r[0] = REPORT_ID;
            r[1] = op;
        }
        r.resize(REPORT_LEN, 0);
        r
    }

    fn reply_long(&self, op: u8, arg: u8) -> Vec<u8> {
        match op {
            opcode::BUTTON_FLAGS => {
                let mut remapped = [false; Key::COUNT];
                for k in self.buttons.keys().chain(self.macros.keys()) {
                    remapped[k.index() as usize] = true;
                }
                ButtonFlags { remapped }.encode().to_vec()
            }
            opcode::BUTTON_MAPPING => {
                let key = Key::from_index(arg).unwrap_or(Key::Key1);
                let action = if self.macros.contains_key(&key) {
                    // Placeholder: the real read-back of a macro key is unknown.
                    ButtonAction::Raw {
                        type_byte: 0x0a,
                        payload: vec![],
                    }
                } else {
                    self.buttons
                        .get(&key)
                        .cloned()
                        .unwrap_or(ButtonAction::Default)
                };
                ButtonMapping { key, action }.encode().to_vec()
            }
            other => {
                let mut r = vec![0u8; LONG_REPORT_LEN];
                r[0] = LONG_REPORT_ID;
                r[1] = other;
                r
            }
        }
    }

    fn apply_write(&mut self, cmd: &Command) {
        match cmd {
            Command::SetEffect(_)
            | Command::SetBrightness { .. }
            | Command::SetSpeed { .. }
            | Command::SetColor { .. } => {
                if let Some(l) = &mut self.lighting {
                    l.apply(cmd);
                }
            }
            Command::SetDpi(d) => {
                // Active stage into the low nibble of all three slots; the
                // factory flag (bit 6) clears on the first write (*likely*).
                for s in &mut self.pointer[3..6] {
                    *s = (*s & 0x30) | (d.active & 0x0f);
                }
                for (i, st) in d.stages.iter().enumerate() {
                    self.pointer[6 + 2 * i..8 + 2 * i].copy_from_slice(&st.to_le_bytes());
                }
                self.pointer[17] = 5;
            }
            Command::SetReportRate(r) => {
                // Slots 0 and 1 take the rate bits; slot 2 never did (*likely*).
                for s in &mut self.pointer[3..5] {
                    *s = (*s & 0x0f) | (r.index() << 4);
                }
                self.pointer[5] &= 0x0f;
            }
            Command::SetSensor(s) => {
                let mut ff = 0u8;
                ff |= s.lift_off.mm();
                if s.ripple {
                    ff |= 0x04;
                }
                if s.angle_snap {
                    ff |= 0x08;
                }
                if s.motion_sync {
                    ff |= 0x10;
                }
                if s.scroll_reverse {
                    ff |= 0x40;
                }
                self.pointer[16] = ff;
            }
            Command::SetDebounce(d) => self.pointer[18] = d.ms(),
            Command::Reset(kind) => match kind {
                ResetKind::Buttons => {
                    self.buttons.clear();
                    self.macros.clear();
                }
                ResetKind::Lighting => self.lighting = Some(LightingState::factory()),
                ResetKind::Factory => {
                    self.buttons.clear();
                    self.macros.clear();
                    self.lighting = Some(LightingState::factory());
                    let factory = protocol::from_hex(SESSION1_REPLIES[1]).expect("constant");
                    self.pointer.copy_from_slice(&factory);
                }
            },
            Command::SetButton { key, action } => {
                self.macros.remove(key);
                if *action == ButtonAction::Default {
                    self.buttons.remove(key);
                } else {
                    self.buttons.insert(*key, action.clone());
                }
            }
            Command::SetMacroEvents { key, .. } => {
                self.buttons.remove(key);
                self.macros.insert(*key, String::new());
            }
            Command::SetMacroName { key, name } => {
                self.macros.insert(*key, name.clone());
            }
            Command::Query(_) | Command::QueryLong { .. } => {}
        }
    }
}

impl Transport for FakeMouse {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), TransportError> {
        match (report.first(), report.len()) {
            (Some(&REPORT_ID), REPORT_LEN) | (Some(&LONG_REPORT_ID), LONG_REPORT_LEN) => {}
            (Some(id), len) => {
                return Err(TransportError::BadReport(format!(
                    "report 0x{id:02x} with {len} bytes; the mouse takes 0x51/21 or 0x52/65"
                )))
            }
            (None, _) => return Err(TransportError::BadReport("empty report".into())),
        }
        self.writes.push(report.to_vec());
        if let Some(path) = &self.record_to {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| TransportError::io(path.display().to_string(), e))?;
            writeln!(f, "{}", protocol::hex(report))
                .map_err(|e| TransportError::io(path.display().to_string(), e))?;
        }
        match Command::decode(report) {
            Ok(Command::Query(op)) => self.pending = Some(self.reply_short(op)),
            Ok(Command::QueryLong { op, arg }) => self.pending = Some(self.reply_long(op, arg)),
            Ok(cmd) => self.apply_write(&cmd),
            Err(_) => {
                // Unknown opcode or malformed arguments: echo an empty frame
                // for a query-shaped report, ignore otherwise.
                if report[2..].iter().all(|&b| b == 0) {
                    let mut r = vec![0u8; report.len()];
                    r[0] = report[0];
                    r[1] = report[1];
                    self.pending = Some(r);
                }
            }
        }
        Ok(())
    }

    fn get_feature(&mut self, report_id: u8, len: usize) -> Result<Vec<u8>, TransportError> {
        let mut reply = match self.pending.take() {
            Some(r) if r[0] == report_id => r,
            _ => {
                let mut r = vec![0u8; len];
                if let Some(b) = r.first_mut() {
                    *b = report_id;
                }
                r
            }
        };
        reply.resize(len, 0);
        Ok(reply)
    }

    fn describe(&self) -> String {
        self.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tailglow_core::buttons::{ConsumerFunction, LoopMode, Macro, MacroEvent, MouseFunction};
    use tailglow_core::pointer::{Debounce, Dpi, ReportRate, SensorSettings};
    use tailglow_core::protocol::hex;
    use tailglow_core::{Brightness, Effect, Lighting, Mouse, Rgb};

    #[test]
    fn info_parses_from_session1_replies() {
        let mut mouse = Mouse::new(FakeMouse::session1());
        let info = mouse.read_info().unwrap();
        assert_eq!(info.device.vid, 0x3434);
        assert_eq!(info.device.pid, 0xd035);
        assert_eq!(info.device.battery_percent, 100);
        assert_eq!(info.firmware.version, "1.0.0r-5");
        assert_eq!(info.pointer.dpi.stages, [400, 800, 1600, 3200, 5000]);
        assert_eq!(info.pointer.dpi.active, 0);
        assert_eq!(info.model.unwrap().name, "Keychron M1");
        assert_eq!(mouse.model().unwrap().name, "Keychron M1");
        let fake = mouse.into_transport();
        assert_eq!(
            fake.commands()
                .into_iter()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            vec![
                Command::Query(opcode::DEVICE_INFO),
                Command::Query(opcode::FIRMWARE),
                Command::Query(opcode::POINTER)
            ]
        );
    }

    #[test]
    fn session2_baseline_reads_like_baseline_md() {
        let mut mouse = Mouse::new(FakeMouse::session2());
        let snap = mouse.snapshot().unwrap();
        assert_eq!(snap.info.pointer.dpi.active, 2);
        assert_eq!(snap.info.pointer.report_rate(), Some(ReportRate::Hz1000));
        assert_eq!(snap.info.pointer.debounce.ms(), 8);
        assert_eq!(snap.info.pointer.sensor, SensorSettings::FACTORY);
        assert_eq!(snap.lighting.effect, Effect::Static);
        assert_eq!(snap.lighting.current().color, Rgb::GREEN);
        assert!(snap.buttons.actions.is_empty());
        // The read sequence is the app's: 06 04 07, then 12 13 17 14 15 16, then 52 61.
        let ops: Vec<String> = mouse
            .transport()
            .writes()
            .iter()
            .map(|w| hex(&w[..2]))
            .collect();
        assert_eq!(
            ops,
            [
                "51 06", "51 04", "51 07", "51 12", "51 13", "51 17", "51 14", "51 15", "51 16",
                "52 61"
            ]
        );
    }

    #[test]
    fn pcap_backed_fake_matches_builtin() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/session1.pcap");
        let mut a = Mouse::new(FakeMouse::from_pcap(path).unwrap());
        let mut b = Mouse::new(FakeMouse::session1());
        assert_eq!(a.read_info().unwrap(), b.read_info().unwrap());
        for op in opcode::STARTUP_QUERIES {
            assert_eq!(a.query(op).unwrap(), b.query(op).unwrap(), "0x{op:02x}");
        }
    }

    #[test]
    fn apply_lighting_records_exact_bytes_and_reads_back() {
        let mut mouse = Mouse::new(FakeMouse::session1());
        mouse.read_info().unwrap();
        mouse.transport_mut().clear_writes();
        let want = Lighting {
            effect: Effect::Static,
            brightness: Brightness::new(0x85),
            color: Rgb::new(0xab, 0x34, 0x56),
            ..Default::default()
        };
        mouse.apply_lighting(&want).unwrap();
        let w: Vec<String> = mouse.transport().writes().iter().map(|w| hex(w)).collect();
        assert_eq!(
            w,
            vec![
                "51 22 01 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
                "51 23 01 01 85 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
                "51 27 01 01 80 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
                "51 28 01 01 ab 34 56 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
            ]
        );
        let state = mouse.lighting().unwrap();
        assert_eq!(state.current(), want);
        // Other effects keep their factory parameters.
        assert_eq!(state.for_effect(Effect::Rainbow).color, Rgb::RED);
    }

    #[test]
    fn writes_are_refused_before_identification() {
        let mut mouse = Mouse::new(FakeMouse::session1());
        let err = mouse
            .apply_lighting(&Lighting::static_color(Rgb::RED))
            .unwrap_err();
        assert!(matches!(err, tailglow_core::Error::Refused(_)), "{err}");
        assert!(mouse.transport().writes().is_empty());
    }

    #[test]
    fn likely_model_needs_opt_in() {
        let mut fake = FakeMouse::session1();
        // Pretend to be an M3 (3434:d032).
        fake.set_reply(
            opcode::DEVICE_INFO,
            protocol::from_hex("51 06 01 00 34 34 32 d0 00 01 00 64 02 00 00 00 00 00 00 00 00")
                .unwrap(),
        );
        let mut mouse = Mouse::new(fake);
        let info = mouse.read_info().unwrap();
        assert_eq!(info.model_name(), "Keychron M3");
        let err = mouse
            .apply_lighting(&Lighting::static_color(Rgb::RED))
            .unwrap_err();
        assert!(
            matches!(err, tailglow_core::Error::Untested { .. }),
            "{err}"
        );
        mouse.allow_untested(true);
        mouse
            .apply_lighting(&Lighting::static_color(Rgb::RED))
            .unwrap();
        // Unsupported family: never, even with the opt-in.
        let mut fake = FakeMouse::session1();
        fake.set_reply(
            opcode::DEVICE_INFO,
            protocol::from_hex("51 06 01 00 34 34 50 d0 00 01 00 64 02 00 00 00 00 00 00 00 00")
                .unwrap(),
        );
        let mut mouse = Mouse::new(fake);
        mouse.read_info().unwrap();
        mouse.allow_untested(true);
        let err = mouse.apply_dpi(&Dpi::FACTORY).unwrap_err();
        assert!(
            matches!(err, tailglow_core::Error::WrongFamily { .. }),
            "{err}"
        );
    }

    #[test]
    fn pointer_writes_read_back() {
        let mut mouse = Mouse::new(FakeMouse::session2());
        mouse.read_info().unwrap();
        let dpi = Dpi {
            stages: [450, 800, 1600, 4900, 5000],
            active: 3,
        };
        mouse.apply_dpi(&dpi).unwrap();
        mouse.apply_report_rate(ReportRate::Hz500).unwrap();
        let sensor = SensorSettings {
            lift_off: tailglow_core::pointer::LiftOff::Mm2,
            ripple: false,
            angle_snap: true,
            motion_sync: false,
            scroll_reverse: true,
        };
        mouse.apply_sensor(&sensor).unwrap();
        mouse.apply_debounce(Debounce::new(11).unwrap()).unwrap();
        let p = mouse.pointer().unwrap();
        assert_eq!(p.dpi, dpi);
        assert_eq!(p.report_rate(), Some(ReportRate::Hz500));
        assert_eq!(p.sensor, sensor);
        assert_eq!(
            p.sensor_flags, 0x4a,
            "matches the relaunch read in session 2"
        );
        assert_eq!(p.debounce.ms(), 11);
        assert_eq!(p.slots[0], 0x13, "500 Hz + stage 3, as captured");
        // Invalid values never reach the wire.
        let n = mouse.transport().writes().len();
        assert!(mouse
            .apply_dpi(&Dpi {
                stages: [410, 800, 1600, 3200, 5000],
                active: 0
            })
            .is_err());
        assert!(mouse.apply_debounce(Debounce(0)).is_err());
        assert_eq!(mouse.transport().writes().len(), n);
    }

    #[test]
    fn button_writes_read_back_and_left_click_is_guarded() {
        let mut mouse = Mouse::new(FakeMouse::session2());
        mouse.read_info().unwrap();
        let map = mouse.buttons().unwrap();
        assert!(map.actions.is_empty());
        mouse
            .apply_button(
                Key::Key4,
                &ButtonAction::Consumer(ConsumerFunction::VolumeUp),
                Some(&map),
            )
            .unwrap();
        let map = mouse.buttons().unwrap();
        assert_eq!(map.flags.remapped_keys(), vec![Key::Key4]);
        assert_eq!(
            map.actions[&Key::Key4],
            ButtonAction::Consumer(ConsumerFunction::VolumeUp)
        );
        // Key 1 is the only left click: refused.
        let err = mouse
            .apply_button(Key::Key1, &ButtonAction::Disabled, Some(&map))
            .unwrap_err();
        assert!(matches!(err, tailglow_core::Error::Refused(_)), "{err}");
        // Give Key 5 left click, then Key 1 may change.
        mouse
            .apply_button(
                Key::Key5,
                &ButtonAction::Mouse(MouseFunction::Left),
                Some(&map),
            )
            .unwrap();
        let map = mouse.buttons().unwrap();
        assert_eq!(map.left_click_keys(), vec![Key::Key1, Key::Key5]);
        mouse
            .apply_button(
                Key::Key1,
                &ButtonAction::Mouse(MouseFunction::Right),
                Some(&map),
            )
            .unwrap();
        // Macro assignment: 0x54 then 0x53; the key is then flagged.
        let m = Macro {
            name: "Custom 1".into(),
            loop_mode: LoopMode::Times(3),
            fixed_delay_ms: 25,
            events: vec![
                MacroEvent::KeyDown(4),
                MacroEvent::Delay(47),
                MacroEvent::KeyUp(4),
            ],
        };
        let map = mouse.buttons().unwrap();
        mouse.apply_macro(Key::Key6, &m, Some(&map)).unwrap();
        let last: Vec<String> = mouse
            .transport()
            .writes()
            .iter()
            .rev()
            .take(2)
            .map(|w| hex(&w[..4]))
            .collect();
        assert_eq!(last, ["52 53 05 08", "52 54 05 00"]);
        assert!(mouse.buttons().unwrap().flags.is_remapped(Key::Key6));
        // Reset buttons clears everything.
        mouse.reset(ResetKind::Buttons).unwrap();
        assert!(mouse.buttons().unwrap().actions.is_empty());
    }

    #[test]
    fn rejects_malformed_reports() {
        let mut fake = FakeMouse::session1();
        assert!(fake.send_feature(&[0x51; 20]).is_err());
        assert!(fake.send_feature(&[0x50; 21]).is_err());
        assert!(fake.send_feature(&[]).is_err());
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn unknown_query_gets_empty_frame_and_get_without_query_is_safe() {
        let mut fake = FakeMouse::session1();
        let r = fake.get_feature(0x51, 21).unwrap();
        assert_eq!(r[0], 0x51);
        assert!(r[1..].iter().all(|&b| b == 0));
        fake.send_feature(&Command::Query(0x7f).encode()).unwrap();
        let r = fake.get_feature(0x51, 21).unwrap();
        assert_eq!(&r[..2], &[0x51, 0x7f]);
    }

    #[test]
    fn recording_round_trip() {
        let dir = std::env::temp_dir().join(format!("tailglow-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("writes.hex");
        let _ = std::fs::remove_file(&path);
        let mut fake = FakeMouse::session1().record_to(&path);
        fake.send_feature(&Command::SetEffect(Effect::Snake).encode())
            .unwrap();
        fake.send_feature(&Command::Query(0x06).encode()).unwrap();
        let rec = FakeMouse::read_recording(&path).unwrap();
        assert_eq!(rec, fake.writes());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
