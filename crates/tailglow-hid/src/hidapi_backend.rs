// SPDX-License-Identifier: GPL-3.0-or-later
//! hidapi transport for Windows and macOS. Compiled only off Linux (Linux has
//! its own hidraw backend so we don't need libudev). **Untested**: written
//! against the hidapi 2.x API and built in CI, but never run against a mouse.
//!
//! Identification uses hidapi's own device info (VID, PID, interface number)
//! before a node is opened; [`crate::vet`] then refuses anything that is not
//! the vendor interface of a known model. macOS: IOKit may report
//! `interface_number() == -1`; such nodes are refused rather than probed,
//! because probing means sending feature reports to unknown collections.
//! Supporting macOS needs a real run on a Mac first.

use std::ffi::CString;

use hidapi::{HidApi, HidDevice};
use tailglow_core::{device, DeviceId, Transport, TransportError};

use crate::{FoundDevice, NodeIdentity};

fn api() -> Result<HidApi, TransportError> {
    HidApi::new().map_err(|e| TransportError::Unsupported(format!("hidapi: {e}")))
}

pub fn enumerate() -> Result<Vec<FoundDevice>, TransportError> {
    let api = api()?;
    let mut out = Vec::new();
    for info in api.device_list() {
        let id = DeviceId::new(info.vendor_id(), info.product_id());
        let Some(model) = device::lookup(id) else {
            continue;
        };
        let interface = u8::try_from(info.interface_number()).ok();
        out.push(FoundDevice {
            path: info.path().to_string_lossy().into_owned(),
            id,
            interface,
            product: info.product_string().unwrap_or("").to_string(),
            model,
        });
    }
    Ok(out)
}

/// Looks `path` up in hidapi's device list (no open, no report).
pub fn identify(path: &str) -> Result<NodeIdentity, TransportError> {
    let api = api()?;
    let info = api
        .device_list()
        .find(|i| i.path().to_string_lossy() == path)
        .ok_or_else(|| {
            TransportError::Unsupported(format!(
                "{path} is not a HID device hidapi knows; refusing to open it (run `tailglow list`)"
            ))
        })?;
    Ok(NodeIdentity {
        path: path.to_string(),
        id: DeviceId::new(info.vendor_id(), info.product_id()),
        interface: u8::try_from(info.interface_number()).ok(),
        product: info.product_string().unwrap_or("").to_string(),
    })
}

/// Opens a node that already passed [`crate::vet`].
pub(crate) fn open_vetted(path: &str) -> Result<Box<dyn Transport>, TransportError> {
    let api = api()?;
    let cpath = CString::new(path).map_err(|e| TransportError::Unsupported(e.to_string()))?;
    let dev = api.open_path(&cpath).map_err(|e| TransportError::Io {
        path: path.to_string(),
        source: std::io::Error::other(e.to_string()),
    })?;
    Ok(Box::new(HidapiDevice {
        dev,
        path: path.to_string(),
    }))
}

pub struct HidapiDevice {
    dev: HidDevice,
    path: String,
}

impl Transport for HidapiDevice {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), TransportError> {
        self.dev
            .send_feature_report(report)
            .map_err(|e| TransportError::Io {
                path: self.path.clone(),
                source: std::io::Error::other(e.to_string()),
            })
    }

    fn get_feature(&mut self, report_id: u8, len: usize) -> Result<Vec<u8>, TransportError> {
        let mut buf = vec![0u8; len];
        buf[0] = report_id;
        let n = self
            .dev
            .get_feature_report(&mut buf)
            .map_err(|e| TransportError::Io {
                path: self.path.clone(),
                source: std::io::Error::other(e.to_string()),
            })?;
        buf.truncate(n.min(len));
        Ok(buf)
    }

    fn describe(&self) -> String {
        self.path.clone()
    }
}
