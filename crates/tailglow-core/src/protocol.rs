// SPDX-License-Identifier: GPL-3.0-or-later
//! Bytes on the wire.
//!
//! The vendor interface (USB interface 2 on the M1) speaks HID **feature
//! reports** with two report IDs (PROTOCOL.md §1, *verified*):
//!
//! ```text
//! 51 <cmd> <args…>   21 bytes: settings, lighting, reads
//! 52 <cmd> <args…>   65 bytes: button map, macros
//! ```
//!
//! - Lighting writes carry `01` at byte 2 (*verified* on every write; whether
//!   it means "write" or "profile 1" is *unknown*).
//! - Reads are a write of `<id> <cmd> [arg] 00…` followed by a GET_REPORT of the
//!   same report ID; the reply echoes `<id> <cmd>` and carries the payload.
//!
//! Every byte layout here comes from `testdata/session1.pcap` and
//! `testdata/session2.pcap`; the unit tests pin them to the captured strings.

use crate::buttons::{ButtonAction, Key, Macro};
use crate::lighting::{Brightness, Effect, Lighting, Rgb, Speed};
use crate::pointer::{Debounce, Dpi, ReportRate, ResetKind, SensorSettings};
use thiserror::Error;

/// Report ID of the short command channel.
pub const REPORT_ID: u8 = 0x51;
/// Length of every `0x51` report, report ID included.
pub const REPORT_LEN: usize = 21;
/// Report ID of the long channel (buttons, macros, profile blob).
pub const LONG_REPORT_ID: u8 = 0x52;
/// Length of every `0x52` report, report ID included.
pub const LONG_REPORT_LEN: usize = 65;
/// The byte at offset 2 on every lighting write (see module docs).
pub const WRITE_FLAG: u8 = 0x01;

/// A full short feature report, report ID first.
pub type Report = [u8; REPORT_LEN];
/// A full long feature report, report ID first.
pub type LongReport = [u8; LONG_REPORT_LEN];

/// Command opcodes (byte 1). Reads and writes share the number space.
pub mod opcode {
    /// Reply: `09 "1.0.0r-5"` — firmware version, Pascal-style string.
    pub const FIRMWARE: u8 = 0x04;
    /// Reply: `01 00 <vid le16> <pid le16> 00 01 00 <battery> 02`.
    pub const DEVICE_INFO: u8 = 0x06;
    /// Reply: profile, 3 slot bytes, five LE16 DPI stages, sensor flags, stage count, debounce.
    pub const POINTER: u8 = 0x07;
    /// Write: reset (`01 01` buttons, `01 04` lighting, `aa` factory).
    pub const RESET: u8 = 0x0b;
    /// Reply `01 06 <effect> ff ff cf cf cd cd cf cd`: current effect at byte 4.
    pub const LIGHT_EFFECT: u8 = 0x12;
    /// Reply: brightness per effect (6 bytes, index = effect id − 1).
    pub const LIGHT_BRIGHTNESS: u8 = 0x13;
    /// Reply: red per effect.
    pub const LIGHT_RED: u8 = 0x14;
    /// Reply: green per effect.
    pub const LIGHT_GREEN: u8 = 0x15;
    /// Reply: blue per effect.
    pub const LIGHT_BLUE: u8 = 0x16;
    /// Reply: speed per effect.
    pub const LIGHT_SPEED: u8 = 0x17;
    /// Lighting: select effect. `51 22 01 <effect>`.
    pub const SET_EFFECT: u8 = 0x22;
    /// Lighting: brightness. `51 23 01 <effect> <0x0c..=0xff>`.
    pub const SET_BRIGHTNESS: u8 = 0x23;
    /// Lighting: speed. `51 27 01 <effect> <0x14..=0xff>`.
    pub const SET_SPEED: u8 = 0x27;
    /// Lighting: color. `51 28 01 <effect> <r> <g> <b>`.
    pub const SET_COLOR: u8 = 0x28;
    /// Sent once at app launch with no reply read — *unknown*. Not sent by us.
    pub const UNKNOWN_36: u8 = 0x36;
    /// Write: DPI stages + active stage.
    pub const SET_DPI: u8 = 0x40;
    /// Write: report rate.
    pub const SET_REPORT_RATE: u8 = 0x41;
    /// Write: lift-off / ripple / angle snap / motion sync / scroll direction.
    pub const SET_SENSOR: u8 = 0x42;
    /// Write: debounce ms.
    pub const SET_DEBOUNCE: u8 = 0x43;
    /// Write: key-pairing combination (*likely*; never written by tailglow).
    pub const KEY_PAIRING: u8 = 0x46;

    // Report 0x52:
    /// Write: assign one button. `52 52 KK 00 TT <payload>`.
    pub const SET_BUTTON: u8 = 0x52;
    /// Write: macro name. `52 53 KK LEN <ascii>`.
    pub const SET_MACRO_NAME: u8 = 0x53;
    /// Write: macro events. `52 54 KK 00 LEN 01 LC LM DL DL N N <events>`.
    pub const SET_MACRO_EVENTS: u8 = 0x54;
    /// Write: profile blob (*likely*; never written by tailglow).
    pub const PROFILE_SWITCH: u8 = 0x57;
    /// Reply: one byte per key, 01 = remapped.
    pub const BUTTON_FLAGS: u8 = 0x61;
    /// Reply to `52 62 KK`: `52 62 KK 00 TT <payload>`.
    pub const BUTTON_MAPPING: u8 = 0x62;

    /// The 0x51 queries the vendor app issues at launch, in its order (it also
    /// writes 0x36 and reads 0x52/0x61 in between; we skip 0x36).
    pub const STARTUP_QUERIES: [u8; 9] = [
        DEVICE_INFO,
        POINTER,
        LIGHT_EFFECT,
        LIGHT_BRIGHTNESS,
        LIGHT_SPEED,
        LIGHT_RED,
        LIGHT_GREEN,
        LIGHT_BLUE,
        FIRMWARE,
    ];
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("report is {0} bytes, expected {REPORT_LEN} or {LONG_REPORT_LEN}")]
    BadLength(usize),
    #[error("report ID 0x{0:02x}, expected 0x{REPORT_ID:02x} or 0x{LONG_REPORT_ID:02x}")]
    BadReportId(u8),
    #[error("reply is for command 0x{got:02x}, expected 0x{expected:02x}")]
    WrongCommand { expected: u8, got: u8 },
    #[error("unknown effect id {0}")]
    UnknownEffect(u8),
    #[error("unknown command 0x{0:02x}")]
    UnknownCommand(u8),
    #[error("command 0x{0:02x} is not a write (flag byte is not 0x01)")]
    NotAWrite(u8),
    #[error("command 0x{0:02x}: malformed arguments")]
    BadArguments(u8),
    #[error("invalid value: {0}")]
    Invalid(String),
}

