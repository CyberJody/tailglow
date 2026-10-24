// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake BlueZ on a private D-Bus, for screenshots and manual tests.
//!
//!     cargo run -p tailglow-hid --example fake-bluez -- [--battery N] [--scene default|connected|stale]
//!
//! Starts its own `dbus-daemon`, prints `TAILGLOW_BLUEZ_BUS=<address>` on
//! the first line of stdout, then serves until killed or until
//! `io.github.cyberjody.Tailglow.FakeBluez.Quit` is called on `/fake`:
//!
//!     gdbus call --address "$ADDR" --dest org.bluez --object-path /fake \
//!         --method io.github.cyberjody.Tailglow.FakeBluez.SetBattery 14
//!
//! Never touches the system bus.

#[cfg(target_os = "linux")]
mod imp {
    use std::io::Write;
    use std::sync::mpsc;

    use tailglow_hid::bluez::fake::{FakeBluez, FakeDevice, PrivateBus};

    const M1: &str = "dev_AA_BB_CC_00_11_22";

    struct Control {
        fake: std::sync::Arc<std::sync::Mutex<Option<FakeBluez>>>,
        quit: mpsc::Sender<()>,
    }

    #[zbus::interface(name = "io.github.cyberjody.Tailglow.FakeBluez")]
    impl Control {
        fn set_battery(&self, percentage: u8) -> zbus::fdo::Result<()> {
            let fake = self.fake.clone();
            // The object server runs this on its executor; do the blocking
            // property update on a plain thread.
            std::thread::spawn(move || {
                if let Some(f) = fake.lock().unwrap().as_ref() {
                    let _ = f.set_battery(M1, percentage);
                }
            })
            .join()
            .map_err(|_| zbus::fdo::Error::Failed("set_battery panicked".into()))
        }

        fn set_connected(&self, connected: bool, battery: u8) -> zbus::fdo::Result<()> {
            let fake = self.fake.clone();
            std::thread::spawn(move || {
                if let Some(f) = fake.lock().unwrap().as_ref() {
                    let _ = f.set_connected(M1, connected, battery);
                }
            })
            .join()
            .map_err(|_| zbus::fdo::Error::Failed("set_connected panicked".into()))
        }

        fn calls(&self) -> Vec<String> {
            self.fake
                .lock()
                .unwrap()
                .as_ref()
                .map(|f| f.calls())
                .unwrap_or_default()
        }

        fn quit(&self) {
            let _ = self.quit.send(());
        }
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut battery = 95u8;
        let mut scene = "default".to_string();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--battery" => {
                    battery = args[i + 1].parse().expect("--battery N");
                    i += 1;
                }
                "--scene" => {
                    scene = args[i + 1].clone();
                    i += 1;
                }
                other => panic!("unknown argument {other}"),
            }
            i += 1;
        }
        let devices = match scene.as_str() {
            "default" => FakeDevice::default_scene(battery),
            "connected" => vec![FakeDevice::m1_connected(battery)],
            "stale" => vec![FakeDevice::m1_stale()],
            other => panic!("unknown scene {other}"),
        };
        let bus = PrivateBus::start().expect("dbus-daemon");
        let fake = FakeBluez::serve(bus.address(), &devices).expect("serve");
        let (tx, rx) = mpsc::channel();
        let conn = fake.connection().clone();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Some(fake)));
        conn.object_server()
            .at(
                "/fake",
                Control {
                    fake: shared.clone(),
                    quit: tx,
                },
            )
            .expect("control interface");
        println!("TAILGLOW_BLUEZ_BUS={}", bus.address());
        std::io::stdout().flush().unwrap();
        let _ = rx.recv();
        let calls = shared
            .lock()
            .unwrap()
            .take()
            .map(|f| f.calls())
            .unwrap_or_default();
        eprintln!("fake-bluez: calls received: {calls:?}");
        drop(bus);
    }
}

#[cfg(target_os = "linux")]
fn main() {
    imp::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("fake-bluez needs Linux (BlueZ, dbus-daemon)");
}
