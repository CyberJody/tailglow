// SPDX-License-Identifier: GPL-3.0-or-later
//! `tailglow-hid`: transports for `tailglow-core`.
//!
//! - Linux: our own hidraw backend ([`linux`]) — sysfs scan plus the
//!   `HIDIOCSFEATURE`/`HIDIOCGFEATURE` ioctls. No libudev, no hidapi.
//! - Windows/macOS: the `hidapi` crate ([`hidapi_backend`]), compiled only
//!   off Linux. It builds in CI but was never run against a mouse.
//! - [`FakeMouse`]: a transport that answers queries with replies recorded in
//!   a usbmon capture and records every write, for tests and demos.
//!
//! - [`bluez`]: Bluetooth status (connection, battery) read from BlueZ over
//!   D-Bus. Read-only: the M1 has no settings channel over Bluetooth.
//! - [`paths`]: the config directory.
//!
//! [`enumerate`], [`open_default`], [`open_path`] and [`open_from_env`] are
//! the entry points the CLI and GUI use. Every real device is identified from
//! OS metadata (Linux: sysfs `HID_ID` + the parent `bInterfaceNumber`; elsewhere
//! hidapi's device info) **before** it is opened, so no report — not even a
//! read query, which is a SET_REPORT too — reaches a node that is not the
//! vendor interface of a known, readable model ([`vet`]).

pub mod bluez;
pub mod capture;
pub mod fake;
pub mod paths;

#[cfg(not(target_os = "linux"))]
pub mod hidapi_backend;
#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(not(target_os = "linux"))]
use hidapi_backend as backend;
#[cfg(target_os = "linux")]
use linux as backend;

pub use capture::Capture;
pub use fake::FakeMouse;
use tailglow_core::{device, DeviceId, Model, Transport, TransportError};

/// Environment variable selecting the fake transport. Values:
/// `1`/`session1` (built-in session 1 replies, factory state), `session2`
/// (session 2 baseline: static green, stage 3), a path to a usbmon `.pcap`,
/// `none` (simulate "no mouse"), `noperm` (simulate a permission error).
pub const FAKE_ENV: &str = "TAILGLOW_FAKE";
/// If set with the fake transport, every write is appended to this file as a
/// hex line (`51 22 01 01 00 …`), one report per line.
pub const FAKE_RECORD_ENV: &str = "TAILGLOW_FAKE_RECORD";

/// One HID interface found on the system that belongs to a known model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundDevice {
    /// OS path (`/dev/hidrawN` on Linux, hidapi path elsewhere).
    pub path: String,
    pub id: DeviceId,
    /// USB `bInterfaceNumber`, if the OS exposes it.
    pub interface: Option<u8>,
    pub product: String,
    pub model: &'static Model,
}

impl FoundDevice {
    /// True for the interface that carries the vendor feature reports.
    pub fn is_vendor_interface(&self) -> bool {
        self.interface == Some(self.model.vendor_interface)
    }
}

/// Lists every HID interface of every known model (all interfaces, so `list`
/// can show the mouse even when the vendor one is missing or unreadable).
pub fn enumerate() -> Result<Vec<FoundDevice>, TransportError> {
    backend::enumerate()
}

/// What the OS says about one HID node, before the device table is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIdentity {
    pub path: String,
    pub id: DeviceId,
    /// USB `bInterfaceNumber`; `None` for Bluetooth/virtual devices or when
    /// the OS does not report it (macOS).
    pub interface: Option<u8>,
    pub product: String,
}

/// Decides whether tailglow may talk to a node at all. Refuses unknown
/// VID:PIDs, unsupported families, nodes without a USB interface number and
/// every interface except the model's vendor one. Pure, so it is tested with
/// injected identities; the backends feed it real OS data.
pub fn vet(node: NodeIdentity) -> Result<FoundDevice, TransportError> {
    let NodeIdentity {
        path,
        id,
        interface,
        product,
    } = node;
    let Some(model) = device::lookup(id) else {
        return Err(TransportError::Unsupported(format!(
            "{path} is {id} ({}), which is not a mouse Tailglow knows; refusing to open it",
            if product.is_empty() {
                "no name"
            } else {
                &product
            }
        )));
    };
    if !model.readable() {
        return Err(TransportError::Unsupported(format!(
            "{path} is a {} ({id}), listed as unsupported ({}); refusing to open it",
            model.name, model.evidence
        )));
    }
    match interface {
        Some(i) if i == model.vendor_interface => Ok(FoundDevice {
            path,
            id,
            interface,
            product,
            model,
        }),
        Some(i) => Err(TransportError::Unsupported(format!(
            "{path} is USB interface {i} of the {} (the pointer/keyboard side); its settings live on interface {} — run `tailglow list` to find that node",
            model.name, model.vendor_interface
        ))),
        None => Err(TransportError::Unsupported(format!(
            "{path} is a {} ({id}) without a USB interface number (Bluetooth or an OS that hides it); only the wired vendor interface was tested, so Tailglow will not open it",
            model.name
        ))),
    }
}