/// A typed command. `encode` produces the exact bytes the vendor app sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `51 <cmd> 00…`: arm a read; follow with a GET_REPORT of 0x51.
    Query(u8),
    /// `52 <cmd> <arg> 00…`: arm a long read; follow with a GET_REPORT of 0x52.
    QueryLong {
        op: u8,
        arg: u8,
    },
    SetEffect(Effect),
    SetBrightness {
        effect: Effect,
        brightness: Brightness,
    },
    SetSpeed {
        effect: Effect,
        speed: Speed,
    },
    SetColor {
        effect: Effect,
        color: Rgb,
    },
    /// `51 40 A A A <5 × LE16> 05`
    SetDpi(Dpi),
    /// `51 41 R R R 7d 00 f4 01 e8 03`
    SetReportRate(ReportRate),
    /// `51 42 LO RP AS MS 00 SD`
    SetSensor(SensorSettings),
    /// `51 43 MS`
    SetDebounce(Debounce),
    /// `51 0b …`
    Reset(ResetKind),
    /// `52 52 KK 00 TT <payload>`
    SetButton {
        key: Key,
        action: ButtonAction,
    },
    /// `52 54 KK 00 LEN …`
    SetMacroEvents {
        key: Key,
        macro_: Macro,
    },
    /// `52 53 KK LEN <name>`
    SetMacroName {
        key: Key,
        name: String,
    },
}

impl Command {
    pub fn report_id(&self) -> u8 {
        match self {
            Command::QueryLong { .. }
            | Command::SetButton { .. }
            | Command::SetMacroEvents { .. }
            | Command::SetMacroName { .. } => LONG_REPORT_ID,
            _ => REPORT_ID,
        }
    }

    pub fn report_len(&self) -> usize {
        if self.report_id() == LONG_REPORT_ID {
            LONG_REPORT_LEN
        } else {
            REPORT_LEN
        }
    }

    pub fn opcode(&self) -> u8 {
        match self {
            Command::Query(c) => *c,
            Command::QueryLong { op, .. } => *op,
            Command::SetEffect(_) => opcode::SET_EFFECT,
            Command::SetBrightness { .. } => opcode::SET_BRIGHTNESS,
            Command::SetSpeed { .. } => opcode::SET_SPEED,
            Command::SetColor { .. } => opcode::SET_COLOR,
            Command::SetDpi(_) => opcode::SET_DPI,
            Command::SetReportRate(_) => opcode::SET_REPORT_RATE,
            Command::SetSensor(_) => opcode::SET_SENSOR,
            Command::SetDebounce(_) => opcode::SET_DEBOUNCE,
            Command::Reset(_) => opcode::RESET,
            Command::SetButton { .. } => opcode::SET_BUTTON,
            Command::SetMacroEvents { .. } => opcode::SET_MACRO_EVENTS,
            Command::SetMacroName { .. } => opcode::SET_MACRO_NAME,
        }
    }

    pub fn is_query(&self) -> bool {
        matches!(self, Command::Query(_) | Command::QueryLong { .. })
    }

