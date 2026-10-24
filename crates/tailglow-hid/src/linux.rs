// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux hidraw transport without libudev or hidapi.
//!
//! Discovery walks `/sys/class/hidraw/hidraw*/device`: the `uevent` file gives
//! `HID_ID=0003:0000VVVV:0000PPPP` and `HID_NAME`, and for USB devices the
//! parent directory is the interface (`…/1-1:1.2`) whose `bInterfaceNumber`
//! file tells us which interface the node belongs to. I/O uses the
//! `HIDIOCSFEATURE(len)` / `HIDIOCGFEATURE(len)` ioctls, exactly like the
//! Python test script used during the capture sessions.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use tailglow_core::{device, DeviceId, Transport, TransportError};

use crate::{FoundDevice, NodeIdentity};

const SYSFS_HIDRAW: &str = "/sys/class/hidraw";

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

/// `_IOC(_IOC_READ|_IOC_WRITE, 'H', nr, len)` from `<linux/hidraw.h>`.
const fn hid_iocwr(nr: u32, len: usize) -> libc::c_ulong {
    (((IOC_READ | IOC_WRITE) << IOC_DIRSHIFT)
        | ((b'H' as u32) << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)
        | ((len as u32) << IOC_SIZESHIFT)) as libc::c_ulong
}

fn hidiocsfeature(len: usize) -> libc::c_ulong {
    hid_iocwr(0x06, len)
}

fn hidiocgfeature(len: usize) -> libc::c_ulong {
    hid_iocwr(0x07, len)
}

/// Raw facts about one hidraw node, before filtering by the device table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawNode {
    pub path: String,
    pub bus: u16,
    pub id: DeviceId,
    pub name: String,
    pub interface: Option<u8>,
}

/// Parses a `uevent` file body (`HID_ID=0003:00003434:0000D035`, `HID_NAME=…`).
pub fn parse_uevent(text: &str) -> Option<(u16, DeviceId, String)> {
    let mut bus = None;
    let mut id = None;
    let mut name = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("HID_ID=") {
            let mut parts = v.split(':');
            let b = u16::from_str_radix(parts.next()?, 16).ok()?;
            let vid = u32::from_str_radix(parts.next()?, 16).ok()?;
            let pid = u32::from_str_radix(parts.next()?, 16).ok()?;
            bus = Some(b);
            id = Some(DeviceId::new(vid as u16, pid as u16));
        } else if let Some(v) = line.strip_prefix("HID_NAME=") {
            name = v.trim().to_string();
        }
    }
    Some((bus?, id?, name))
}

/// Scans sysfs. `root` is `/sys/class/hidraw` in production; tests point it
/// at a fake tree.
pub fn scan_sysfs(root: &Path) -> Vec<HidrawNode> {
    let mut nodes = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return nodes;
    };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    dirs.sort();
    for node in dirs {
        let Some(base) = node.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !base.starts_with("hidraw") {
            continue;
        }
        if let Some(n) = read_node(root, base) {
            nodes.push(n);
        }
    }
    nodes
}

/// Reads one node's sysfs entry (`<class_root>/hidrawN/device/…`).
fn read_node(class_root: &Path, base: &str) -> Option<HidrawNode> {
    let dev = class_root.join(base).join("device");
    let dev = fs::canonicalize(&dev).unwrap_or(dev);
    let uevent = fs::read_to_string(dev.join("uevent")).ok()?;
    let (bus, id, name) = parse_uevent(&uevent)?;
    let interface = dev
        .parent()
        .and_then(|intf| fs::read_to_string(intf.join("bInterfaceNumber")).ok())
        .and_then(|s| u8::from_str_radix(s.trim(), 16).ok());
    Some(HidrawNode {
        path: format!("/dev/{base}"),
        bus,
        id,
        name,
        interface,
    })
}

