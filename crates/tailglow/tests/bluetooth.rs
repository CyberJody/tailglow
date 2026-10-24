// SPDX-License-Identifier: GPL-3.0-or-later
//! `tailglow status` / `tailglow bluetooth` against a fake BlueZ on a private
//! D-Bus (never the system bus).
#![cfg(target_os = "linux")]

use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tailglow_hid::bluez::fake::{FakeBluez, FakeDevice, PrivateBus};

/// `tailglow` with a private config dir and no real hardware: USB comes from
/// `usb_fake` (`none` = no cable), Bluetooth from the fake BlueZ.
fn tailglow(cfg: &std::path::Path, usb_fake: &str, bus: &str) -> Command {
    let mut c = Command::cargo_bin("tailglow").unwrap();
    c.env_remove("TAILGLOW_STATE")
        .env("XDG_CONFIG_HOME", cfg)
        .env("TAILGLOW_FAKE", usb_fake)
        .env("TAILGLOW_BLUEZ_BUS", bus);
    c
}

fn fake_bluez(devices: &[FakeDevice]) -> Option<(PrivateBus, FakeBluez)> {
    let Some(bus) = PrivateBus::start() else {
        eprintln!("SKIP: dbus-daemon not available");
        return None;
    };
    let fake = FakeBluez::serve(bus.address(), devices).unwrap();
    Some((bus, fake))
}

#[test]
fn status_bluetooth_only_is_read_only_with_battery() {
    let Some((bus, fake)) = fake_bluez(&FakeDevice::default_scene(95)) else {
        return;
    };
    let cfg = tempfile::tempdir().unwrap();
    tailglow(cfg.path(), "none", bus.address())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("USB cable:  not connected"))
        .stdout(predicate::str::contains(
            "Bluetooth:  Keychron M1 connected (\"keychron M1\", AA:BB:CC:00:11:22), battery 95%",
        ))
        .stdout(predicate::str::contains("1 more pairing(s) not connected (old pairings)"))
        .stdout(predicate::str::contains(
            "Settings:   read-only: settings can only be changed over the USB cable \
             (Keychron's firmware doesn't support it over Bluetooth). Settings you save stay in the mouse.",
        ));
    // `info` explains why it cannot read settings.
    tailglow(cfg.path(), "none", bus.address())
        .arg("info")
        .assert()
        .failure()
        .stderr(predicate::str::contains("connected over Bluetooth"))
        .stderr(predicate::str::contains("Connect the USB cable"));
    assert!(fake.calls().is_empty(), "reads only: {:?}", fake.calls());
}

#[test]
fn status_usb_and_bluetooth_and_nothing() {
    let Some((bus, _fake)) = fake_bluez(&[FakeDevice::m1_connected(60)]) else {
        return;
    };
    let cfg = tempfile::tempdir().unwrap();
    tailglow(cfg.path(), "session2", bus.address())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "USB cable:  Keychron M1 via fake mouse (session 2 baseline), battery 100%",
        ))
        .stdout(predicate::str::contains("battery 60%"))
        .stdout(predicate::str::contains(
            "Settings:   can be changed (USB cable)",
        ));
    // No BlueZ at all, no cable: exit 1, nothing crashes.
    let empty = PrivateBus::start().unwrap();
    tailglow(cfg.path(), "none", empty.address())
        .arg("status")
        .assert()
        .failure()
        .stdout(predicate::str::contains("Bluetooth:  unavailable"))
        .stdout(predicate::str::contains("Settings:   no mouse connected"));
    // The fake mouse alone never looks at the real system bus.
    let mut c = Command::cargo_bin("tailglow").unwrap();
    c.env_remove("TAILGLOW_BLUEZ_BUS")
        .env_remove("TAILGLOW_STATE")
        .env("XDG_CONFIG_HOME", cfg.path())
        .env("TAILGLOW_FAKE", "session2")
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Bluetooth:  not checked"));
}

#[test]
fn forget_stale_needs_yes_and_never_touches_the_connected_mouse() {
    let Some((bus, fake)) = fake_bluez(&FakeDevice::default_scene(95)) else {
        return;
    };
    let cfg = tempfile::tempdir().unwrap();
    tailglow(cfg.path(), "none", bus.address())
        .arg("bluetooth")
        .assert()
        .success()
        .stdout(predicate::str::contains("dev_AA_BB_CC_00_11_22"))
        .stdout(predicate::str::contains("connected, battery 95%"))
        .stdout(predicate::str::contains("dev_D1_D2_D3_D4_D5_D6"))
        .stdout(predicate::str::contains("Headphones").not());
    // Without --yes: a list and exit 1, no call.
    tailglow(cfg.path(), "none", bus.address())
        .args(["bluetooth", "forget-stale"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("would remove 1 old pairing(s)"))
        .stdout(predicate::str::contains("D1:D2:D3:D4:D5:D6"))
        .stdout(predicate::str::contains("--yes"));
    assert!(fake.calls().is_empty());
    // With --yes: exactly one RemoveDevice, for the stale entry.
    tailglow(cfg.path(), "none", bus.address())
        .args(["bluetooth", "forget-stale", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "removed /org/bluez/hci0/dev_D1_D2_D3_D4_D5_D6",
        ));
    assert_eq!(
        fake.calls(),
        vec!["Adapter1.RemoveDevice /org/bluez/hci0/dev_D1_D2_D3_D4_D5_D6".to_string()]
    );
    tailglow(cfg.path(), "none", bus.address())
        .args(["bluetooth", "forget-stale", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no old Keychron pairings"));
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn forget_stale_warns_when_nothing_is_connected() {
    let Some((bus, fake)) = fake_bluez(&[FakeDevice::m1_stale()]) else {
        return;
    };
    let cfg = tempfile::tempdir().unwrap();
    tailglow(cfg.path(), "none", bus.address())
        .args(["bluetooth", "forget-stale"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "may be your current pairing with the mouse asleep",
        ));
    assert!(fake.calls().is_empty());
}