/// Opens a specific path (`--device`). The node is identified and vetted
/// first ([`vet`]); nothing is sent to a node that fails.
pub fn open_path(path: &str) -> Result<(Box<dyn Transport>, &'static Model), TransportError> {
    let found = vet(backend::identify(path)?)?;
    Ok((backend::open_vetted(&found.path)?, found.model))
}

/// Opens the vendor interface of the first known mouse whose model is
/// readable (verified or likely). Unsupported families are never opened.
pub fn open_default() -> Result<(Box<dyn Transport>, &'static Model), TransportError> {
    let found = enumerate()?;
    if let Some(dev) = found
        .iter()
        .find(|d| d.is_vendor_interface() && d.model.readable())
    {
        return open_path(&dev.path);
    }
    if let Some(dev) = found.iter().find(|d| !d.model.readable()) {
        return Err(TransportError::Unsupported(format!(
            "{} ({}) is listed as unsupported: {}",
            dev.model.name, dev.id, dev.model.evidence
        )));
    }
    Err(TransportError::NotFound)
}

/// Honors [`FAKE_ENV`]: `Some(..)` when a fake was requested, `None` to use
/// real hardware. `fake` forces the built-in session-1 fake even when the
/// variable is unset (for `--fake` flags).
pub fn open_from_env(force_fake: bool) -> Option<Result<Box<dyn Transport>, TransportError>> {
    let value = std::env::var(FAKE_ENV).ok();
    let value = match (value, force_fake) {
        (Some(v), _) if !v.is_empty() => v,
        (_, true) => "session1".to_string(),
        _ => return None,
    };
    let record = std::env::var(FAKE_RECORD_ENV)
        .ok()
        .filter(|s| !s.is_empty());
    let built = match value.as_str() {
        "none" | "missing" => Err(TransportError::NotFound),
        "noperm" => Err(TransportError::PermissionDenied {
            path: "/dev/hidraw9 (simulated)".into(),
        }),
        "1" | "true" | "session1" => Ok(FakeMouse::session1()),
        "2" | "session2" | "baseline" => Ok(FakeMouse::session2()),
        path => FakeMouse::from_pcap(path).map_err(|e| TransportError::Unsupported(e.to_string())),
    };
    Some(built.map(|mut fake| {
        if let Some(path) = record {
            fake = fake.record_to(path);
        }
        Box::new(fake) as Box<dyn Transport>
    }))
}

/// Opens whatever the environment and flags ask for: the fake when requested,
/// otherwise `--device PATH` or the first known mouse.
pub fn open(
    force_fake: bool,
    device_path: Option<&str>,
) -> Result<Box<dyn Transport>, TransportError> {
    if let Some(fake) = open_from_env(force_fake) {
        return fake;
    }
    match device_path {
        Some(p) => open_path(p).map(|(t, _)| t),
        None => open_default().map(|(t, _)| t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(pid: u16, interface: Option<u8>) -> NodeIdentity {
        NodeIdentity {
            path: "/dev/hidraw5".into(),
            id: DeviceId::new(0x3434, pid),
            interface,
            product: "test".into(),
        }
    }

    #[test]
    fn vet_accepts_only_the_vendor_interface_of_readable_models() {
        let ok = vet(node(0xd035, Some(2))).unwrap();
        assert_eq!(ok.model.name, "Keychron M1");
        // Likely models open for reads (writes still need the opt-in).
        assert!(vet(node(0xd032, Some(2))).is_ok());
        let msg = |n| vet(n).unwrap_err().to_string();
        assert!(msg(node(0xd035, Some(0))).contains("interface 0"));
        assert!(msg(node(0xd035, Some(1))).contains("interface 2"));
        assert!(msg(node(0xd035, None)).contains("without a USB interface"));
        assert!(msg(node(0xd03c, Some(3))).contains("unsupported"));
        assert!(msg(node(0xd050, Some(4))).contains("unsupported"));
        let logi = NodeIdentity {
            id: DeviceId::new(0x046d, 0xc08b),
            ..node(0, Some(2))
        };
        assert!(msg(logi).contains("not a mouse Tailglow knows"));
    }

    #[test]
    fn open_path_refuses_non_hidraw_nodes_before_opening() {
        // /dev/null is a character device but not a hidraw node: refused at
        // identification, so it is never opened, let alone written.
        let err = open_path("/dev/null").map(|_| ()).unwrap_err();
        assert!(matches!(err, TransportError::Unsupported(_)), "{err}");
        let err = open_path("/dev/hidraw-does-not-exist")
            .map(|_| ())
            .unwrap_err();
        assert!(!matches!(err, TransportError::Io { .. }), "{err}");
    }
}