/// Identifies `path` (a `/dev/hidrawN` node or a symlink to one, e.g. a udev
/// alias) from sysfs without opening it. `class_root` is `/sys/class/hidraw`
/// in production and a fake tree in tests. When the path exists it must be
/// a character device whose device number matches sysfs `dev`.
pub fn identify_in(class_root: &Path, path: &str) -> Result<NodeIdentity, TransportError> {
    let real = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let base = real
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let is_hidraw_name = base
        .strip_prefix("hidraw")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if !is_hidraw_name {
        return Err(TransportError::Unsupported(format!(
            "{path} is not a hidraw node (/dev/hidrawN); refusing to open it"
        )));
    }
    if let Ok(meta) = fs::metadata(&real) {
        if !meta.file_type().is_char_device() {
            return Err(TransportError::Unsupported(format!(
                "{path} is not a character device; refusing to open it"
            )));
        }
        if let Ok(dev) = fs::read_to_string(class_root.join(&base).join("dev")) {
            let rdev = meta.rdev();
            let (major, minor) = (libc::major(rdev), libc::minor(rdev));
            if dev.trim() != format!("{major}:{minor}") {
                return Err(TransportError::Unsupported(format!(
                    "{path} (device {major}:{minor}) does not match sysfs {base} ({}); refusing to open it",
                    dev.trim()
                )));
            }
        }
    } else if !class_root.join(&base).exists() {
        return Err(TransportError::Disconnected {
            path: path.to_string(),
        });
    }
    let node = read_node(class_root, &base).ok_or_else(|| {
        TransportError::Unsupported(format!(
            "{path}: no HID identity in sysfs ({}/{base}/device/uevent); refusing to open it",
            class_root.display()
        ))
    })?;
    Ok(NodeIdentity {
        path: node.path,
        id: node.id,
        interface: node.interface,
        product: node.name,
    })
}

/// [`identify_in`] against the real sysfs.
pub fn identify(path: &str) -> Result<NodeIdentity, TransportError> {
    identify_in(Path::new(SYSFS_HIDRAW), path)
}

pub fn enumerate() -> Result<Vec<FoundDevice>, TransportError> {
    Ok(scan_sysfs(Path::new(SYSFS_HIDRAW))
        .into_iter()
        .filter_map(|n| {
            device::lookup(n.id).map(|model| FoundDevice {
                path: n.path,
                id: n.id,
                interface: n.interface,
                product: n.name,
                model,
            })
        })
        .collect())
}

/// Opens a node that already passed [`crate::vet`].
pub(crate) fn open_vetted(path: &str) -> Result<Box<dyn Transport>, TransportError> {
    Ok(Box::new(HidrawDevice::open(path)?))
}

#[derive(Debug)]
pub struct HidrawDevice {
    file: File,
    path: String,
}

impl HidrawDevice {
    pub fn open(path: &str) -> Result<HidrawDevice, TransportError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| TransportError::io(path, e))?;
        Ok(HidrawDevice {
            file,
            path: path.to_string(),
        })
    }

    fn ioctl(&self, req: libc::c_ulong, buf: &mut [u8]) -> Result<usize, TransportError> {
        // SAFETY: `buf` is a valid, writable buffer of the length encoded in
        // `req`; the kernel copies at most that many bytes.
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), req, buf.as_mut_ptr()) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            // After an unplug hidraw returns ENODEV (mapped by `io`), or EIO
            // while the node is being torn down: check whether it is gone.
            let gone = !Path::new(&self.path).exists();
            return Err(TransportError::io_after_check(&self.path, err, gone));
        }
        Ok(rc as usize)
    }
}