    /// Full report, zero padded to 21 or 65 bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut r = vec![0u8; self.report_len()];
        r[0] = self.report_id();
        r[1] = self.opcode();
        match self {
            Command::Query(_) => {}
            Command::QueryLong { arg, .. } => r[2] = *arg,
            Command::SetEffect(e) => {
                r[2] = WRITE_FLAG;
                r[3] = e.id();
            }
            Command::SetBrightness { effect, brightness } => {
                r[2] = WRITE_FLAG;
                r[3] = effect.id();
                r[4] = brightness.raw();
            }
            Command::SetSpeed { effect, speed } => {
                r[2] = WRITE_FLAG;
                r[3] = effect.id();
                r[4] = speed.raw();
            }
            Command::SetColor { effect, color } => {
                r[2] = WRITE_FLAG;
                r[3] = effect.id();
                r[4..7].copy_from_slice(&color.to_array());
            }
            Command::SetDpi(d) => {
                r[2] = d.active;
                r[3] = d.active;
                r[4] = d.active;
                for (i, s) in d.stages.iter().enumerate() {
                    r[5 + 2 * i..7 + 2 * i].copy_from_slice(&s.to_le_bytes());
                }
                r[15] = Dpi::STAGE_COUNT as u8;
            }
            Command::SetReportRate(rate) => {
                r[2] = rate.index();
                r[3] = rate.index();
                r[4] = rate.index();
                for (i, hz) in ReportRate::TABLE.iter().enumerate() {
                    r[5 + 2 * i..7 + 2 * i].copy_from_slice(&hz.to_le_bytes());
                }
            }
            Command::SetSensor(s) => r[2..8].copy_from_slice(&s.payload()),
            Command::SetDebounce(d) => r[2] = d.ms(),
            Command::Reset(kind) => {
                let p = kind.payload();
                r[2..2 + p.len()].copy_from_slice(p);
            }
            Command::SetButton { key, action } => {
                r[2] = key.index();
                r[4] = action.type_byte();
                let p = action.payload();
                let n = p.len().min(LONG_REPORT_LEN - 5);
                r[5..5 + n].copy_from_slice(&p[..n]);
            }
            Command::SetMacroEvents { key, macro_ } => {
                r[2] = key.index();
                let p = macro_.events_payload();
                let n = p.len().min(LONG_REPORT_LEN - 4);
                r[4..4 + n].copy_from_slice(&p[..n]);
            }
            Command::SetMacroName { key, name } => {
                r[2] = key.index();
                let bytes = name.as_bytes();
                let n = bytes.len().min(LONG_REPORT_LEN - 4);
                r[3] = n as u8;
                r[4..4 + n].copy_from_slice(&bytes[..n]);
            }
        }
        r
    }

    /// Inverse of [`Command::encode`] for the commands we know. Used by the
    /// fake mouse and by tests that replay captures.
    pub fn decode(report: &[u8]) -> Result<Command, ProtocolError> {
        match report.first() {
            Some(&REPORT_ID) => Self::decode_short(check_frame(report)?),
            Some(&LONG_REPORT_ID) => Self::decode_long(check_long_frame(report)?),
            Some(&other) => Err(ProtocolError::BadReportId(other)),
            None => Err(ProtocolError::BadLength(0)),
        }
    }

    fn decode_short(r: Report) -> Result<Command, ProtocolError> {
        let cmd = r[1];
        let args = &r[2..];
        if args.iter().all(|&b| b == 0) {
            return Ok(Command::Query(cmd));
        }
        let trimmed = &args[..args
            .iter()
            .rposition(|&b| b != 0)
            .map(|p| p + 1)
            .unwrap_or(0)];
        match cmd {
            opcode::SET_EFFECT | opcode::SET_BRIGHTNESS | opcode::SET_SPEED | opcode::SET_COLOR => {
                if r[2] != WRITE_FLAG {
                    return Err(ProtocolError::NotAWrite(cmd));
                }
                let effect = Effect::from_id(r[3]).ok_or(ProtocolError::UnknownEffect(r[3]))?;
                Ok(match cmd {
                    opcode::SET_EFFECT => Command::SetEffect(effect),
                    opcode::SET_BRIGHTNESS => Command::SetBrightness {
                        effect,
                        brightness: Brightness::new(r[4]),
                    },
                    opcode::SET_SPEED => Command::SetSpeed {
                        effect,
                        speed: Speed::new(r[4]),
                    },
                    _ => Command::SetColor {
                        effect,
                        color: Rgb::new(r[4], r[5], r[6]),
                    },
                })
            }
            opcode::SET_DPI => {
                let mut stages = [0u16; Dpi::STAGE_COUNT];
                for (i, s) in stages.iter_mut().enumerate() {
                    *s = u16::from_le_bytes([r[5 + 2 * i], r[6 + 2 * i]]);
                }
                if r[2] != r[3] || r[3] != r[4] || r[15] != Dpi::STAGE_COUNT as u8 {
                    return Err(ProtocolError::BadArguments(cmd));
                }
                Ok(Command::SetDpi(Dpi {
                    stages,
                    active: r[2],
                }))
            }
            opcode::SET_REPORT_RATE => {
                let rate = ReportRate::from_index(r[2]).ok_or(ProtocolError::BadArguments(cmd))?;
                if Command::SetReportRate(rate).encode() != r {
                    return Err(ProtocolError::BadArguments(cmd));
                }
                Ok(Command::SetReportRate(rate))
            }
            opcode::SET_SENSOR => SensorSettings::from_payload(&r[2..8])
                .map(Command::SetSensor)
                .ok_or(ProtocolError::BadArguments(cmd)),
            opcode::SET_DEBOUNCE => Debounce::new(r[2])
                .map(Command::SetDebounce)
                .map_err(|_| ProtocolError::BadArguments(cmd)),
            opcode::RESET => ResetKind::from_payload(trimmed)
                .map(Command::Reset)
                .ok_or(ProtocolError::BadArguments(cmd)),
            other => Err(ProtocolError::UnknownCommand(other)),
        }
    }

    fn decode_long(r: LongReport) -> Result<Command, ProtocolError> {
        let cmd = r[1];
        match cmd {
            opcode::BUTTON_FLAGS | opcode::BUTTON_MAPPING => {
                Ok(Command::QueryLong { op: cmd, arg: r[2] })
            }
            opcode::SET_BUTTON => {
                let key = Key::from_index(r[2]).ok_or(ProtocolError::BadArguments(cmd))?;
                Ok(Command::SetButton {
                    key,
                    action: ButtonAction::parse_payload(r[4], &r[5..]),
                })
            }
            opcode::SET_MACRO_NAME => {
                let key = Key::from_index(r[2]).ok_or(ProtocolError::BadArguments(cmd))?;
                let n = (r[3] as usize).min(LONG_REPORT_LEN - 4);
                Ok(Command::SetMacroName {
                    key,
                    name: String::from_utf8_lossy(&r[4..4 + n]).into_owned(),
                })
            }
            opcode::SET_MACRO_EVENTS => {
                let key = Key::from_index(r[2]).ok_or(ProtocolError::BadArguments(cmd))?;
                let macro_ = Macro::parse_events_payload(&r[4..], String::new())
                    .ok_or(ProtocolError::BadArguments(cmd))?;
                Ok(Command::SetMacroEvents { key, macro_ })
            }
            other => Err(ProtocolError::UnknownCommand(other)),
        }
    }
}

/// Checks length and report ID of a short report.
pub fn check_frame(report: &[u8]) -> Result<Report, ProtocolError> {
    if report.len() != REPORT_LEN {
        return Err(ProtocolError::BadLength(report.len()));
    }
    if report[0] != REPORT_ID {
        return Err(ProtocolError::BadReportId(report[0]));
    }
    let mut r = [0u8; REPORT_LEN];
    r.copy_from_slice(report);
    Ok(r)
}

/// Checks length and report ID of a long report.
pub fn check_long_frame(report: &[u8]) -> Result<LongReport, ProtocolError> {
    if report.len() != LONG_REPORT_LEN {
        return Err(ProtocolError::BadLength(report.len()));
    }
    if report[0] != LONG_REPORT_ID {
        return Err(ProtocolError::BadReportId(report[0]));
    }
    let mut r = [0u8; LONG_REPORT_LEN];
    r.copy_from_slice(report);
    Ok(r)
}

fn check_reply(report: &[u8], expected: u8) -> Result<Report, ProtocolError> {
    let r = check_frame(report)?;
    if r[1] != expected {
        return Err(ProtocolError::WrongCommand {
            expected,
            got: r[1],
        });
    }
    Ok(r)
}

fn check_long_reply(report: &[u8], expected: u8) -> Result<LongReport, ProtocolError> {
    let r = check_long_frame(report)?;
    if r[1] != expected {
        return Err(ProtocolError::WrongCommand {
            expected,
            got: r[1],
        });
    }
    Ok(r)
}

/// Reply to [`opcode::DEVICE_INFO`].
///
/// `51 06 01 00 34 34 35 d0 00 01 00 64 02` → VID 0x3434, PID 0xd035, battery
/// 0x64 = 100% (*verified*). Bytes 2, 3, 8, 9, 10 and 12 are *unknown*
/// (constant) and kept in `raw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub battery_percent: u8,
    pub raw: Report,
}

impl DeviceInfo {
    pub fn parse(report: &[u8]) -> Result<Self, ProtocolError> {
        let r = check_reply(report, opcode::DEVICE_INFO)?;
        Ok(DeviceInfo {
            vid: u16::from_le_bytes([r[4], r[5]]),
            pid: u16::from_le_bytes([r[6], r[7]]),
            battery_percent: r[11].min(100),
            raw: r,
        })
    }
}

/// Reply to [`opcode::FIRMWARE`]: `51 04 <len> <ascii…>` (*verified*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    pub version: String,
}

