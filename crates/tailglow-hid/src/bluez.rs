// SPDX-License-Identifier: GPL-3.0-or-later
//! Bluetooth status for Keychron mice, read from BlueZ over D-Bus.
//!
//! Over Bluetooth the M1 exposes no settings channel (PROTOCOL.md §11: no
//! 0x51/0x52 vendor reports in its HID report map; Keychron's own app says
//! settings are "not supported in Bluetooth mode"). So this module only
//! reports **status**: is a Keychron mouse paired/connected, and its battery
//! level (`org.bluez.Battery1`). Settings stay on the USB cable.
//!
//! # Read-only by construction
//!
//! Every D-Bus method call goes through one private function, `call`, which
//! refuses anything not in [`ALLOWED_METHODS`]:
//!
//! - `org.freedesktop.DBus.ObjectManager.GetManagedObjects` (a read), and
//! - `org.bluez.Adapter1.RemoveDevice`, reachable only from
//!   [`Bluez::remove_stale`], which needs a [`UserConfirmed`] token, re-reads
//!   the device first and refuses one that is connected or is not a
//!   Keychron mouse.
//!
//! Status changes arrive as signals (`PropertiesChanged`, `InterfacesAdded`,
//! `InterfacesRemoved`) that we only listen to. Nothing here ever calls
//! `Pair`, `Connect`, `Disconnect`, `Properties.Set`, or any
//! `org.bluez.GattCharacteristic1` method (`ReadValue`, `WriteValue`,
//! `StartNotify`, `AcquireWrite`…). In particular **no GATT characteristic of
//! the mouse is ever written**: one of them is the Telink OTA firmware-update
//! characteristic. The tests `only_allowed_methods_in_source` and
//! `fake_bluez_sees_only_reads` (tests/bluez.rs in this crate) check both
//! rules: the first scans this file, the second records every call a fake
//! BlueZ receives.
//!
//! The BLE address of the M1 is random and changes on every re-pair, so a
//! mouse is recognised by its name (`keychron M1`) or its modalias
//! (`usb:v3434pD035…`), never by address.
//!
//! Which bus: `TAILGLOW_BLUEZ_BUS` = `system` (default), `off`, or a D-Bus
//! address (tests and screenshots run a fake BlueZ on a private bus). When
//! the fake mouse is in use (`TAILGLOW_FAKE`) and the variable is unset,
//! Bluetooth is off, so demos never show the real machine's devices.

use std::time::Duration;

#[cfg(target_os = "linux")]
pub mod fake;

/// The name the M1 advertises (and BlueZ reports as `Device1.Name`).
pub const M1_NAME: &str = "keychron M1";
/// `Device1.Modalias` prefix of the M1 (USB-style VID/PID 3434:d035).
pub const M1_MODALIAS_PREFIX: &str = "usb:v3434pD035";
/// Environment variable selecting the bus (see the module docs).
pub const BUS_ENV: &str = "TAILGLOW_BLUEZ_BUS";

/// The only D-Bus methods this module may call, as (interface, method).
pub const ALLOWED_METHODS: &[(&str, &str)] = &[
    ("org.freedesktop.DBus.ObjectManager", "GetManagedObjects"),
    ("org.bluez.Adapter1", "RemoveDevice"),
];

/// A Keychron mouse known to BlueZ (paired, or at least seen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtMouse {
    /// BlueZ object path, e.g. `/org/bluez/hci0/dev_AA_BB_CC_00_11_22`.
    pub path: String,
    /// The adapter's object path, e.g. `/org/bluez/hci0`.
    pub adapter: String,
    /// Model name from the device table, e.g. "Keychron M1".
    pub model: &'static str,
    /// `Device1.Name` (or `Alias` when the name is missing).
    pub name: String,
    /// For display only. It changes on every re-pair: never key on it.
    pub address: String,
    pub paired: bool,
    pub connected: bool,
    /// `Battery1.Percentage`; BlueZ only has it while connected.
    pub battery: Option<u8>,
}

impl BtMouse {
    /// Paired but not connected: an old pairing (the M1 makes a new random
    /// address on every re-pair, so old entries pile up).
    pub fn is_stale(&self) -> bool {
        self.paired && !self.connected
    }
}

