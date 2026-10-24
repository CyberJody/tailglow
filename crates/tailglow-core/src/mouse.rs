// SPDX-License-Identifier: GPL-3.0-or-later
//! `Mouse`: a session with one mouse over any [`Transport`].
//!
//! Reads never need an opt-in. Writes go through [`Mouse::check_writable`],
//! which refuses unknown VID:PIDs, refuses unsupported families, and refuses
//! *likely* (untested) models unless `allow_untested` was set.

use std::collections::BTreeMap;

use crate::buttons::{ButtonAction, Key, Macro, MouseFunction};
use crate::device::{self, DeviceId, Model, SupportStatus};
use crate::lighting::Lighting;
use crate::pointer::{Debounce, Dpi, ReportRate, ResetKind, SensorSettings};
use crate::protocol::{
    self, opcode, ButtonFlags, ButtonMapping, Command, DeviceInfo, Firmware, LightingState,
    LongReport, PointerSettings, Report,
};
use crate::transport::{Transport, TransportError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("protocol: {0}")]
    Protocol(#[from] protocol::ProtocolError),
    #[error("unsupported device {id}: not in the device table")]
    UnknownDevice { id: DeviceId },
    #[error("{model} does not support {what}")]
    Unsupported { model: &'static str, what: String },
    #[error(
        "{model} ({id}) is listed as likely-compatible but was never tested; pass --allow-untested (CLI) or tick the box in the GUI to write to it, and please report the result"
    )]
    Untested { model: &'static str, id: DeviceId },
    #[error("{model} ({id}) uses a different protocol; Tailglow will not write to it")]
    WrongFamily { model: &'static str, id: DeviceId },
    #[error("{0}; pass --allow-untested (CLI) or tick \"Allow untested values\" on the GUI's Settings page to send it anyway")]
    UntestedValue(String),
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("refusing: {0}")]
    Refused(String),
}

/// Identity read at connect time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub device: DeviceInfo,
    pub firmware: Firmware,
    pub pointer: PointerSettings,
    pub model: Option<&'static Model>,
}

impl Info {
    pub fn id(&self) -> DeviceId {
        DeviceId::new(self.device.vid, self.device.pid)
    }

    pub fn model_name(&self) -> &'static str {
        self.model
            .map(|m| m.name)
            .unwrap_or("Unknown Keychron mouse")
    }

    pub fn status(&self) -> Option<SupportStatus> {
        self.model.map(|m| m.status)
    }
}

/// The current button map: every key with its action (keys the mouse flags
/// as not remapped carry their factory action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonMap {
    pub flags: ButtonFlags,
    pub actions: BTreeMap<Key, ButtonAction>,
}

impl ButtonMap {
    pub fn action(&self, key: Key) -> ButtonAction {
        match self.actions.get(&key) {
            Some(ButtonAction::Default) | None => key.factory_action(),
            Some(a) => a.clone(),
        }
    }

    /// Keys whose effective action is Left-Click.
    pub fn left_click_keys(&self) -> Vec<Key> {
        Key::ALL
            .iter()
            .copied()
            .filter(|k| self.action(*k).is_left_click())
            .collect()
    }
}

/// Everything readable from the mouse, in one struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub info: Info,
    pub lighting: LightingState,
    pub buttons: ButtonMap,
}

pub struct Mouse<T: Transport> {
    transport: T,
    model: Option<&'static Model>,
    allow_untested: bool,
}

impl<T: Transport> Mouse<T> {
    /// Wraps a transport. Call [`Mouse::read_info`] to identify the model;
    /// until then writes are refused.
    pub fn new(transport: T) -> Self {
        Mouse {
            transport,
            model: None,
            allow_untested: false,
        }
    }

    /// Wraps a transport whose model is already known (from enumeration).
    pub fn with_model(transport: T, model: &'static Model) -> Self {
        Mouse {
            transport,
            model: Some(model),
            allow_untested: false,
        }
    }

    /// Allow writes to *likely* (untested) models and values that were
    /// never captured (debounce outside 8-11 ms).
    pub fn allow_untested(&mut self, allow: bool) {
        self.allow_untested = allow;
    }

    pub fn allows_untested(&self) -> bool {
        self.allow_untested
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    pub fn model(&self) -> Option<&'static Model> {
        self.model
    }

    pub fn describe(&self) -> String {
        self.transport.describe()
    }

    // ----- raw I/O ----------------------------------------------------------

    /// Sends one command, no reply, after the write gate.
    pub fn send(&mut self, cmd: &Command) -> Result<(), Error> {
        if !cmd.is_query() {
            self.check_writable()?;
        }
        self.transport.send_feature(&cmd.encode())?;
        Ok(())
    }

