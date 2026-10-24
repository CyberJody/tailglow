// SPDX-License-Identifier: GPL-3.0-or-later
//! The BlueZ status layer against a fake BlueZ on a private D-Bus.
//! Never touches the system bus. Skips (passes with a note) when
//! `dbus-daemon` is not installed.
#![cfg(target_os = "linux")]

use std::sync::mpsc;
use std::time::Duration;

use tailglow_hid::bluez::fake::{FakeBluez, FakeDevice, PrivateBus};
use tailglow_hid::bluez::{Bluez, BtState, Bus, UserConfirmed, Watcher};

fn setup(devices: &[FakeDevice]) -> Option<(PrivateBus, FakeBluez, Bus)> {
    let Some(bus) = PrivateBus::start() else {
        eprintln!("SKIP: dbus-daemon not available");
        return None;
    };
    let fake = FakeBluez::serve(bus.address(), devices).expect("serve fake BlueZ");
    let addr = Bus::Address(bus.address().to_string());
    Some((bus, fake, addr))
}

#[test]
fn lists_keychron_mice_and_battery() {
    let Some((_bus, fake, addr)) = setup(&FakeDevice::default_scene(95)) else {
        return;
    };
    let bz = Bluez::connect(&addr).unwrap();
    let mice = bz.mice().unwrap();
    assert_eq!(mice.len(), 2, "headphones are filtered out: {mice:?}");
    assert!(mice[0].connected);
    assert_eq!(mice[0].name, "keychron M1");
    assert_eq!(mice[0].battery, Some(95));
    assert!(mice[1].is_stale());
    assert_eq!(mice[1].battery, None);
    // Reading is invisible to the fake's call log: no method was called.
    assert_eq!(fake.calls(), Vec::<String>::new());
}

#[test]
fn watcher_follows_battery_and_connection_signals() {
    let Some((_bus, fake, addr)) = setup(&[FakeDevice::m1_connected(40)]) else {
        return;
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut w = Watcher::new(addr);
        loop {
            if tx.send(w.wait_change()).is_err() {
                break;
            }
        }
    });
    let next = || {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("watcher update")
    };
    let first = next();
    assert_eq!(first.connected().and_then(|m| m.battery), Some(40));

    fake.set_battery("dev_AA_BB_CC_00_11_22", 14).unwrap();
    let s = next();
    assert_eq!(s.connected().and_then(|m| m.battery), Some(14));

    fake.set_connected("dev_AA_BB_CC_00_11_22", false, 0)
        .unwrap();
    // Device1.Connected and the Battery1 removal are two signals; wait for
    // the state where the mouse is disconnected and has no battery.
    let mut s = next();
    while s
        .mice()
        .first()
        .map(|m| m.battery.is_some())
        .unwrap_or(false)
    {
        s = next();
    }
    assert!(s.connected().is_none());
    assert_eq!(s.stale().len(), 1);
    assert!(
        fake.calls().is_empty(),
        "watching made calls: {:?}",
        fake.calls()
    );
}

#[test]
fn removes_only_a_stale_pairing_and_nothing_else() {
    let Some((_bus, fake, addr)) = setup(&FakeDevice::default_scene(95)) else {
        return;
    };
    let bz = Bluez::connect(&addr).unwrap();
    let mice = bz.mice().unwrap();
    let connected = mice.iter().find(|m| m.connected).unwrap().clone();
    let stale = mice.iter().find(|m| m.is_stale()).unwrap().clone();

    // A connected mouse is refused before any call is made.
    let err = bz
        .remove_stale(&connected, UserConfirmed::after_explicit_confirmation())
        .unwrap_err();
    assert!(err.to_string().contains("is connected"), "{err}");
    assert!(fake.calls().is_empty());

    bz.remove_stale(&stale, UserConfirmed::after_explicit_confirmation())
        .unwrap();
    let after = bz.mice().unwrap();
    assert_eq!(after.len(), 1);
    assert!(after[0].connected);

    // The fake saw exactly one method call: the confirmed RemoveDevice.
    // No Pair/Connect/Disconnect, no GATT ReadValue/WriteValue/StartNotify.
    assert_eq!(
        fake.calls(),
        vec!["Adapter1.RemoveDevice /org/bluez/hci0/dev_D1_D2_D3_D4_D5_D6".to_string()]
    );

    // Removing it again is refused (it is gone), still without a call.
    let err = bz
        .remove_stale(&stale, UserConfirmed::after_explicit_confirmation())
        .unwrap_err();
    assert!(err.to_string().contains("no longer known"), "{err}");
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn no_bluez_on_the_bus_is_unavailable_not_an_error() {
    let Some(bus) = PrivateBus::start() else {
        eprintln!("SKIP: dbus-daemon not available");
        return;
    };
    let addr = Bus::Address(bus.address().to_string());
    let bz = Bluez::connect(&addr).unwrap();
    assert!(matches!(bz.state(), BtState::Unavailable(_)));
    let mut w = Watcher::new(addr);
    assert!(matches!(w.wait_change(), BtState::Unavailable(_)));
    let mut off = Watcher::new(Bus::Off);
    assert_eq!(off.wait_change(), BtState::Disabled);
}