impl Transport for HidrawDevice {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), TransportError> {
        if report.is_empty() || report.len() > 4096 {
            return Err(TransportError::BadReport(format!(
                "{} byte report",
                report.len()
            )));
        }
        let mut buf = report.to_vec();
        self.ioctl(hidiocsfeature(buf.len()), &mut buf)?;
        Ok(())
    }

    fn get_feature(&mut self, report_id: u8, len: usize) -> Result<Vec<u8>, TransportError> {
        let mut buf = vec![0u8; len];
        buf[0] = report_id;
        let n = self.ioctl(hidiocgfeature(len), &mut buf)?;
        buf.truncate(n.min(len));
        Ok(buf)
    }

    fn describe(&self) -> String {
        self.path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_python_reference() {
        // Python reference: _ioc(3, 0x06, 21) and _ioc(3, 0x07, 21)
        assert_eq!(hidiocsfeature(21), 0xC015_4806);
        assert_eq!(hidiocgfeature(21), 0xC015_4807);
        assert_eq!(hidiocgfeature(65), 0xC041_4807);
    }

    #[test]
    fn parses_uevent() {
        let text = "DRIVER=hid-generic\nHID_ID=0003:00003434:0000D035\nHID_NAME=Keychron Keychron M1\nHID_PHYS=usb-0000:03:00.0-1/input2\nMODALIAS=hid:b0003g0001v00003434p0000D035\n";
        let (bus, id, name) = parse_uevent(text).unwrap();
        assert_eq!(bus, 3);
        assert_eq!(id, DeviceId::new(0x3434, 0xd035));
        assert_eq!(name, "Keychron Keychron M1");
        assert!(parse_uevent("DRIVER=hid-generic\n").is_none());
    }

    /// A fake `/sys`: the M1 with its three interfaces (hidraw7-9, vendor =
    /// iface 2 = hidraw9), a Bluetooth keyboard (hidraw3), an M3 4K on
    /// iface 3 (hidraw4), and the M1 over Bluetooth (hidraw5, no interface).
    fn fake_sysfs(tag: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("tailglow-sysfs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let class = root.join("class/hidraw");
        fs::create_dir_all(&class).unwrap();
        let usb = |n: u32, iface: u8, pid: &str, name: &str| {
            let intf = root.join(format!("devices/usb1/1-{n}/1-{n}:1.{iface}"));
            let hid = intf.join(format!("0003:3434:{pid}.000{n}"));
            fs::create_dir_all(&hid).unwrap();
            fs::write(intf.join("bInterfaceNumber"), format!("{iface:02x}\n")).unwrap();
            fs::write(
                hid.join("uevent"),
                format!("HID_ID=0003:00003434:0000{pid}\nHID_NAME={name}\n"),
            )
            .unwrap();
            let node = class.join(format!("hidraw{n}"));
            fs::create_dir_all(&node).unwrap();
            std::os::unix::fs::symlink(&hid, node.join("device")).unwrap();
        };
        for (n, iface) in [(7u32, 0u8), (8, 1), (9, 2)] {
            usb(n, iface, "D035", "Keychron Keychron M1");
        }
        usb(4, 3, "D03C", "Keychron M3 4K");
        let bt_dev = |n: u32, ids: &str, name: &str| {
            let bt = root.join(format!("devices/bt/{n}"));
            fs::create_dir_all(&bt).unwrap();
            fs::write(
                bt.join("uevent"),
                format!("HID_ID={ids}\nHID_NAME={name}\n"),
            )
            .unwrap();
            fs::create_dir_all(class.join(format!("hidraw{n}"))).unwrap();
            std::os::unix::fs::symlink(&bt, class.join(format!("hidraw{n}/device"))).unwrap();
        };
        bt_dev(3, "0005:0000046D:0000B33F", "KB");
        bt_dev(5, "0005:00003434:0000D035", "Keychron M1");
        (root, class)
    }

    #[test]
    fn identifies_nodes_from_sysfs_and_vets_them_before_opening() {
        let (root, class) = fake_sysfs("identify");
        let id = |p: &str| identify_in(&class, p);
        // The vendor interface passes.
        let n = id("/dev/hidraw9").unwrap();
        assert_eq!(n.interface, Some(2));
        assert_eq!(n.id, DeviceId::new(0x3434, 0xd035));
        assert_eq!(crate::vet(n).unwrap().model.name, "Keychron M1");
        // Pointer interfaces, other vendors, unsupported families and the
        // Bluetooth M1 are identified but refused by vet().
        for (path, why) in [
            ("/dev/hidraw7", "interface 0"),
            ("/dev/hidraw8", "interface 1"),
            ("/dev/hidraw3", "not a mouse Tailglow knows"),
            ("/dev/hidraw4", "unsupported"),
            ("/dev/hidraw5", "without a USB interface"),
        ] {
            let err = crate::vet(id(path).unwrap()).unwrap_err().to_string();
            assert!(err.contains(why), "{path}: {err}");
        }
        // Not a hidraw name, or no sysfs entry at all: refused outright.
        assert!(matches!(
            id("/tmp/hidraw9x"),
            Err(TransportError::Unsupported(_))
        ));
        assert!(matches!(
            id("/dev/sda"),
            Err(TransportError::Unsupported(_))
        ));
        assert!(matches!(
            id("/dev/hidraw42"),
            Err(TransportError::Disconnected { .. })
        ));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scans_a_fake_sysfs_tree() {
        let (root, class) = fake_sysfs("scan");
        let nodes = scan_sysfs(&class);
        assert_eq!(nodes.len(), 6);
        let m1: Vec<_> = nodes
            .iter()
            .filter(|n| n.id == DeviceId::new(0x3434, 0xd035))
            .collect();
        assert_eq!(m1.len(), 4);
        assert_eq!(
            nodes
                .iter()
                .filter(|n| device::lookup(n.id).is_some())
                .count(),
            5
        );
        let vendor = m1.iter().find(|n| n.interface == Some(2)).unwrap();
        assert_eq!(vendor.path, "/dev/hidraw9");
        assert_eq!(vendor.name, "Keychron Keychron M1");
        let kb = nodes.iter().find(|n| n.path == "/dev/hidraw3").unwrap();
        assert_eq!(kb.interface, None);
        assert_eq!(kb.bus, 5);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn open_missing_path_is_a_clean_error() {
        let err = HidrawDevice::open("/dev/hidraw-does-not-exist").unwrap_err();
        assert!(matches!(err, TransportError::Disconnected { .. }), "{err}");
    }
}