    /// Arms and reads a short query: `51 <op> 00…` then GET_REPORT.
    pub fn query(&mut self, op: u8) -> Result<Report, Error> {
        self.send(&Command::Query(op))?;
        let reply = self
            .transport
            .get_feature(protocol::REPORT_ID, protocol::REPORT_LEN)?;
        Ok(protocol::check_frame(&reply)?)
    }

    /// Arms and reads a long query: `52 <op> <arg> 00…` then GET_REPORT.
    pub fn query_long(&mut self, op: u8, arg: u8) -> Result<LongReport, Error> {
        self.send(&Command::QueryLong { op, arg })?;
        let reply = self
            .transport
            .get_feature(protocol::LONG_REPORT_ID, protocol::LONG_REPORT_LEN)?;
        Ok(protocol::check_long_frame(&reply)?)
    }

    // ----- reads ------------------------------------------------------------

    pub fn device_info(&mut self) -> Result<DeviceInfo, Error> {
        let r = self.query(opcode::DEVICE_INFO)?;
        Ok(DeviceInfo::parse(&r)?)
    }

    pub fn firmware(&mut self) -> Result<Firmware, Error> {
        let r = self.query(opcode::FIRMWARE)?;
        Ok(Firmware::parse(&r)?)
    }

    pub fn pointer(&mut self) -> Result<PointerSettings, Error> {
        let r = self.query(opcode::POINTER)?;
        Ok(PointerSettings::parse(&r)?)
    }

    /// The six lighting reads (0x12, 0x13, 0x17, 0x14, 0x15, 0x16 in the
    /// app's order).
    pub fn lighting(&mut self) -> Result<LightingState, Error> {
        let e = self.query(opcode::LIGHT_EFFECT)?;
        let b = self.query(opcode::LIGHT_BRIGHTNESS)?;
        let s = self.query(opcode::LIGHT_SPEED)?;
        let r = self.query(opcode::LIGHT_RED)?;
        let g = self.query(opcode::LIGHT_GREEN)?;
        let bl = self.query(opcode::LIGHT_BLUE)?;
        Ok(LightingState::assemble(&e, &b, &s, &r, &g, &bl)?)
    }

    /// `52 61`, then `52 62 KK` for every flagged key (the app's sequence).
    pub fn buttons(&mut self) -> Result<ButtonMap, Error> {
        let flags = ButtonFlags::parse(&self.query_long(opcode::BUTTON_FLAGS, 0)?)?;
        let mut actions = BTreeMap::new();
        for key in flags.remapped_keys() {
            let m = ButtonMapping::parse(&self.query_long(opcode::BUTTON_MAPPING, key.index())?)?;
            actions.insert(key, m.action);
        }
        Ok(ButtonMap { flags, actions })
    }

    /// Reads identity, firmware and pointer settings and remembers the model.
    pub fn read_info(&mut self) -> Result<Info, Error> {
        let device = self.device_info()?;
        let firmware = self.firmware()?;
        let pointer = self.pointer()?;
        let model = device::lookup(DeviceId::new(device.vid, device.pid));
        if model.is_some() {
            self.model = model;
        }
        Ok(Info {
            device,
            firmware,
            pointer,
            model,
        })
    }

    /// Everything the vendor app reads at launch.
    pub fn snapshot(&mut self) -> Result<Snapshot, Error> {
        let info = self.read_info()?;
        let lighting = self.lighting()?;
        let buttons = self.buttons()?;
        Ok(Snapshot {
            info,
            lighting,
            buttons,
        })
    }

    // ----- write gate -------------------------------------------------------

