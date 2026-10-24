// SPDX-License-Identifier: GPL-3.0-or-later
//! CLI integration tests against the fake mouse. Every write the CLI makes
//! is recorded via TAILGLOW_FAKE_RECORD and compared with the exact bytes the
//! vendor app sent in testdata/session1.pcap and testdata/session2.pcap.
//!
//! Each CLI invocation starts a fresh fake (factory state for session 1,
//! the session 2 baseline for `TAILGLOW_FAKE=session2`), so tests reason about one
//! command's writes at a time.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        Sandbox {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn record(&self) -> PathBuf {
        self.dir.path().join("writes.hex")
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state.toml")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("tailglow").unwrap();
        c.env_remove("TAILGLOW_FAKE")
            .env("TAILGLOW_FAKE_RECORD", self.record())
            .env("TAILGLOW_STATE", self.state())
            .arg("--fake");
        c
    }

    /// Same, on the session-2 baseline fake.
    fn cmd2(&self) -> Command {
        let mut c = Command::cargo_bin("tailglow").unwrap();
        c.env("TAILGLOW_FAKE", "session2")
            .env("TAILGLOW_FAKE_RECORD", self.record())
            .env("TAILGLOW_STATE", self.state());
        c
    }

    /// Recorded non-query writes as hex strings, then clears the recording.
    /// Queries are `51 xx 00…` and `52 61/62 …`.
    fn take_writes(&self) -> Vec<String> {
        let path = self.record();
        if !path.exists() {
            return vec![];
        }
        let lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .filter(|l| {
                let is_query51 =
                    l.starts_with("51") && l[6..].bytes().all(|b| b == b'0' || b == b' ');
                let is_query52 = l.starts_with("52 61") || l.starts_with("52 62");
                !is_query51 && !is_query52
            })
            .map(str::to_string)
            .collect();
        std::fs::remove_file(path).unwrap();
        lines
    }
}

fn pad(head: &str) -> String {
    let n = head.split(' ').count();
    let len = if head.starts_with("52") { 65 } else { 21 };
    format!("{head}{}", " 00".repeat(len - n))
}

fn write_lines(effect: u8, brightness: u8, speed: u8, color: Option<[u8; 3]>) -> Vec<String> {
    let mut v = vec![pad(&format!("51 22 01 {effect:02x}"))];
    if effect != 0 {
        v.push(pad(&format!("51 23 01 {effect:02x} {brightness:02x}")));
        v.push(pad(&format!("51 27 01 {effect:02x} {speed:02x}")));
    }
    if let Some([r, g, b]) = color {
        v.push(pad(&format!(
            "51 28 01 {effect:02x} {r:02x} {g:02x} {b:02x}"
        )));
    }
    v
}

#[test]
fn info_shows_full_read_back() {
    let sb = Sandbox::new();
    sb.cmd()
        .arg("info")
        .assert()
        .success()
        .stdout(predicate::str::contains("Keychron M1 (3434:d035)"))
        .stdout(predicate::str::contains("[verified]"))
        .stdout(predicate::str::contains("Firmware: 1.0.0r-5"))
        .stdout(predicate::str::contains("Battery:  100%"))
        .stdout(predicate::str::contains(
            "Lighting: rainbow brightness 100% (255) speed 128",
        ))
        .stdout(predicate::str::contains(
            "DPI:      [400] 800 1600 3200 5000",
        ))
        .stdout(predicate::str::contains("Rate:     1000 Hz"))
        .stdout(predicate::str::contains(
            "Sensor:   lift-off 1 mm, ripple on, angle snap off, motion sync on, scroll forward",
        ))
        .stdout(predicate::str::contains("Debounce: 8 ms"))
        .stdout(predicate::str::contains("key1         Left-Click"))
        .stdout(predicate::str::contains("(all factory"));
    assert!(
        sb.take_writes().is_empty(),
        "info must not write to the mouse"
    );
    // Session-2 baseline: static green, stage 3 active.
    sb.cmd2()
        .arg("info")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Lighting: static #00ff00 brightness 100% (255) speed 128",
        ))
        .stdout(predicate::str::contains(
            "DPI:      400 800 [1600] 3200 5000  (active stage 3 = 1600 DPI, indicator blue)",
        ));
}

