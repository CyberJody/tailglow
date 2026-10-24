// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake BlueZ for tests and screenshots, served on a **private** D-Bus
//! (never the system bus).
//!
//! It owns `org.bluez` on the bus it is given and serves:
//! - `/` with `org.freedesktop.DBus.ObjectManager` (zbus's built-in one, so
//!   `GetManagedObjects`, `InterfacesAdded` and `InterfacesRemoved` behave
//!   like BlueZ);
//! - `/org/bluez/hci0` with `org.bluez.Adapter1` (`RemoveDevice`);
//! - one object per device with `org.bluez.Device1`, `org.bluez.Battery1`
//!   (while connected) and, for Keychron mice, a GATT characteristic like
//!   the M1's OTA one (`…/service0030/char0031`).
//!
//! Every method call any client makes is recorded ([`FakeBluez::calls`]),
//! including the ones Tailglow must never make (`Pair`, `Connect`,
//! `Disconnect`, `WriteValue`, `ReadValue`, `StartNotify`…), so a test can
//! prove the client only read.
//!
//! [`PrivateBus`] starts a throw-away `dbus-daemon` for it. The example
//! `fake-bluez` (crates/tailglow-hid/examples) runs the same thing for
//! scripts, with a control interface `io.github.cyberjody.Tailglow.FakeBluez`
//! at `/fake` (`SetBattery`, `SetConnected`, `Calls`, `Quit`).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// One device the fake knows about.
#[derive(Debug, Clone)]
pub struct FakeDevice {
    /// Last part of the object path, e.g. `dev_AA_BB_CC_00_11_22`.
    pub id: String,
    pub name: String,
    pub address: String,
    pub modalias: Option<String>,
    pub paired: bool,
    pub connected: bool,
    pub battery: u8,
}

impl FakeDevice {
    /// The M1 as BlueZ reports it (name and modalias as on a real M1; the
    /// address is made up).
    pub fn m1_connected(battery: u8) -> FakeDevice {
        FakeDevice {
            id: "dev_AA_BB_CC_00_11_22".into(),
            name: "keychron M1".into(),
            address: "AA:BB:CC:00:11:22".into(),
            modalias: Some("usb:v3434pD035d0100".into()),
            paired: true,
            connected: true,
            battery,
        }
    }

    /// An old pairing of the same mouse (it re-pairs with a new address).
    pub fn m1_stale() -> FakeDevice {
        FakeDevice {
            id: "dev_D1_D2_D3_D4_D5_D6".into(),
            name: "keychron M1".into(),
            address: "D1:D2:D3:D4:D5:D6".into(),
            modalias: Some("usb:v3434pD035d0100".into()),
            paired: true,
            connected: false,
            battery: 0,
        }
    }

    /// Something that is not a Keychron mouse.
    pub fn headphones() -> FakeDevice {
        FakeDevice {
            id: "dev_3C_4D_BE_00_00_01".into(),
            name: "Headphones".into(),
            address: "3C:4D:BE:00:00:01".into(),
            modalias: Some("bluetooth:v004Cp200Ed0001".into()),
            paired: true,
            connected: false,
            battery: 80,
        }
    }

    /// Default scene: the connected M1, one stale M1 pairing, headphones.
    pub fn default_scene(battery: u8) -> Vec<FakeDevice> {
        vec![
            FakeDevice::m1_connected(battery),
            FakeDevice::m1_stale(),
            FakeDevice::headphones(),
        ]
    }
}

type Calls = Arc<Mutex<Vec<String>>>;

const ADAPTER: &str = "/org/bluez/hci0";

fn record(calls: &Calls, what: impl Into<String>) {
    calls.lock().unwrap().push(what.into());
}

struct Adapter {
    calls: Calls,
}

#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    async fn remove_device(
        &self,
        device: OwnedObjectPath,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> zbus::fdo::Result<()> {
        record(
            &self.calls,
            format!("Adapter1.RemoveDevice {}", device.as_str()),
        );
        let p = device.as_str().to_string();
        let found = server
            .remove::<Device, _>(p.as_str())
            .await
            .unwrap_or(false);
        if !found {
            return Err(zbus::fdo::Error::Failed(
                "org.bluez.Error.DoesNotExist".into(),
            ));
        }
        let _ = server.remove::<Battery, _>(p.as_str()).await;
        let ch = format!("{p}/service0030/char0031");
        let _ = server.remove::<Characteristic, _>(ch.as_str()).await;
        Ok(())
    }

    #[zbus(property)]
    fn address(&self) -> String {
        "00:11:22:33:44:55".into()
    }
    #[zbus(property)]
    fn name(&self) -> String {
        "fake-adapter".into()
    }
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
    }
}

struct Device {
    dev: FakeDevice,
    calls: Calls,
}

#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    // State-changing methods Tailglow must never call: recorded, then OK.
    fn connect(&self) {
        record(&self.calls, format!("Device1.Connect {}", self.dev.id));
    }
    fn disconnect(&self) {
        record(&self.calls, format!("Device1.Disconnect {}", self.dev.id));
    }
    fn pair(&self) {
        record(&self.calls, format!("Device1.Pair {}", self.dev.id));
    }
    fn cancel_pairing(&self) {
        record(
            &self.calls,
            format!("Device1.CancelPairing {}", self.dev.id),
        );
    }

    #[zbus(property)]
    fn name(&self) -> String {
        self.dev.name.clone()
    }
    #[zbus(property)]
    fn alias(&self) -> String {
        self.dev.name.clone()
    }
    #[zbus(property)]
    fn address(&self) -> String {
        self.dev.address.clone()
    }
    #[zbus(property)]
    fn address_type(&self) -> String {
        "random".into()
    }
    #[zbus(property)]
    fn modalias(&self) -> zbus::fdo::Result<String> {
        self.dev
            .modalias
            .clone()
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no modalias".into()))
    }
    #[zbus(property)]
    fn paired(&self) -> bool {
        self.dev.paired
    }
    #[zbus(property)]
    fn bonded(&self) -> bool {
        self.dev.paired
    }
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.dev.connected
    }
    #[zbus(property)]
    fn adapter(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(ADAPTER).unwrap()
    }
}