impl Firmware {
    pub fn parse(report: &[u8]) -> Result<Self, ProtocolError> {
        let r = check_reply(report, opcode::FIRMWARE)?;
        let len = (r[2] as usize).min(REPORT_LEN - 3);
        let text = &r[3..3 + len];
        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        Ok(Firmware {
            version: String::from_utf8_lossy(&text[..end]).into_owned(),
        })
    }
}

/// Reply to [`opcode::POINTER`] (PROTOCOL.md §4):
///
/// ```text
/// 51 07 PP S1 S2 S3 d1lo d1hi … d5lo d5hi FF NN DB
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerSettings {
    /// Byte 2, always 00 so far (*likely* a profile index).
    pub profile: u8,
    /// Three slot bytes, one per connection mode (*likely*); slot 0 is USB.
    /// Bits 0-3 active DPI stage, bits 4-5 report-rate index, bit 6 factory flag.
    pub slots: [u8; 3],
    pub dpi: Dpi,
    /// Raw sensor flag byte (`FF`).
    pub sensor_flags: u8,
    pub sensor: SensorSettings,
    /// `NN`, always 5.
    pub stage_count: u8,
    pub debounce: Debounce,
    pub raw: Report,
}

impl PointerSettings {
    const STAGES_AT: usize = 6;

    pub fn parse(report: &[u8]) -> Result<Self, ProtocolError> {
        let r = check_reply(report, opcode::POINTER)?;
        let mut stages = [0u16; Dpi::STAGE_COUNT];
        for (i, s) in stages.iter_mut().enumerate() {
            let at = Self::STAGES_AT + 2 * i;
            *s = u16::from_le_bytes([r[at], r[at + 1]]);
        }
        let after = Self::STAGES_AT + 2 * Dpi::STAGE_COUNT;
        let slots = [r[3], r[4], r[5]];
        Ok(PointerSettings {
            profile: r[2],
            slots,
            dpi: Dpi {
                stages,
                active: (slots[0] & 0x0f).min(Dpi::STAGE_COUNT as u8 - 1),
            },
            sensor_flags: r[after],
            sensor: SensorSettings::from_flags(r[after]),
            stage_count: r[after + 1],
            debounce: Debounce(r[after + 2]),
            raw: r,
        })
    }

    /// Report rate of the USB slot (*verified* for slot 0).
    pub fn report_rate(&self) -> Option<ReportRate> {
        ReportRate::from_index((self.slots[0] >> 4) & 0x03)
    }

    /// Bit 6 of the USB slot: set only before the first 0x40 write (*likely*).
    pub fn factory_flag(&self) -> bool {
        self.slots[0] & 0x40 != 0
    }
}

/// Lighting state assembled from the six reads 0x12-0x17 (*verified*).
/// Per-effect tables are indexed by `effect.id() - 1` (Static … Christmas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightingState {
    pub effect: Effect,
    pub brightness: [u8; 6],
    pub speed: [u8; 6],
    pub red: [u8; 6],
    pub green: [u8; 6],
    pub blue: [u8; 6],
    /// The 0x12 reply (bytes 2-3 and 5-12 are *unknown* constants).
    pub raw_effect: Report,
}

impl LightingState {
    pub const EFFECT_COUNT: usize = 6;

    /// Effect byte from a 0x12 reply.
    pub fn parse_effect(report: &[u8]) -> Result<(Effect, Report), ProtocolError> {
        let r = check_reply(report, opcode::LIGHT_EFFECT)?;
        let e = Effect::from_id(r[4]).ok_or(ProtocolError::UnknownEffect(r[4]))?;
        Ok((e, r))
    }

    /// Six per-effect bytes from a 0x13/0x14/0x15/0x16/0x17 reply.
    pub fn parse_table(report: &[u8], expected: u8) -> Result<[u8; 6], ProtocolError> {
        let r = check_reply(report, expected)?;
        let mut t = [0u8; 6];
        t.copy_from_slice(&r[2..8]);
        Ok(t)
    }

    pub fn assemble(
        effect_reply: &[u8],
        brightness: &[u8],
        speed: &[u8],
        red: &[u8],
        green: &[u8],
        blue: &[u8],
    ) -> Result<Self, ProtocolError> {
        let (effect, raw_effect) = Self::parse_effect(effect_reply)?;
        Ok(LightingState {
            effect,
            brightness: Self::parse_table(brightness, opcode::LIGHT_BRIGHTNESS)?,
            speed: Self::parse_table(speed, opcode::LIGHT_SPEED)?,
            red: Self::parse_table(red, opcode::LIGHT_RED)?,
            green: Self::parse_table(green, opcode::LIGHT_GREEN)?,
            blue: Self::parse_table(blue, opcode::LIGHT_BLUE)?,
            raw_effect,
        })
    }

    /// The stored parameters of one effect (for Off, the Static entry).
    pub fn for_effect(&self, effect: Effect) -> Lighting {
        let i = (effect.id().max(1) - 1) as usize;
        Lighting {
            effect,
            brightness: Brightness::new(self.brightness[i]),
            speed: Speed::new(self.speed[i]),
            color: Rgb::new(self.red[i], self.green[i], self.blue[i]),
        }
    }

    /// What the mouse is showing now.
    pub fn current(&self) -> Lighting {
        self.for_effect(self.effect)
    }

    /// Applies a write burst to the tables (used by the fake mouse).
    pub fn apply(&mut self, cmd: &Command) {
        let idx = |e: Effect| (e.id().max(1) - 1) as usize;
        match cmd {
            Command::SetEffect(e) => {
                self.effect = *e;
                self.raw_effect[4] = e.id();
            }
            Command::SetBrightness { effect, brightness } if *effect != Effect::Off => {
                self.brightness[idx(*effect)] = brightness.raw()
            }
            Command::SetSpeed { effect, speed } if *effect != Effect::Off => {
                self.speed[idx(*effect)] = speed.raw()
            }
            Command::SetColor { effect, color } if *effect != Effect::Off => {
                let i = idx(*effect);
                self.red[i] = color.r;
                self.green[i] = color.g;
                self.blue[i] = color.b;
            }
            _ => {}
        }
    }

    /// Factory lighting (*verified*: Rainbow, all ff, all 80, all red).
    pub fn factory() -> LightingState {
        let mut raw_effect = [0u8; REPORT_LEN];
        raw_effect[..13].copy_from_slice(&[
            0x51, 0x12, 0x01, 0x06, 0x03, 0xff, 0xff, 0xcf, 0xcf, 0xcd, 0xcd, 0xcf, 0xcd,
        ]);
        LightingState {
            effect: Effect::Rainbow,
            brightness: [0xff; 6],
            speed: [0x80; 6],
            red: [0xff; 6],
            green: [0; 6],
            blue: [0; 6],
            raw_effect,
        }
    }

