// SPDX-License-Identifier: GPL-3.0-or-later
//! `tailglow-core`: protocol, device model and settings for Keychron mice.
//!
//! This crate does no I/O. It turns typed commands into the HID feature
//! reports the mouse understands (21-byte `0x51`, 65-byte `0x52`), parses
//! replies, and describes what each supported model can do. Everything here
//! comes from our own USB captures of the vendor app (see `PROTOCOL.md` in
//! the project root).
//!
//! Layering:
//! - [`protocol`]: bytes on the wire ([`Command`], reply parsers).
//! - [`lighting`]: typed lighting values ([`Effect`], [`Rgb`], [`Brightness`],
//!   [`Speed`], [`Lighting`]).
//! - [`pointer`]: DPI, report rate, sensor options, debounce, resets.
//! - [`buttons`]: keys, button actions, macros.
//! - [`device`]: the device table ([`Model`], [`Capabilities`]).
//! - [`transport`]: the [`Transport`] trait implemented by `tailglow-hid`.
//! - [`mouse`]: [`Mouse`], the high-level session over any transport.
//! - [`settings`]: versioned, serde-backed export/import ([`Settings`]).
//! - [`apply`]: [`Mouse::apply_settings`], diff + write + read-back compare.

pub mod apply;
pub mod buttons;
pub mod device;
pub mod lighting;
pub mod mouse;
pub mod pointer;
pub mod protocol;
pub mod settings;
pub mod transport;

pub use apply::{ApplyOptions, ApplyOutcome};
pub use buttons::{ButtonAction, Key, Macro, MacroEvent};
pub use device::{Capabilities, DeviceId, LightingCaps, Model, SupportStatus, MODELS};
pub use lighting::{Brightness, Effect, Lighting, Rgb, Speed};
pub use mouse::{ButtonMap, Error, Info, Mouse, Snapshot};
pub use pointer::{Debounce, Dpi, ReportRate, ResetKind, SensorSettings};
pub use protocol::{
    Command, DeviceInfo, Firmware, LightingState, PointerSettings, ProtocolError, Report,
};
pub use settings::{macro_fingerprint, PointerConfig, Settings, SettingsError};
pub use transport::{Transport, TransportError};