#[test]
fn list_with_fake() {
    let sb = Sandbox::new();
    sb.cmd()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("Keychron M1"));
}

#[test]
fn color_sends_the_vendor_apps_sequence() {
    let sb = Sandbox::new();
    sb.cmd()
        .args(["color", "ab3456"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "static #ab3456 brightness 100% (255) speed 128",
        ));
    // Same four reports as "EFFECT Static Light" with R=171 G=52 B=86 in session 1.
    assert_eq!(
        sb.take_writes(),
        vec![
            "51 22 01 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
            "51 23 01 01 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
            "51 27 01 01 80 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
            "51 28 01 01 ab 34 56 00 00 00 00 00 00 00 00 00 00 00 00 00 00",
        ]
    );
    // The state file records what the mouse now holds.
    let state = std::fs::read_to_string(sb.state()).unwrap();
    assert!(state.contains("effect = \"static\""), "{state}");
    assert!(state.contains("color = \"#ab3456\""), "{state}");
    assert!(state.contains("[dpi]"), "{state}");
}

#[test]
fn color_with_brightness_percent_and_effect() {
    let sb = Sandbox::new();
    sb.cmd()
        .args([
            "color",
            "#000cff",
            "--effect",
            "breathing",
            "--brightness",
            "0%",
        ])
        .assert()
        .success();
    assert_eq!(
        sb.take_writes(),
        write_lines(2, 0x0c, 0x80, Some([0x00, 0x0c, 0xff]))
    );
    sb.cmd()
        .args(["color", "00ff06", "--brightness", "255", "--speed", "128"])
        .assert()
        .success();
    assert_eq!(
        sb.take_writes(),
        write_lines(1, 0xff, 0x80, Some([0, 0xff, 6]))
    );
    // Brightness below the mouse's floor is clamped, never sent as-is.
    sb.cmd()
        .args(["color", "ffffff", "--brightness", "3"])
        .assert()
        .success();
    assert_eq!(
        sb.take_writes()[1],
        "51 23 01 01 0c 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00"
    );
    // Speed floor is the app's slider minimum 0x14; the session-2 values pass through.
    sb.cmd()
        .args([
            "color",
            "123456",
            "--effect",
            "breathing",
            "--brightness",
            "137",
            "--speed",
            "78",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("(~5.9 s period)"));
    assert_eq!(
        sb.take_writes(),
        write_lines(2, 0x89, 0x4e, Some([0x12, 0x34, 0x56]))
    );
    sb.cmd()
        .args(["effect", "rainbow", "--speed", "0"])
        .assert()
        .success();
    assert_eq!(sb.take_writes()[2], pad("51 27 01 03 14"));
}

#[test]
fn every_effect_matches_session1() {
    let sb = Sandbox::new();
    // The factory fake stores red for every color effect, like the mouse did
    // in session 1; `effect NAME` reuses the stored parameters.
    let cases = [
        ("off", 0u8, None),
        ("breathing", 2, Some([0xff, 0, 0])),
        ("rainbow", 3, None),
        ("rainbow-swirl", 4, None),
        ("snake", 5, Some([0xff, 0, 0])),
        ("christmas", 6, None),
        ("static", 1, Some([0xff, 0, 0])),
    ];
    for (name, id, color) in cases {
        sb.cmd().args(["effect", name]).assert().success();
        assert_eq!(
            sb.take_writes(),
            write_lines(id, 0xff, 0x80, color),
            "{name}"
        );
    }
    // Aliases and labels work too.
    sb.cmd()
        .args(["effect", "Rainbow Swirl"])
        .assert()
        .success();
    assert_eq!(sb.take_writes(), write_lines(4, 0xff, 0x80, None));
    sb.cmd()
        .args(["effect", "swirl", "--brightness", "50%"])
        .assert()
        .success();
    assert_eq!(sb.take_writes()[1][..14], *"51 23 01 04 86");
    // On the session-2 baseline the stored static color is green.
    sb.cmd2().args(["effect", "static"]).assert().success();
    assert_eq!(
        sb.take_writes(),
        write_lines(1, 0xff, 0x80, Some([0, 0xff, 0]))
    );
}

#[test]
fn rejects_bad_input_without_writing() {
    let sb = Sandbox::new();
    sb.cmd().args(["color", "red"]).assert().failure();
    sb.cmd().args(["color", "ff00"]).assert().failure();
    sb.cmd().args(["effect", "disco"]).assert().failure();
    sb.cmd()
        .args(["color", "ff0000", "--effect", "rainbow"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no color"));
    sb.cmd()
        .args(["color", "ff0000", "--brightness", "101%"])
        .assert()
        .failure();
    sb.cmd().args(["dpi", "set", "1", "800"]).assert().failure();
    sb.cmd().args(["dpi", "active", "6"]).assert().failure();
    sb.cmd().args(["rate", "250"]).assert().failure();
    sb.cmd().args(["debounce", "0"]).assert().failure();
    sb.cmd()
        .args(["buttons", "set", "key4", "teleport"])
        .assert()
        .failure();
    sb.cmd()
        .args(["reset", "factory"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--yes"));
    assert!(sb.take_writes().is_empty());
}

#[test]
fn dpi_rate_sensor_debounce_match_session2() {
    let sb = Sandbox::new();
    // 22:55:03 — stage 1 typed 450, confirm.
    sb.cmd()
        .args([
            "dpi", "set", "450", "800", "1600", "3200", "5000", "--active", "1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[450] 800 1600 3200 5000"));
    assert_eq!(
        sb.take_writes(),
        vec![pad("51 40 00 00 00 c2 01 20 03 40 06 80 0c 88 13 05")]
    );
    // 22:56:01 — stage 4 4900, active 4; typed values are floored to 50.
    sb.cmd()
        .args([
            "dpi", "set", "450", "800", "1600", "4920", "5000", "--active", "4",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("adjusted to 4900"));
    assert_eq!(
        sb.take_writes(),
        vec![pad("51 40 03 03 03 c2 01 20 03 40 06 24 13 88 13 05")]
    );
    // 22:57:51 — Reset DPI.
    sb.cmd().args(["dpi", "reset"]).assert().success();
    assert_eq!(
        sb.take_writes(),
        vec![pad("51 40 02 02 02 90 01 20 03 40 06 80 0c 88 13 05")]
    );
    // dpi active keeps the stages (session-2 baseline has stage 3 active).
    sb.cmd2().args(["dpi", "active", "5"]).assert().success();
    assert_eq!(
        sb.take_writes(),
        vec![pad("51 40 04 04 04 90 01 20 03 40 06 80 0c 88 13 05")]
    );
    // Report rate 22:56:20-27.
    for (hz, idx) in [("125", "00"), ("500", "01"), ("1000", "02")] {
        sb.cmd().args(["rate", hz]).assert().success();
        assert_eq!(
            sb.take_writes(),
            vec![pad(&format!("51 41 {idx} {idx} {idx} 7d 00 f4 01 e8 03"))]
        );
    }
    // Sensor 23:17:13 lift-off 2 mm (rest factory).
    sb.cmd()
        .args(["sensor", "--lift-off", "2"])
        .assert()
        .success();
    assert_eq!(sb.take_writes(), vec![pad("51 42 02 01 02 01 00 01")]);
    // 23:17:42 scroll reverse.
    sb.cmd()
        .args(["sensor", "--scroll", "reverse"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scroll reverse"));
    assert_eq!(sb.take_writes(), vec![pad("51 42 01 01 02 01 00 02")]);
    // 23:17:27 angle snap on.
    sb.cmd()
        .args(["sensor", "--angle-snap", "on"])
        .assert()
        .success();
    assert_eq!(sb.take_writes(), vec![pad("51 42 01 01 01 01 00 01")]);
    // Show only: no write.
    sb.cmd().arg("sensor").assert().success();
    assert!(sb.take_writes().is_empty());
    // Debounce 23:17:49.
    sb.cmd().args(["debounce", "9"]).assert().success();
    assert_eq!(sb.take_writes(), vec![pad("51 43 09")]);
    sb.cmd()
        .arg("debounce")
        .assert()
        .success()
        .stdout("debounce 8 ms\n");
    assert!(sb.take_writes().is_empty());
}

#[test]
fn buttons_match_session2_and_left_click_is_protected() {
    let sb = Sandbox::new();
    let cases = [
        ("right-click", "52 52 03 00 01 02"),
        ("consumer:player", "52 52 03 00 03 83 01"),
        ("light:effect", "52 52 03 00 06 01"),
        ("light:brightness-down", "52 52 03 00 06 05"),
        ("dpi:loop", "52 52 03 00 05 01"),
        ("dpi:down", "52 52 03 00 05 03"),
        ("consumer:volume-up", "52 52 03 00 03 e9"),
        ("consumer:stop", "52 52 03 00 03 b7"),
        ("system:screen-brightness-up", "52 52 03 00 08 0c 6f"),
        ("system:calculator", "52 52 03 00 08 0c 92 01"),
        ("system:website", "52 52 03 00 08 0c 23 02"),
        ("system:refresh", "52 52 03 00 08 07 00 3e"),
        ("system:switch-app", "52 52 03 00 08 07 04 2b 00 00 01"),
        ("system:paste", "52 52 03 00 08 07 01 19"),
        ("middle-click", "52 52 03 00 01 04"),
        ("backward", "52 52 03 00 01 10"),
        ("double-click", "52 52 03 00 01 80"),
        ("scroll-up", "52 52 03 00 01 00 02"),
        ("scroll-down", "52 52 03 00 01 00 fe"),
        ("scroll-left", "52 52 03 00 01 00 00 fe"),
        ("scroll-right", "52 52 03 00 01 00 00 02"),
        ("left-click", "52 52 03 00 01 01"),
        ("forward", "52 52 03 00 01 08"),
        ("disabled", "52 52 03 00 09"),
        ("key:ctrl+shift+k", "52 52 03 00 02 03 0e"),
        ("key:f7", "52 52 03 00 02 00 40"),
        (
            "autofire:left-click,37ms,5",
            "52 52 03 00 07 01 01 25 00 05",
        ),
        ("default", "52 52 03 00 00"),
    ];
    for (action, want) in cases {
        sb.cmd()
            .args(["buttons", "set", "key4", action])
            .assert()
            .success();
        assert_eq!(sb.take_writes(), vec![pad(want)], "{action}");
    }
    // "reset" = type 00; "reset --all" = 51 0b 01 01.
    sb.cmd()
        .args(["buttons", "reset", "key4"])
        .assert()
        .success();
    assert_eq!(sb.take_writes(), vec![pad("52 52 03 00 00")]);
    sb.cmd()
        .args(["buttons", "reset", "--all"])
        .assert()
        .success();
    assert_eq!(sb.take_writes(), vec![pad("51 0b 01 01")]);
    // Key 1 is the only left click on a factory mouse.
    sb.cmd()
        .args(["buttons", "set", "key1", "disabled"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only Left-Click"));
    assert!(sb.take_writes().is_empty());
    // Raw payloads are refused even though the parser accepts them.
    sb.cmd()
        .args(["buttons", "set", "key4", "raw:07:01 01 25 00 05"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("raw button payloads"));
    assert!(sb.take_writes().is_empty());
    sb.cmd()
        .args(["buttons", "actions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("consumer:volume-up"));
}

#[test]
fn macro_matches_session2() {
    let sb = Sandbox::new();
    sb.cmd()
        .args([
            "macro",
            "key4",
            "--loop",
            "times:3",
            "--fixed-delay",
            "25",
            "a+",
            "47ms",
            "a-",
            "3000ms",
            "b+",
            "32ms",
            "b-",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[experimental]"));
    assert_eq!(
        sb.take_writes(),
        vec![
            pad("52 54 03 00 22 01 03 00 19 00 07 00 81 04 00 00 0f 2f 00 00 01 04 00 00 0f b8 0b 00 81 05 00 00 0f 20 00 00 01 05 00 00"),
            pad("52 53 03 08 43 75 73 74 6f 6d 20 31"),
        ]
    );
    // The state file keeps the macro (the mouse cannot report it).
    let state = std::fs::read_to_string(sb.state()).unwrap();
    assert!(state.contains("[macros.key4]"), "{state}");
    assert!(
        state.contains("events = [\"a+\", \"47ms\", \"a-\", \"3000ms\", \"b+\", \"32ms\", \"b-\"]"),
        "{state}"
    );
    // Until-release / until-any-click headers.
    sb.cmd()
        .args(["macro", "key4", "--loop", "until-release", "a+", "a-"])
        .assert()
        .success();
    assert_eq!(sb.take_writes()[0][..23], *"52 54 03 00 0e 01 01 20");
    // Too long and bad steps are refused before any write.
    let mut long = vec!["macro", "key4"];
    long.extend(std::iter::repeat_n("a+", 14));
    sb.cmd()
        .args(&long)
        .assert()
        .failure()
        .stderr(predicate::str::contains("13"));
    sb.cmd().args(["macro", "key4", "a*"]).assert().failure();
    sb.cmd()
        .args(["macro", "key1", "a+", "a-"])
        .assert()
        .failure();
    assert!(sb.take_writes().is_empty());
}

#[test]
fn reset_needs_yes_and_sends_captured_bytes() {
    let sb = Sandbox::new();
    for (what, want) in [
        ("lighting", "51 0b 01 04"),
        ("buttons", "51 0b 01 01"),
        ("factory", "51 0b aa"),
    ] {
        sb.cmd()
            .args(["reset", what, "--yes"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("reset {what} done")));
        assert_eq!(sb.take_writes(), vec![pad(want)], "{what}");
    }
}

#[test]
fn export_import_round_trip() {
    let sb = Sandbox::new();
    let file = sb.dir.path().join("mouse.toml");
    sb.cmd2()
        .args(["export", file.to_str().unwrap()])
        .assert()
        .success();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("schema = 1"), "{text}");
    assert!(text.contains("model = \"Keychron M1\""), "{text}");
    assert!(text.contains("firmware = \"1.0.0r-5\""), "{text}");
    assert!(text.contains("effect = \"static\""), "{text}");
    assert!(text.contains("color = \"#00ff00\""), "{text}");
    assert!(
        text.contains("stages = [400, 800, 1600, 3200, 5000]"),
        "{text}"
    );
    assert!(text.contains("active = 2"), "{text}");
    assert!(text.contains("report_rate = 1000"), "{text}");
    assert!(text.contains("debounce_ms = 8"), "{text}");
    assert!(sb.take_writes().is_empty(), "export must not write");

    // Export to stdout too.
    sb.cmd2()
        .arg("export")
        .assert()
        .success()
        .stdout(predicate::str::contains("effect = \"static\""));

    // Importing into the factory fake writes only the differences:
    // lighting (rainbow → static green) and DPI (stage 1 → 3); rate, sensor,
    // debounce already match.
    sb.cmd()
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied 2 change(s)"));
    let mut want = write_lines(1, 0xff, 0x80, Some([0, 0xff, 0]));
    want.push(pad("51 40 02 02 02 90 01 20 03 40 06 80 0c 88 13 05"));
    assert_eq!(sb.take_writes(), want);
    // Importing into the matching fake writes nothing; --force writes all.
    sb.cmd2()
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied 0 change(s)"));
    assert!(sb.take_writes().is_empty());
    sb.cmd2()
        .args(["import", file.to_str().unwrap(), "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied 5 change(s)"));
    assert_eq!(sb.take_writes().len(), 4 + 1 + 1 + 1 + 1);

    // A partial file (buttons + macro) applies just those, in order.
    let partial = sb.dir.path().join("partial.toml");
    std::fs::write(
        &partial,
        "schema = 1\n[buttons]\nkey4 = \"consumer:volume-up\"\n[macros.key5]\nname = \"M\"\nevents = [\"a+\", \"a-\"]\n",
    )
    .unwrap();
    sb.cmd()
        .args(["import", partial.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied 2 change(s)"));
    let w = sb.take_writes();
    assert_eq!(w[0], pad("52 52 03 00 03 e9"));
    assert_eq!(w[1][..23], *"52 54 04 00 0e 01 01 00");
    assert_eq!(w[2][..14], *"52 53 04 01 4d");
}

#[test]
fn import_rejects_bad_files_without_writing() {
    let sb = Sandbox::new();
    let bad = sb.dir.path().join("bad.toml");
    std::fs::write(&bad, "schema = 9\n[lighting]\neffect = \"static\"\n").unwrap();
    sb.cmd()
        .args(["import", bad.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("schema 9"));
    std::fs::write(&bad, "schema = 1\n").unwrap();
    sb.cmd()
        .args(["import", bad.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no settings sections"));
    std::fs::write(
        &bad,
        "schema = 1\n[dpi]\nstages = [10, 800, 1600, 3200, 5000]\nactive = 0\n",
    )
    .unwrap();
    sb.cmd()
        .args(["import", bad.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("outside 100-26000"));
    sb.cmd()
        .args(["import", "/nonexistent/file.toml"])
        .assert()
        .failure();
    assert!(sb.take_writes().is_empty());
}

#[test]
fn watch_once_reapplies_saved_settings() {
    let sb = Sandbox::new();
    // Save a state file that differs from the factory fake in lighting only.
    std::fs::write(
        sb.state(),
        "schema = 1\n[lighting]\neffect = \"static\"\ncolor = \"#00ff00\"\n[dpi]\nstages = [400, 800, 1600, 3200, 5000]\nactive = 0\n",
    )
    .unwrap();
    sb.cmd()
        .args(["watch", "--once"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 change(s) re-applied"));
    assert_eq!(
        sb.take_writes(),
        write_lines(1, 0xff, 0x80, Some([0, 0xff, 0]))
    );
    // Nothing saved yet: no writes, still exits 0.
    let empty = Sandbox::new();
    empty
        .cmd()
        .args(["watch", "--once"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no saved settings yet"));
    assert!(empty.take_writes().is_empty());
}

#[test]
fn profile_is_read_only() {
    let sb = Sandbox::new();
    sb.cmd()
        .arg("profile")
        .assert()
        .success()
        .stdout(predicate::str::contains("on-board profile index: 0"))
        .stdout(predicate::str::contains("not supported"));
    assert!(sb.take_writes().is_empty());
}

#[test]
fn simulated_missing_mouse_and_permission_errors() {
    let sb = Sandbox::new();
    let mut c = Command::cargo_bin("tailglow").unwrap();
    c.env("TAILGLOW_FAKE", "none")
        .env("TAILGLOW_STATE", sb.state())
        .arg("info")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no supported mouse found"));
    let mut c = Command::cargo_bin("tailglow").unwrap();
    c.env("TAILGLOW_FAKE", "noperm")
        .env("TAILGLOW_STATE", sb.state())
        .args(["color", "ff0000"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("70-keychron-m1.rules"));
}

#[test]
fn fake_from_pcap_file() {
    let sb = Sandbox::new();
    let pcap = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/session1.pcap");
    let mut c = Command::cargo_bin("tailglow").unwrap();
    c.env("TAILGLOW_FAKE", pcap)
        .env("TAILGLOW_STATE", sb.state())
        .arg("info")
        .assert()
        .success()
        .stdout(predicate::str::contains("session1.pcap"))
        .stdout(predicate::str::contains(
            "DPI:      [400] 800 1600 3200 5000",
        ));
}