    /// Replies the mouse would give for 0x12-0x17 (used by the fake mouse).
    pub fn replies(&self) -> Vec<Report> {
        let table = |op: u8, t: &[u8; 6]| {
            let mut r = [0u8; REPORT_LEN];
            r[0] = REPORT_ID;
            r[1] = op;
            r[2..8].copy_from_slice(t);
            r
        };
        vec![
            self.raw_effect,
            table(opcode::LIGHT_BRIGHTNESS, &self.brightness),
            table(opcode::LIGHT_SPEED, &self.speed),
            table(opcode::LIGHT_RED, &self.red),
            table(opcode::LIGHT_GREEN, &self.green),
            table(opcode::LIGHT_BLUE, &self.blue),
        ]
    }
}

/// Reply to `52 61`: which keys are remapped (*verified*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonFlags {
    pub remapped: [bool; Key::COUNT],
}

impl ButtonFlags {
    pub fn parse(report: &[u8]) -> Result<Self, ProtocolError> {
        let r = check_long_reply(report, opcode::BUTTON_FLAGS)?;
        let mut remapped = [false; Key::COUNT];
        for (i, f) in remapped.iter_mut().enumerate() {
            *f = r[2 + i] != 0;
        }
        Ok(ButtonFlags { remapped })
    }

    pub fn is_remapped(&self, key: Key) -> bool {
        self.remapped[key.index() as usize]
    }

    pub fn remapped_keys(&self) -> Vec<Key> {
        Key::ALL
            .iter()
            .copied()
            .filter(|k| self.is_remapped(*k))
            .collect()
    }

    pub fn encode(&self) -> LongReport {
        let mut r = [0u8; LONG_REPORT_LEN];
        r[0] = LONG_REPORT_ID;
        r[1] = opcode::BUTTON_FLAGS;
        for (i, f) in self.remapped.iter().enumerate() {
            r[2 + i] = u8::from(*f);
        }
        r
    }
}

/// Reply to `52 62 KK`: `52 62 KK 00 TT <payload>` (*verified* for Key 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonMapping {
    pub key: Key,
    pub action: ButtonAction,
}

impl ButtonMapping {
    pub fn parse(report: &[u8]) -> Result<Self, ProtocolError> {
        let r = check_long_reply(report, opcode::BUTTON_MAPPING)?;
        let key =
            Key::from_index(r[2]).ok_or(ProtocolError::BadArguments(opcode::BUTTON_MAPPING))?;
        Ok(ButtonMapping {
            key,
            action: ButtonAction::parse_payload(r[4], &r[5..]),
        })
    }

    pub fn encode(&self) -> LongReport {
        let mut r = Command::SetButton {
            key: self.key,
            action: self.action.clone(),
        }
        .encode();
        r[1] = opcode::BUTTON_MAPPING;
        let mut out = [0u8; LONG_REPORT_LEN];
        out.copy_from_slice(&r);
        out
    }
}