struct Battery {
    percentage: u8,
}

#[zbus::interface(name = "org.bluez.Battery1")]
impl Battery {
    #[zbus(property)]
    fn percentage(&self) -> u8 {
        self.percentage
    }
}

/// Like the M1's Telink OTA characteristic: any access is recorded.
struct Characteristic {
    calls: Calls,
}

#[zbus::interface(name = "org.bluez.GattCharacteristic1")]
impl Characteristic {
    fn read_value(&self, _options: std::collections::HashMap<String, OwnedValue>) -> Vec<u8> {
        record(&self.calls, "GattCharacteristic1.ReadValue");
        vec![0, 0, 0, 0]
    }
    fn write_value(&self, value: Vec<u8>, _options: std::collections::HashMap<String, OwnedValue>) {
        record(
            &self.calls,
            format!("GattCharacteristic1.WriteValue {value:02x?}"),
        );
    }
    fn start_notify(&self) {
        record(&self.calls, "GattCharacteristic1.StartNotify");
    }
    fn stop_notify(&self) {
        record(&self.calls, "GattCharacteristic1.StopNotify");
    }
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        "00010203-0405-0607-0809-0a0b0c0d2b12".into()
    }
}

/// A running fake BlueZ.
pub struct FakeBluez {
    conn: zbus::blocking::Connection,
    calls: Calls,
}

impl FakeBluez {
    /// Serves `devices` as `org.bluez` on the bus at `address`.
    pub fn serve(address: &str, devices: &[FakeDevice]) -> zbus::Result<FakeBluez> {
        let calls: Calls = Arc::default();
        let conn = zbus::blocking::connection::Builder::address(address)?
            .serve_at("/", zbus::fdo::ObjectManager)?
            .serve_at(
                ADAPTER,
                Adapter {
                    calls: calls.clone(),
                },
            )?
            .build()?;
        let fake = FakeBluez { conn, calls };
        for d in devices {
            fake.add_device(d.clone())?;
        }
        // Take the name last, so clients never see a half-built tree.
        fake.conn.request_name("org.bluez")?;
        Ok(fake)
    }

    fn device_path(id: &str) -> String {
        format!("{ADAPTER}/{id}")
    }

    /// Adds a device (emits InterfacesAdded).
    pub fn add_device(&self, d: FakeDevice) -> zbus::Result<()> {
        let path = FakeBluez::device_path(&d.id);
        let srv = self.conn.object_server();
        if d.connected {
            srv.at(
                path.as_str(),
                Battery {
                    percentage: d.battery,
                },
            )?;
        }
        if d.modalias
            .as_deref()
            .is_some_and(|m| m.starts_with("usb:v3434"))
        {
            srv.at(
                format!("{path}/service0030/char0031").as_str(),
                Characteristic {
                    calls: self.calls.clone(),
                },
            )?;
        }
        srv.at(
            path.as_str(),
            Device {
                dev: d,
                calls: self.calls.clone(),
            },
        )?;
        Ok(())
    }

    /// Changes a connected device's battery level (emits PropertiesChanged).
    pub fn set_battery(&self, id: &str, percentage: u8) -> zbus::Result<()> {
        let path = FakeBluez::device_path(id);
        let iface = self
            .conn
            .object_server()
            .interface::<_, Battery>(path.as_str())?;
        iface.get_mut().percentage = percentage;
        let emitter: &SignalEmitter<'static> = iface.signal_emitter();
        zbus::block_on(iface.get().percentage_changed(emitter))?;
        Ok(())
    }

    /// Connects or disconnects a device (PropertiesChanged on Device1, and
    /// Battery1 added or removed like BlueZ does).
    pub fn set_connected(&self, id: &str, connected: bool, battery: u8) -> zbus::Result<()> {
        let path = FakeBluez::device_path(id);
        let srv = self.conn.object_server();
        let iface = srv.interface::<_, Device>(path.as_str())?;
        iface.get_mut().dev.connected = connected;
        let emitter: &SignalEmitter<'static> = iface.signal_emitter();
        zbus::block_on(iface.get().connected_changed(emitter))?;
        if connected {
            srv.at(
                path.as_str(),
                Battery {
                    percentage: battery,
                },
            )?;
        } else {
            srv.remove::<Battery, _>(path.as_str())?;
        }
        Ok(())
    }

    /// Every method call clients made, in order ("Iface.Method args").
    /// `GetManagedObjects` and property reads are not listed (zbus answers
    /// them itself); everything else is.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The connection, e.g. to serve extra objects (the example's control).
    pub fn connection(&self) -> &zbus::blocking::Connection {
        &self.conn
    }
}

/// A private `dbus-daemon` (session config, random socket), killed on drop.
pub struct PrivateBus {
    child: Child,
    address: String,
}

impl PrivateBus {
    /// `None` if `dbus-daemon` is not installed or fails to start.
    pub fn start() -> Option<PrivateBus> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut line = String::new();
        let stdout = child.stdout.take()?;
        BufReader::new(stdout).read_line(&mut line).ok()?;
        let address = line.trim().to_string();
        if address.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(PrivateBus { child, address })
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