/// What the Bluetooth side looks like right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtState {
    /// Bluetooth status is switched off (`TAILGLOW_BLUEZ_BUS=off`, the fake
    /// mouse, or not Linux).
    Disabled,
    /// No bus or no BlueZ on it (the text says why).
    Unavailable(String),
    /// Keychron mice BlueZ knows about (may be empty).
    Mice(Vec<BtMouse>),
}

impl BtState {
    pub fn mice(&self) -> &[BtMouse] {
        match self {
            BtState::Mice(m) => m,
            _ => &[],
        }
    }

    /// The first connected Keychron mouse, if any.
    pub fn connected(&self) -> Option<&BtMouse> {
        self.mice().iter().find(|m| m.connected)
    }

    /// Paired Keychron mice that are not connected.
    pub fn stale(&self) -> Vec<&BtMouse> {
        self.mice().iter().filter(|m| m.is_stale()).collect()
    }
}

/// True for a BlueZ device that is a Keychron M1, by name or modalias.
pub fn is_keychron_m1(name: Option<&str>, modalias: Option<&str>) -> bool {
    name.is_some_and(|n| n.trim().eq_ignore_ascii_case(M1_NAME))
        || modalias.is_some_and(|m| {
            m.to_ascii_lowercase()
                .starts_with(&M1_MODALIAS_PREFIX.to_ascii_lowercase())
        })
}

/// Proof that a person said yes to removing a pairing (a GUI confirm button
/// or the CLI's `--yes`). Only [`Bluez::remove_stale`] takes one.
#[derive(Debug)]
pub struct UserConfirmed(());

impl UserConfirmed {
    /// Call only right after the user explicitly confirmed the removal.
    pub fn after_explicit_confirmation() -> Self {
        UserConfirmed(())
    }
}

/// Errors from the BlueZ layer.
#[derive(Debug, thiserror::Error)]
pub enum BtError {
    #[error("Bluetooth status is switched off")]
    Disabled,
    #[error("D-Bus: {0}")]
    Bus(String),
    #[error("refusing D-Bus call {0}.{1}: not on the read-only allow list")]
    NotAllowed(String, String),
    #[error("{0}")]
    Refused(String),
}

/// Which bus to read BlueZ from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bus {
    Off,
    System,
    Address(String),
}

impl Bus {
    /// From `TAILGLOW_BLUEZ_BUS` (and `TAILGLOW_FAKE`, see the module docs).
    pub fn from_env() -> Bus {
        let fake = std::env::var_os(crate::FAKE_ENV).is_some_and(|v| !v.is_empty());
        Bus::parse(std::env::var(BUS_ENV).ok().as_deref(), fake)
    }