    /// Refuses writes to unknown, unsupported or (without opt-in) untested models.
    pub fn check_writable(&self) -> Result<&'static Model, Error> {
        let model = self.model.ok_or_else(|| {
            Error::Refused(
                "the mouse has not been identified yet (read_info first); refusing to write".into(),
            )
        })?;
        match model.status {
            SupportStatus::Verified => Ok(model),
            SupportStatus::Likely if self.allow_untested => Ok(model),
            SupportStatus::Likely => Err(Error::Untested {
                model: model.name,
                id: model.id,
            }),
            SupportStatus::Unsupported => Err(Error::WrongFamily {
                model: model.name,
                id: model.id,
            }),
        }
    }

    // ----- writes -----------------------------------------------------------

    /// Validates `lighting` against the model's capabilities and sends the
    /// vendor app's write sequence (0x22 → 0x23 → 0x27 → 0x28).
    pub fn apply_lighting(&mut self, lighting: &Lighting) -> Result<(), Error> {
        let model = self.check_writable()?;
        let caps = model
            .caps
            .lighting
            .as_ref()
            .ok_or_else(|| Error::Unsupported {
                model: model.name,
                what: "lighting".into(),
            })?;
        if !caps.supports(lighting.effect) {
            return Err(Error::Unsupported {
                model: model.name,
                what: format!("effect {}", lighting.effect.name()),
            });
        }
        if !caps.brightness.contains(&lighting.brightness.raw()) {
            return Err(Error::Invalid(format!(
                "brightness 0x{:02x}",
                lighting.brightness.raw()
            )));
        }
        if !caps.speed.contains(&lighting.speed.raw()) {
            return Err(Error::Invalid(format!(
                "speed 0x{:02x}",
                lighting.speed.raw()
            )));
        }
        for cmd in lighting.commands() {
            self.send(&cmd)?;
        }
        Ok(())
    }

    fn check_pointer(&self) -> Result<&'static Model, Error> {
        let model = self.check_writable()?;
        if !model.caps.pointer {
            return Err(Error::Unsupported {
                model: model.name,
                what: "pointer settings".into(),
            });
        }
        Ok(model)
    }

    fn check_buttons(&self) -> Result<&'static Model, Error> {
        let model = self.check_writable()?;
        if !model.caps.buttons {
            return Err(Error::Unsupported {
                model: model.name,
                what: "button mapping".into(),
            });
        }
        Ok(model)
    }

    /// `51 40`: all five stages and the active index, as the app's Confirm does.
    pub fn apply_dpi(&mut self, dpi: &Dpi) -> Result<(), Error> {
        self.check_pointer()?;
        dpi.validate().map_err(Error::Invalid)?;
        self.send(&Command::SetDpi(*dpi))
    }

    pub fn apply_report_rate(&mut self, rate: ReportRate) -> Result<(), Error> {
        self.check_pointer()?;
        self.send(&Command::SetReportRate(rate))
    }

    pub fn apply_sensor(&mut self, s: &SensorSettings) -> Result<(), Error> {
        self.check_pointer()?;
        self.send(&Command::SetSensor(*s))
    }

    /// `51 43 MS`. 8-11 ms were captured; 1-30 only with `allow_untested`.
    pub fn apply_debounce(&mut self, d: Debounce) -> Result<(), Error> {
        self.check_pointer()?;
        Debounce::new(d.ms()).map_err(Error::Invalid)?;
        if !d.is_observed() && !self.allow_untested {
            return Err(Error::UntestedValue(d.untested_warning()));
        }
        self.send(&Command::SetDebounce(d))
    }

    /// `51 0b …`. Callers must confirm with the user first.
    pub fn reset(&mut self, kind: ResetKind) -> Result<(), Error> {
        self.check_writable()?;
        self.send(&Command::Reset(kind))
    }

    /// `52 52 KK 00 TT …`. Refuses to remove the last Left-Click: `current`
    /// is the map read before the change; without it only the factory layout
    /// is assumed (so Key 1 is protected).
    pub fn apply_button(
        &mut self,
        key: Key,
        action: &ButtonAction,
        current: Option<&ButtonMap>,
    ) -> Result<(), Error> {
        self.check_buttons()?;
        if let ButtonAction::Raw { .. } = action {
            return Err(Error::Refused(
                "raw button payloads were never captured from the vendor app".into(),
            ));
        }
        if let ButtonAction::AutoFire(a) = action {
            if !matches!(
                a.button,
                MouseFunction::Left
                    | MouseFunction::Right
                    | MouseFunction::Middle
                    | MouseFunction::Forward
                    | MouseFunction::Backward
            ) {
                return Err(Error::Invalid("auto-fire needs a clickable button".into()));
            }
        }
        self.guard_left_click(key, action, current)?;
        self.send(&Command::SetButton {
            key,
            action: action.clone(),
        })
    }

    /// Assigns a macro: `52 54` (events) then `52 53` (name), the app's order.
    pub fn apply_macro(
        &mut self,
        key: Key,
        macro_: &Macro,
        current: Option<&ButtonMap>,
    ) -> Result<(), Error> {
        self.check_buttons()?;
        macro_.validate().map_err(Error::Invalid)?;
        // A macro is never a left click.
        self.guard_left_click(key, &ButtonAction::Disabled, current)?;
        self.send(&Command::SetMacroEvents {
            key,
            macro_: macro_.clone(),
        })?;
        self.send(&Command::SetMacroName {
            key,
            name: macro_.name.clone(),
        })
    }

    fn guard_left_click(
        &self,
        key: Key,
        action: &ButtonAction,
        current: Option<&ButtonMap>,
    ) -> Result<(), Error> {
        let keeps_left = action.is_left_click()
            || (*action == ButtonAction::Default && key.factory_action().is_left_click());
        if keeps_left {
            return Ok(());
        }
        let others_with_left = match current {
            Some(map) => map
                .left_click_keys()
                .into_iter()
                .filter(|k| *k != key)
                .count(),
            // Without a map we can only reason about the factory layout.
            None => usize::from(key != Key::Key1),
        };
        if others_with_left == 0 {
            return Err(Error::Refused(format!(
                "{} is the only Left-Click; map another key to left-click first (the vendor app locks Key 1 for the same reason)",
                key.label()
            )));
        }
        Ok(())
    }
}
