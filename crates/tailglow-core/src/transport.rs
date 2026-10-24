// SPDX-License-Identifier: GPL-3.0-or-later
//! The transport abstraction: anything that can exchange HID feature reports.
//!
//! Implemented in `tailglow-hid` by the Linux hidraw backend, the hidapi
//! backend (Windows/macOS) and the `FakeMouse` used in tests.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("no supported mouse found (is it connected by USB cable?)")]
    NotFound,
    #[error("no permission to open {path}: install the udev rule (70-keychron-m1.rules) and replug the mouse")]
    PermissionDenied { path: String },
    #[error("the mouse went away ({path}): it was unplugged, switched off or re-enumerated; reconnect it and try again")]
    Disconnected { path: String },
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Unsupported(String),
    #[error("bad report: {0}")]
    BadReport(String),
}

impl TransportError {
    /// Maps an OS error on `path` to the error the user should see.
    /// `ENOENT` (node removed) and `ENODEV` (device gone while the node was
    /// open: what hidraw returns after an unplug mid-session) become
    /// [`TransportError::Disconnected`]. `EIO` is ambiguous (it is also what a
    /// half-unplugged USB device returns), so backends that can tell the node
    /// is gone use [`TransportError::io_after_check`].
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        let path = path.into();
        match source.kind() {
            std::io::ErrorKind::PermissionDenied => TransportError::PermissionDenied { path },
            std::io::ErrorKind::NotFound => TransportError::Disconnected { path },
            _ if source.raw_os_error() == Some(ENODEV) => TransportError::Disconnected { path },
            _ => TransportError::Io { path, source },
        }
    }

    /// Like [`TransportError::io`], plus `EIO` → `Disconnected` when the
    /// caller has checked that the device node no longer exists.
    pub fn io_after_check(
        path: impl Into<String>,
        source: std::io::Error,
        node_gone: bool,
    ) -> Self {
        if node_gone && source.raw_os_error() == Some(EIO) {
            return TransportError::Disconnected { path: path.into() };
        }
        TransportError::io(path, source)
    }

    /// True when the user can fix it by installing the udev rule.
    pub fn is_permission(&self) -> bool {
        matches!(self, TransportError::PermissionDenied { .. })
    }

    /// True when the mouse went away (unplug, power-off).
    pub fn is_disconnected(&self) -> bool {
        matches!(self, TransportError::Disconnected { .. })
    }
}

/// `ENODEV` and `EIO`: the same numbers on Linux, the BSDs/macOS and the
/// Windows CRT (`ENOENT` is covered by `ErrorKind::NotFound`).
const ENODEV: i32 = 19;
const EIO: i32 = 5;

/// A connection to one HID interface.
///
/// `report[0]` is always the report ID. Implementations must not retry or
/// reorder writes: the lighting sequence depends on order.
pub trait Transport: Send {
    /// SET_REPORT (feature). `report` is the full report, ID first.
    fn send_feature(&mut self, report: &[u8]) -> Result<(), TransportError>;

    /// GET_REPORT (feature) of `report_id`, returning `len` bytes, ID first.
    fn get_feature(&mut self, report_id: u8, len: usize) -> Result<Vec<u8>, TransportError>;

    /// Human-readable description (device path, "fake mouse", …).
    fn describe(&self) -> String;
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), TransportError> {
        (**self).send_feature(report)
    }
    fn get_feature(&mut self, report_id: u8, len: usize) -> Result<Vec<u8>, TransportError> {
        (**self).get_feature(report_id, len)
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_errors_map_to_friendly_kinds() {
        let e = |code| std::io::Error::from_raw_os_error(code);
        assert!(TransportError::io("/dev/hidraw9", e(2)).is_disconnected()); // ENOENT
        assert!(TransportError::io("/dev/hidraw9", e(ENODEV)).is_disconnected());
        assert!(TransportError::io("/dev/hidraw9", e(13)).is_permission()); // EACCES
        assert!(matches!(
            TransportError::io("/dev/hidraw9", e(EIO)),
            TransportError::Io { .. }
        ));
        assert!(TransportError::io_after_check("/dev/hidraw9", e(EIO), true).is_disconnected());
        assert!(!TransportError::io_after_check("/dev/hidraw9", e(EIO), false).is_disconnected());
        let msg = TransportError::io("/dev/hidraw9", e(ENODEV)).to_string();
        assert!(
            msg.contains("unplugged") && msg.contains("/dev/hidraw9"),
            "{msg}"
        );
    }
}