    fn parse(value: Option<&str>, fake_mouse: bool) -> Bus {
        match value.map(str::trim) {
            None | Some("") if fake_mouse => Bus::Off,
            None | Some("") | Some("system") => Bus::System,
            Some("off") | Some("none") | Some("0") => Bus::Off,
            Some(addr) => Bus::Address(addr.to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    pub(super) type Props = HashMap<String, OwnedValue>;
    pub(super) type Objects = HashMap<OwnedObjectPath, HashMap<String, Props>>;

    /// A connection to BlueZ.
    pub struct Bluez {
        conn: zbus::blocking::Connection,
    }

    impl Bluez {
        /// Connects to the bus chosen by the environment; `Ok(None)` when
        /// Bluetooth status is off.
        pub fn from_env() -> Result<Option<Bluez>, BtError> {
            match Bus::from_env() {
                Bus::Off => Ok(None),
                bus => Bluez::connect(&bus).map(Some),
            }
        }

        pub fn connect(bus: &Bus) -> Result<Bluez, BtError> {
            let builder = match bus {
                Bus::Off => return Err(BtError::Disabled),
                Bus::System => zbus::blocking::connection::Builder::system(),
                Bus::Address(a) => zbus::blocking::connection::Builder::address(a.as_str()),
            }
            .map_err(bus_err)?;
            let conn = builder
                .method_timeout(Duration::from_secs(5))
                .build()
                .map_err(bus_err)?;
            Ok(Bluez { conn })
        }

        /// The single place a D-Bus method is called. Refuses anything not
        /// in [`ALLOWED_METHODS`].
        fn call<B, R>(&self, path: &str, iface: &str, method: &str, body: &B) -> Result<R, BtError>
        where
            B: serde::Serialize + zbus::zvariant::DynamicType,
            R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
        {
            if !ALLOWED_METHODS.contains(&(iface, method)) {
                return Err(BtError::NotAllowed(iface.into(), method.into()));
            }
            let reply = self
                .conn
                .call_method(Some("org.bluez"), path, Some(iface), method, body)
                .map_err(bus_err)?;
            reply.body().deserialize::<R>().map_err(bus_err)
        }

        fn objects(&self) -> Result<Objects, BtError> {
            self.call(
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
                &(),
            )
        }

        /// Keychron mice BlueZ knows about, sorted connected-first.
        pub fn mice(&self) -> Result<Vec<BtMouse>, BtError> {
            Ok(mice_from(&self.objects()?))
        }

        pub fn state(&self) -> BtState {
            match self.mice() {
                Ok(m) => BtState::Mice(m),
                Err(e) => BtState::Unavailable(e.to_string()),
            }
        }

        /// Removes one stale pairing (`Adapter1.RemoveDevice`). Re-reads
        /// BlueZ first and refuses unless the device still exists, is a
        /// Keychron M1, is paired and is NOT connected.
        pub fn remove_stale(&self, mouse: &BtMouse, _yes: UserConfirmed) -> Result<(), BtError> {
            let now = self.mice()?;
            let Some(cur) = now.iter().find(|m| m.path == mouse.path) else {
                return Err(BtError::Refused(format!(
                    "{} is no longer known to BlueZ",
                    mouse.path
                )));
            };
            if cur.connected {
                return Err(BtError::Refused(format!(
                    "{} ({}) is connected; only pairings that are not connected are removed",
                    cur.name, cur.address
                )));
            }
            if !cur.paired {
                return Err(BtError::Refused(format!("{} is not paired", cur.path)));
            }
            let device = OwnedObjectPath::try_from(cur.path.as_str()).map_err(bus_err)?;
            self.call::<_, ()>(
                &cur.adapter,
                "org.bluez.Adapter1",
                "RemoveDevice",
                &(device,),
            )
        }

        /// A blocking iterator over BlueZ's signals (we only listen).
        fn subscribe(&self) -> Result<zbus::blocking::MessageIterator, BtError> {
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.bluez")
                .map_err(bus_err)?
                .build();
            zbus::blocking::MessageIterator::for_match_rule(rule, &self.conn, Some(64))
                .map_err(bus_err)
        }
    }

    fn bus_err(e: impl std::fmt::Display) -> BtError {
        BtError::Bus(e.to_string())
    }

    fn prop_str(p: &Props, k: &str) -> Option<String> {
        p.get(k)
            .and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_string))
    }

    fn prop_bool(p: &Props, k: &str) -> bool {
        p.get(k)
            .and_then(|v| v.downcast_ref::<bool>().ok())
            .unwrap_or(false)
    }

    pub(super) fn mice_from(objects: &Objects) -> Vec<BtMouse> {
        let mut out: Vec<BtMouse> = objects
            .iter()
            .filter_map(|(path, ifaces)| {
                let dev = ifaces.get("org.bluez.Device1")?;
                let name = prop_str(dev, "Name").or_else(|| prop_str(dev, "Alias"));
                let modalias = prop_str(dev, "Modalias");
                if !is_keychron_m1(name.as_deref(), modalias.as_deref()) {
                    return None;
                }
                let path = path.as_str().to_string();
                let adapter = path
                    .rsplit_once('/')
                    .map(|(a, _)| a.to_string())
                    .unwrap_or_default();
                let battery = ifaces
                    .get("org.bluez.Battery1")
                    .and_then(|b| b.get("Percentage"))
                    .and_then(|v| v.downcast_ref::<u8>().ok())
                    .map(|p| p.min(100));
                Some(BtMouse {
                    path,
                    adapter,
                    model: "Keychron M1",
                    name: name.unwrap_or_else(|| M1_NAME.into()),
                    address: prop_str(dev, "Address").unwrap_or_default(),
                    paired: prop_bool(dev, "Paired") || prop_bool(dev, "Bonded"),
                    connected: prop_bool(dev, "Connected"),
                    battery,
                })
            })
            .collect();
        out.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.path.cmp(&b.path)));
        out
    }

    /// Follows BlueZ: [`Watcher::wait_change`] blocks until the Keychron state
    /// differs from the last one returned (the first call returns at once).
    /// Re-reads on every BlueZ signal; reconnects every `retry` after errors.
    pub struct Watcher {
        bus: Bus,
        live: Option<(Bluez, zbus::blocking::MessageIterator)>,
        last: Option<BtState>,
        retry: Duration,
    }

    impl Watcher {
        pub fn from_env() -> Watcher {
            Watcher::new(Bus::from_env())
        }

        pub fn new(bus: Bus) -> Watcher {
            Watcher {
                bus,
                live: None,
                last: None,
                retry: Duration::from_secs(5),
            }
        }

        pub fn wait_change(&mut self) -> BtState {
            if self.bus == Bus::Off {
                if self.last.is_none() {
                    self.last = Some(BtState::Disabled);
                    return BtState::Disabled;
                }
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            loop {
                let state = match self.live.as_ref() {
                    Some((bz, _)) => bz.state(),
                    None => match Bluez::connect(&self.bus).and_then(|bz| {
                        // Subscribe before reading so no change is missed.
                        let it = bz.subscribe()?;
                        Ok((bz, it))
                    }) {
                        Ok(live) => {
                            let s = live.0.state();
                            self.live = Some(live);
                            s
                        }
                        Err(e) => BtState::Unavailable(e.to_string()),
                    },
                };
                if matches!(state, BtState::Unavailable(_)) {
                    self.live = None;
                }
                if self.last.as_ref() != Some(&state) {
                    self.last = Some(state.clone());
                    return state;
                }
                match self.live.as_mut() {
                    Some((_, it)) => match it.next() {
                        Some(Ok(_)) => {}
                        _ => self.live = None,
                    },
                    None => std::thread::sleep(self.retry),
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::{Bluez, Watcher};

/// Off Linux there is no BlueZ: Bluetooth status is reported as disabled.
#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub struct Bluez(());

    impl Bluez {
        pub fn from_env() -> Result<Option<Bluez>, BtError> {
            Ok(None)
        }
        pub fn connect(_bus: &Bus) -> Result<Bluez, BtError> {
            Err(BtError::Disabled)
        }
        pub fn mice(&self) -> Result<Vec<BtMouse>, BtError> {
            Ok(Vec::new())
        }
        pub fn state(&self) -> BtState {
            BtState::Disabled
        }
        pub fn remove_stale(&self, _m: &BtMouse, _yes: UserConfirmed) -> Result<(), BtError> {
            Err(BtError::Disabled)
        }
    }

    pub struct Watcher {
        done: bool,
    }

    impl Watcher {
        pub fn from_env() -> Watcher {
            Watcher { done: false }
        }
        pub fn new(_bus: Bus) -> Watcher {
            Watcher { done: false }
        }
        pub fn wait_change(&mut self) -> BtState {
            if self.done {
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            self.done = true;
            BtState::Disabled
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub use imp::{Bluez, Watcher};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_m1_by_name_or_modalias_not_address() {
        assert!(is_keychron_m1(Some("keychron M1"), None));
        assert!(is_keychron_m1(Some("Keychron M1 "), None));
        assert!(is_keychron_m1(None, Some("usb:v3434pD035d0100")));
        assert!(is_keychron_m1(None, Some("usb:v3434pd035d0100")));
        assert!(!is_keychron_m1(
            Some("AirPods Pro"),
            Some("bluetooth:v004Cp200EdD415")
        ));
        assert!(!is_keychron_m1(
            Some("keychron M3"),
            Some("usb:v3434pD032d0100")
        ));
        assert!(!is_keychron_m1(None, None));
    }

    #[test]
    fn bus_choice() {
        assert_eq!(Bus::parse(None, false), Bus::System);
        assert_eq!(Bus::parse(Some("system"), false), Bus::System);
        assert_eq!(Bus::parse(Some("off"), false), Bus::Off);
        assert_eq!(Bus::parse(None, true), Bus::Off, "fake mouse => no real BT");
        assert_eq!(Bus::parse(Some("system"), true), Bus::System);
        assert_eq!(
            Bus::parse(Some("unix:path=/tmp/x"), true),
            Bus::Address("unix:path=/tmp/x".into())
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn parses_managed_objects() {
        use imp::{mice_from, Objects};
        use std::collections::HashMap;
        use zbus::zvariant::OwnedObjectPath;
        use zbus::zvariant::{OwnedValue, Str};
        let s = |v: &str| OwnedValue::from(Str::from(v.to_string()));
        let b = |v: bool| OwnedValue::from(v);
        let mut objects: Objects = HashMap::new();
        let mut add = |path: &str, dev: Vec<(&str, OwnedValue)>, bat: Option<u8>| {
            let mut ifaces = HashMap::new();
            ifaces.insert(
                "org.bluez.Device1".to_string(),
                dev.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            );
            if let Some(p) = bat {
                let mut m = HashMap::new();
                m.insert("Percentage".to_string(), OwnedValue::from(p));
                ifaces.insert("org.bluez.Battery1".to_string(), m);
            }
            objects.insert(OwnedObjectPath::try_from(path).unwrap(), ifaces);
        };
        add(
            "/org/bluez/hci0/dev_AA",
            vec![("Name", s("Some Headphones")), ("Connected", b(true))],
            Some(50),
        );
        add(
            "/org/bluez/hci0/dev_F2",
            vec![
                ("Name", s("keychron M1")),
                ("Address", s("AA:BB:CC:00:11:22")),
                ("Paired", b(true)),
                ("Connected", b(true)),
            ],
            Some(95),
        );
        add(
            "/org/bluez/hci0/dev_D1",
            vec![
                ("Alias", s("my mouse")),
                ("Modalias", s("usb:v3434pD035d0100")),
                ("Paired", b(true)),
                ("Connected", b(false)),
            ],
            None,
        );
        let m = mice_from(&objects);
        assert_eq!(m.len(), 2, "{m:?}");
        assert_eq!(m[0].path, "/org/bluez/hci0/dev_F2");
        assert_eq!(m[0].adapter, "/org/bluez/hci0");
        assert!(m[0].connected && !m[0].is_stale());
        assert_eq!(m[0].battery, Some(95));
        assert_eq!(m[1].name, "my mouse");
        assert!(m[1].is_stale());
        assert_eq!(m[1].battery, None);
        let st = BtState::Mice(m);
        assert_eq!(st.connected().unwrap().battery, Some(95));
        assert_eq!(st.stale().len(), 1);
    }

    /// The read-only rule, checked on this file's own source: the only
    /// D-Bus method names that appear outside comments and tests are the
    /// two allowed ones, `call_method` appears exactly once (inside `call`),
    /// and no state-changing BlueZ method name appears in code at all.
    #[test]
    fn only_allowed_methods_in_source() {
        let src = include_str!("bluez.rs");
        // Code only: drop everything from the first test module on, and
        // drop comment lines.
        let code: String = src
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(t.starts_with("//") || t.starts_with("///") || t.starts_with("//!"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            code.matches(".call_method(").count(),
            1,
            "one D-Bus call site"
        );
        for forbidden in [
            "WriteValue",
            "AcquireWrite",
            "ReadValue",
            "StartNotify",
            "\"Pair\"",
            "\"Connect\"",
            "\"Disconnect\"",
            "ConnectProfile",
            "\"Set\"",
            "GattCharacteristic1",
            "GattDescriptor1",
            "set_property",
        ] {
            assert!(
                !code.contains(forbidden),
                "{forbidden} must not appear in bluez.rs code"
            );
        }
        assert_eq!(
            ALLOWED_METHODS,
            &[
                ("org.freedesktop.DBus.ObjectManager", "GetManagedObjects"),
                ("org.bluez.Adapter1", "RemoveDevice"),
            ]
        );
        // RemoveDevice is used in exactly one place besides the allow list.
        assert_eq!(code.matches("\"RemoveDevice\"").count(), 2);
    }
}