/// Renders a report as space-separated lowercase hex, the format used in
/// PROTOCOL.md and in the fake mouse's recordings.
pub fn hex(report: &[u8]) -> String {
    report
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses the output of [`hex`] (whitespace tolerant).
pub fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    s.split_whitespace()
        .map(|t| u8::from_str_radix(t, 16).map_err(|e| format!("bad hex byte '{t}': {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buttons::{ConsumerFunction, LoopMode, MacroEvent, MouseFunction};
    use crate::lighting::Lighting;
    use crate::pointer::LiftOff;

    fn report(s: &str) -> Vec<u8> {
        from_hex(s).unwrap()
    }

    /// Pads a hex prefix to a full report of the right length.
    fn padded(prefix: &str) -> String {
        let n = prefix.split_whitespace().count();
        let len = if prefix.starts_with("52") { 65 } else { 21 };
        format!("{prefix}{}", " 00".repeat(len - n))
    }

    // Exact byte strings from testdata/session1.pcap (see PROTOCOL.md).
    const STATIC_RED: [&str; 4] = [
        "51 22 01 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        "51 23 01 01 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        "51 27 01 01 80 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        "51 28 01 01 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
    ];

    #[test]
    fn encode_static_red_sequence() {
        let seq = Lighting::static_color(Rgb::RED).commands();
        assert_eq!(seq.len(), 4);
        for (cmd, expected) in seq.iter().zip(STATIC_RED) {
            assert_eq!(hex(&cmd.encode()), expected);
        }
    }

    #[test]
    fn encode_every_effect_select() {
        for (e, expected) in Effect::ALL.iter().zip([
            "51 22 01 00",
            "51 22 01 01",
            "51 22 01 02",
            "51 22 01 03",
            "51 22 01 04",
            "51 22 01 05",
            "51 22 01 06",
        ]) {
            let bytes = Command::SetEffect(*e).encode();
            assert_eq!(hex(&bytes[..4]), expected);
            assert!(bytes[4..].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn encode_brightness_and_speed_values_from_captures() {
        for (raw, expected) in [(0x0c, "0c"), (0x49, "49"), (0x85, "85"), (0xff, "ff")] {
            let r = Command::SetBrightness {
                effect: Effect::Static,
                brightness: Brightness::new(raw),
            }
            .encode();
            assert_eq!(hex(&r[..5]), format!("51 23 01 01 {expected}"));
        }
        // 0% in the app is 0x0c, never 0.
        let r = Command::SetBrightness {
            effect: Effect::Static,
            brightness: Brightness::new(0),
        }
        .encode();
        assert_eq!(r[4], 0x0c);
        // Session 2 speed slider: 14 / 4e / 89 / c3 / ff for breathing.
        for raw in [0x14u8, 0x4e, 0x89, 0xc3, 0xff] {
            let r = Command::SetSpeed {
                effect: Effect::Breathing,
                speed: Speed::new(raw),
            }
            .encode();
            assert_eq!(hex(&r[..5]), format!("51 27 01 02 {raw:02x}"));
        }
        assert_eq!(
            Speed::new(0x00).raw(),
            0x14,
            "speed floor is the app's minimum"
        );
    }

    #[test]
    fn encode_colors_from_capture() {
        let cases = [
            (Rgb::new(0x00, 0x0c, 0xff), "51 28 01 01 00 0c ff"),
            (Rgb::new(0x00, 0xff, 0x06), "51 28 01 01 00 ff 06"),
            (Rgb::new(0xff, 0xff, 0xff), "51 28 01 01 ff ff ff"),
            (Rgb::new(0x85, 0x79, 0x7d), "51 28 01 01 85 79 7d"),
            (Rgb::new(0x12, 0x34, 0x56), "51 28 01 01 12 34 56"),
            (Rgb::new(0xab, 0x34, 0x56), "51 28 01 01 ab 34 56"),
        ];
        for (color, expected) in cases {
            let r = Command::SetColor {
                effect: Effect::Static,
                color,
            }
            .encode();
            assert_eq!(hex(&r[..7]), expected);
        }
    }

    #[test]
    fn encode_breathing_snake_and_no_color_effects() {
        let breathing = Lighting {
            effect: Effect::Breathing,
            color: Rgb::RED,
            ..Default::default()
        }
        .commands();
        let expected = [
            "51 22 01 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
            "51 23 01 02 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
            "51 27 01 02 80 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
            "51 28 01 02 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        ];
        assert_eq!(
            breathing
                .iter()
                .map(|c| hex(&c.encode()))
                .collect::<Vec<_>>(),
            expected
        );
        let snake = Lighting {
            effect: Effect::Snake,
            color: Rgb::RED,
            ..Default::default()
        }
        .commands();
        assert_eq!(hex(&snake[3].encode()[..7]), "51 28 01 05 ff 00 00");
        for e in [Effect::Rainbow, Effect::RainbowSwirl, Effect::Christmas] {
            let seq = Lighting {
                effect: e,
                ..Default::default()
            }
            .commands();
            assert_eq!(seq.len(), 3, "{e:?} must not send a color");
            assert_eq!(
                seq.iter().map(|c| c.opcode()).collect::<Vec<_>>(),
                vec![0x22, 0x23, 0x27]
            );
        }
    }

    #[test]
    fn encode_lights_off_is_a_single_report() {
        let seq = Lighting {
            effect: Effect::Off,
            ..Default::default()
        }
        .commands();
        assert_eq!(seq.len(), 1);
        assert_eq!(
            hex(&seq[0].encode()),
            "51 22 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00"
        );
    }

    #[test]
    fn encode_queries() {
        for (op, expected) in [
            (opcode::DEVICE_INFO, "51 06"),
            (opcode::FIRMWARE, "51 04"),
            (opcode::POINTER, "51 07"),
            (opcode::LIGHT_EFFECT, "51 12"),
            (opcode::LIGHT_SPEED, "51 17"),
        ] {
            let r = Command::Query(op).encode();
            assert_eq!(hex(&r[..2]), expected);
            assert!(r[2..].iter().all(|&b| b == 0));
            assert_eq!(r.len(), 21);
        }
        let flags = Command::QueryLong {
            op: opcode::BUTTON_FLAGS,
            arg: 0,
        }
        .encode();
        assert_eq!(flags.len(), 65);
        assert_eq!(hex(&flags[..3]), "52 61 00");
        let one = Command::QueryLong {
            op: opcode::BUTTON_MAPPING,
            arg: 4,
        }
        .encode();
        assert_eq!(hex(&one[..4]), "52 62 04 00");
    }

    // ----- session 2 writes, byte for byte ---------------------------------

    #[test]
    fn encode_dpi_writes_from_session2() {
        let cases = [
            (
                Dpi {
                    stages: [450, 800, 1600, 3200, 5000],
                    active: 0,
                },
                "51 40 00 00 00 c2 01 20 03 40 06 80 0c 88 13 05",
            ),
            (
                Dpi {
                    stages: [450, 800, 1600, 4900, 5000],
                    active: 3,
                },
                "51 40 03 03 03 c2 01 20 03 40 06 24 13 88 13 05",
            ),
            (
                Dpi {
                    stages: [450, 800, 1600, 26000, 5000],
                    active: 3,
                },
                "51 40 03 03 03 c2 01 20 03 40 06 90 65 88 13 05",
            ),
            (
                Dpi {
                    stages: [450, 800, 1600, 100, 5000],
                    active: 3,
                },
                "51 40 03 03 03 c2 01 20 03 40 06 64 00 88 13 05",
            ),
            (
                Dpi::FACTORY,
                "51 40 02 02 02 90 01 20 03 40 06 80 0c 88 13 05",
            ),
            (
                Dpi {
                    stages: [400, 800, 1600, 3200, 1200],
                    active: 4,
                },
                "51 40 04 04 04 90 01 20 03 40 06 80 0c b0 04 05",
            ),
        ];
        for (dpi, want) in cases {
            dpi.validate().unwrap();
            let enc = Command::SetDpi(dpi).encode();
            assert_eq!(hex(&enc), padded(want));
            assert_eq!(Command::decode(&enc).unwrap(), Command::SetDpi(dpi));
        }
    }

    #[test]
    fn encode_rate_sensor_debounce_reset_from_session2() {
        for (rate, want) in [
            (ReportRate::Hz125, "51 41 00 00 00 7d 00 f4 01 e8 03"),
            (ReportRate::Hz500, "51 41 01 01 01 7d 00 f4 01 e8 03"),
            (ReportRate::Hz1000, "51 41 02 02 02 7d 00 f4 01 e8 03"),
        ] {
            let enc = Command::SetReportRate(rate).encode();
            assert_eq!(hex(&enc), padded(want));
            assert_eq!(Command::decode(&enc).unwrap(), Command::SetReportRate(rate));
        }
        // 23:17:13 lift-off 2 mm, everything else factory.
        let lift2 = SensorSettings {
            lift_off: LiftOff::Mm2,
            ..SensorSettings::FACTORY
        };
        assert_eq!(
            hex(&Command::SetSensor(lift2).encode()),
            padded("51 42 02 01 02 01 00 01")
        );
        // 23:17:27 angle snap on.
        let snap = SensorSettings {
            angle_snap: true,
            ..SensorSettings::FACTORY
        };
        assert_eq!(
            hex(&Command::SetSensor(snap).encode()),
            padded("51 42 01 01 01 01 00 01")
        );
        // 23:17:42 scroll reverse.
        let rev = SensorSettings {
            scroll_reverse: true,
            ..SensorSettings::FACTORY
        };
        assert_eq!(
            hex(&Command::SetSensor(rev).encode()),
            padded("51 42 01 01 02 01 00 02")
        );
        // 23:22:19 all second options.
        let all2 = SensorSettings {
            lift_off: LiftOff::Mm2,
            ripple: false,
            angle_snap: true,
            motion_sync: false,
            scroll_reverse: true,
        };
        let enc = Command::SetSensor(all2).encode();
        assert_eq!(hex(&enc), padded("51 42 02 02 01 02 00 02"));
        assert_eq!(Command::decode(&enc).unwrap(), Command::SetSensor(all2));

        for (ms, want) in [
            (9u8, "51 43 09"),
            (10, "51 43 0a"),
            (0x0b, "51 43 0b"),
            (8, "51 43 08"),
        ] {
            let enc = Command::SetDebounce(Debounce::new(ms).unwrap()).encode();
            assert_eq!(hex(&enc), padded(want));
            assert_eq!(
                Command::decode(&enc).unwrap(),
                Command::SetDebounce(Debounce(ms))
            );
        }
        for (kind, want) in [
            (ResetKind::Lighting, "51 0b 01 04"),
            (ResetKind::Buttons, "51 0b 01 01"),
            (ResetKind::Factory, "51 0b aa"),
        ] {
            let enc = Command::Reset(kind).encode();
            assert_eq!(hex(&enc), padded(want));
            assert_eq!(Command::decode(&enc).unwrap(), Command::Reset(kind));
        }
    }

    #[test]
    fn encode_button_writes_from_session2() {
        let cases: Vec<(Key, ButtonAction, &str)> = vec![
            (
                Key::Key4,
                ButtonAction::Mouse(MouseFunction::Right),
                "52 52 03 00 01 02",
            ),
            (
                Key::Key4,
                ButtonAction::Consumer(ConsumerFunction::Player),
                "52 52 03 00 03 83 01",
            ),
            (
                Key::Key4,
                ButtonAction::Lighting(crate::buttons::LightingFunction::Effect),
                "52 52 03 00 06 01",
            ),
            (
                Key::Key4,
                ButtonAction::Dpi(crate::buttons::DpiFunction::Loop),
                "52 52 03 00 05 01",
            ),
            (
                Key::Key4,
                ButtonAction::Consumer(ConsumerFunction::Stop),
                "52 52 03 00 03 b7",
            ),
            (
                Key::Key4,
                ButtonAction::System(crate::buttons::SystemShortcut::SwitchApp),
                "52 52 03 00 08 07 04 2b 00 00 01",
            ),
            (
                Key::Key4,
                ButtonAction::Mouse(MouseFunction::ScrollDown),
                "52 52 03 00 01 00 fe",
            ),
            (
                Key::Key4,
                ButtonAction::Mouse(MouseFunction::ScrollLeft),
                "52 52 03 00 01 00 00 fe",
            ),
            (Key::Key4, ButtonAction::Disabled, "52 52 03 00 09"),
            (
                Key::Key4,
                ButtonAction::Keyboard(crate::buttons::KeyCombo {
                    modifiers: 0x03,
                    usage: 0x0e,
                }),
                "52 52 03 00 02 03 0e",
            ),
            (
                Key::Key4,
                ButtonAction::AutoFire(crate::buttons::AutoFire {
                    button: MouseFunction::Left,
                    interval_ms: 37,
                    times: 5,
                }),
                "52 52 03 00 07 01 01 25 00 05",
            ),
            (Key::Key4, ButtonAction::Default, "52 52 03 00 00"),
            (
                Key::Key5,
                ButtonAction::Mouse(MouseFunction::Middle),
                "52 52 04 00 01 04",
            ),
        ];
        for (key, action, want) in cases {
            let cmd = Command::SetButton {
                key,
                action: action.clone(),
            };
            let enc = cmd.encode();
            assert_eq!(enc.len(), 65);
            assert_eq!(hex(&enc), padded(want), "{action}");
            assert_eq!(Command::decode(&enc).unwrap(), cmd);
        }
    }

    #[test]
    fn encode_macro_writes_from_session2() {
        let m = Macro {
            name: "Custom 1".into(),
            loop_mode: LoopMode::Times(3),
            fixed_delay_ms: 25,
            events: vec![
                MacroEvent::KeyDown(0x04),
                MacroEvent::Delay(47),
                MacroEvent::KeyUp(0x04),
                MacroEvent::Delay(3000),
                MacroEvent::KeyDown(0x05),
                MacroEvent::Delay(32),
                MacroEvent::KeyUp(0x05),
            ],
        };
        let events = Command::SetMacroEvents {
            key: Key::Key4,
            macro_: m.clone(),
        }
        .encode();
        assert_eq!(
            hex(&events),
            padded("52 54 03 00 22 01 03 00 19 00 07 00 81 04 00 00 0f 2f 00 00 01 04 00 00 0f b8 0b 00 81 05 00 00 0f 20 00 00 01 05 00 00")
        );
        let name = Command::SetMacroName {
            key: Key::Key4,
            name: m.name.clone(),
        }
        .encode();
        assert_eq!(hex(&name), padded("52 53 03 08 43 75 73 74 6f 6d 20 31"));
        match Command::decode(&events).unwrap() {
            Command::SetMacroEvents { key, macro_ } => {
                assert_eq!(key, Key::Key4);
                assert_eq!(macro_.events, m.events);
                assert_eq!(macro_.loop_mode, LoopMode::Times(3));
                assert_eq!(macro_.fixed_delay_ms, 25);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            Command::decode(&name).unwrap(),
            Command::SetMacroName {
                key: Key::Key4,
                name: "Custom 1".into()
            }
        );
    }

    #[test]
    fn decode_round_trips_and_rejects_garbage() {
        let all = [
            Command::Query(0x06),
            Command::SetEffect(Effect::Christmas),
            Command::SetBrightness {
                effect: Effect::Snake,
                brightness: Brightness::new(0x49),
            },
            Command::SetSpeed {
                effect: Effect::Rainbow,
                speed: Speed::new(0x80),
            },
            Command::SetColor {
                effect: Effect::Breathing,
                color: Rgb::new(1, 2, 3),
            },
        ];
        for c in all {
            assert_eq!(Command::decode(&c.encode()).unwrap(), c);
        }
        for s in STATIC_RED {
            let r = report(s);
            assert_eq!(Command::decode(&r).unwrap().encode(), r);
        }
        assert_eq!(
            Command::decode(&[0u8; 20]),
            Err(ProtocolError::BadReportId(0))
        );
        assert_eq!(
            Command::decode(&[0x51; 20]),
            Err(ProtocolError::BadLength(20))
        );
        assert_eq!(
            Command::decode(&report(&padded("51 22 01 09"))),
            Err(ProtocolError::UnknownEffect(9))
        );
        assert_eq!(
            Command::decode(&report(&padded("51 36 01 01"))),
            Err(ProtocolError::UnknownCommand(0x36))
        );
        assert_eq!(
            Command::decode(&report(&padded("51 46 09 03"))),
            Err(ProtocolError::UnknownCommand(0x46))
        );
        assert_eq!(
            Command::decode(&report(&padded("52 57 01 22"))),
            Err(ProtocolError::UnknownCommand(0x57))
        );
    }

    #[test]
    fn parse_device_info_reply() {
        let r = report("51 06 01 00 34 34 35 d0 00 01 00 64 02 00 00 00 00 00 00 00 00");
        let info = DeviceInfo::parse(&r).unwrap();
        assert_eq!(info.vid, 0x3434);
        assert_eq!(info.pid, 0xd035);
        assert_eq!(info.battery_percent, 100);
        assert_eq!(
            DeviceInfo::parse(&report(&padded("51 07"))),
            Err(ProtocolError::WrongCommand {
                expected: 0x06,
                got: 0x07
            })
        );
    }

    #[test]
    fn parse_firmware_reply() {
        let r = report("51 04 09 31 2e 30 2e 30 72 2d 35 00 00 00 00 00 00 00 00 00 00");
        assert_eq!(Firmware::parse(&r).unwrap().version, "1.0.0r-5");
        let r = report("51 04 ff 31 2e 30 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00");
        assert_eq!(Firmware::parse(&r).unwrap().version, "1.0");
    }

    #[test]
    fn parse_pointer_replies_from_both_sessions() {
        // Factory (session 1 / after 51 0b aa).
        let p = PointerSettings::parse(&report(
            "51 07 00 60 60 40 90 01 20 03 40 06 80 0c 88 13 35 05 08 00 00",
        ))
        .unwrap();
        assert_eq!(p.dpi.stages, [400, 800, 1600, 3200, 5000]);
        assert_eq!(p.dpi.active, 0);
        assert!(p.factory_flag());
        assert_eq!(p.report_rate(), Some(ReportRate::Hz1000)); // 0x60 >> 4 = 6 & 3 = 2
        assert_eq!(p.sensor, SensorSettings::FACTORY);
        assert_eq!(p.stage_count, 5);
        assert_eq!(p.debounce.ms(), 8);
        assert_eq!(p.profile, 0);
        // Baseline: slot 0x22 = stage 2 (1600) at 1000 Hz.
        let p = PointerSettings::parse(&report(
            "51 07 00 22 60 40 90 01 20 03 40 06 80 0c 88 13 35 05 08 00 00",
        ))
        .unwrap();
        assert_eq!(p.dpi.active, 2);
        assert_eq!(p.dpi.active_dpi(), 1600);
        assert_eq!(p.report_rate(), Some(ReportRate::Hz1000));
        assert!(!p.factory_flag());
        // Relaunch after 500 Hz + stage 3 + debounce 11 + flags 0x4a.
        let p = PointerSettings::parse(&report(
            "51 07 00 13 13 03 90 01 20 03 40 06 80 0c 88 13 4a 05 0b 00 00",
        ))
        .unwrap();
        assert_eq!(p.dpi.active, 3);
        assert_eq!(p.report_rate(), Some(ReportRate::Hz500));
        assert_eq!(p.debounce.ms(), 11);
        assert_eq!(p.sensor.lift_off, LiftOff::Mm2);
        assert!(
            p.sensor.angle_snap
                && !p.sensor.ripple
                && !p.sensor.motion_sync
                && p.sensor.scroll_reverse
        );
    }

    #[test]
    fn parse_lighting_state_from_session2() {
        // After breathing 12 34 56, brightness 0x89, speed 0x4e (PROTOCOL §4).
        let st = LightingState::assemble(
            &report(&padded("51 12 01 06 02 ff ff cf cf cd cd cf cd")),
            &report(&padded("51 13 ff 89 ff ff ff ff")),
            &report(&padded("51 17 80 4e 80 80 80 80")),
            &report(&padded("51 14 ff 12 ff ff ff ff")),
            &report(&padded("51 15 00 34 00 00 00 00")),
            &report(&padded("51 16 00 56 00 00 00 00")),
        )
        .unwrap();
        assert_eq!(st.effect, Effect::Breathing);
        let cur = st.current();
        assert_eq!(cur.color, Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(cur.brightness.raw(), 0x89);
        assert_eq!(cur.speed.raw(), 0x4e);
        let stat = st.for_effect(Effect::Static);
        assert_eq!(stat.color, Rgb::RED);
        assert_eq!(stat.speed.raw(), 0x80);
        // Replies round-trip.
        let again = {
            let r = st.replies();
            LightingState::assemble(&r[0], &r[1], &r[2], &r[3], &r[4], &r[5]).unwrap()
        };
        assert_eq!(again, st);
        // Apply the baseline's static green and check the tables.
        let mut f = LightingState::factory();
        for c in Lighting::static_color(Rgb::GREEN).commands() {
            f.apply(&c);
        }
        assert_eq!(f.effect, Effect::Static);
        assert_eq!(hex(&f.replies()[3][..8]), "51 14 00 ff ff ff ff ff");
        assert_eq!(hex(&f.replies()[4][..8]), "51 15 ff 00 00 00 00 00");
        assert_eq!(
            hex(&f.replies()[0][..13]),
            "51 12 01 06 01 ff ff cf cf cd cd cf cd"
        );
        assert!(LightingState::parse_effect(&report(&padded("51 12 01 06 09"))).is_err());
    }

    #[test]
    fn parse_button_replies_from_session2() {
        let none = ButtonFlags::parse(&report(&padded("52 61"))).unwrap();
        assert!(none.remapped_keys().is_empty());
        let one = ButtonFlags::parse(&report(&padded("52 61 00 00 00 00 01"))).unwrap();
        assert_eq!(one.remapped_keys(), vec![Key::Key5]);
        assert_eq!(hex(&one.encode()[..7]), "52 61 00 00 00 00 01");
        let m = ButtonMapping::parse(&report(&padded("52 62 04 00 01 04"))).unwrap();
        assert_eq!(m.key, Key::Key5);
        assert_eq!(m.action, ButtonAction::Mouse(MouseFunction::Middle));
        assert_eq!(hex(&m.encode()[..6]), "52 62 04 00 01 04");
        assert!(ButtonMapping::parse(&report(&padded("52 61"))).is_err());
        assert!(ButtonFlags::parse(&report(&padded("51 06"))).is_err());
    }

    #[test]
    fn hex_helpers() {
        let r = Command::Query(0x04).encode();
        assert_eq!(from_hex(&hex(&r)).unwrap(), r.to_vec());
        assert!(from_hex("zz").is_err());
    }
}
