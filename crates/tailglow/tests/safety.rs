// SPDX-License-Identifier: GPL-3.0-or-later
//! Safety and state behaviour added after the 2026-10-24 review:
//! `--device` identification, the debounce opt-in, read-back compare on
//! import, and macro fingerprints in state.toml.

use std::path::PathBuf;
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

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Session-2 baseline fake; every write recorded to writes.hex.
    fn cmd2(&self) -> Command {
        let mut c = Command::cargo_bin("tailglow").unwrap();
        c.env("TAILGLOW_FAKE", "session2")
            .env("TAILGLOW_FAKE_RECORD", self.path("writes.hex"))
            .env("TAILGLOW_STATE", self.path("state.toml"));
        c
    }

    /// Non-query writes recorded so far (then cleared).
    fn take_writes(&self) -> Vec<String> {
        let path = self.path("writes.hex");
        if !path.exists() {
            return vec![];
        }
        let lines = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .filter(|l| {
                let q51 = l.starts_with("51") && l[6..].bytes().all(|b| b == b'0' || b == b' ');
                let q52 = l.starts_with("52 61") || l.starts_with("52 62");
                !q51 && !q52
            })
            .map(str::to_string)
            .collect();
        std::fs::remove_file(path).unwrap();
        lines
    }
}

// hidraw paths and their error wording exist only on Linux; other platforms open
// devices through hidapi and refuse unknown paths with their own message.
#[cfg(target_os = "linux")]
#[test]
fn device_flag_refuses_nodes_that_are_not_a_vendor_interface() {
    let sb = Sandbox::new();
    for (path, why) in [
        ("/dev/null", "not a hidraw node"),
        ("/dev/hidraw-nope", "not a hidraw node"),
        ("/dev/hidraw999", "went away"),
    ] {
        let mut c = Command::cargo_bin("tailglow").unwrap();
        c.env_remove("TAILGLOW_FAKE")
            .env("TAILGLOW_STATE", sb.path("state.toml"))
            .args(["--device", path, "info"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(why));
    }
}

#[test]
fn debounce_outside_8_to_11_needs_allow_untested() {
    let sb = Sandbox::new();
    sb.cmd2()
        .args(["debounce", "12"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("never captured"))
        .stderr(predicate::str::contains("--allow-untested"));
    assert!(sb.take_writes().is_empty(), "nothing may be sent");
    sb.cmd2().args(["debounce", "11"]).assert().success();
    assert_eq!(sb.take_writes().len(), 1);
    sb.cmd2()
        .args(["--allow-untested", "debounce", "12"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: debounce 12 ms was never captured",
        ))
        .stdout("debounce 12 ms\n");
    let w = sb.take_writes();
    assert_eq!(w.len(), 1);
    assert!(w[0].starts_with("51 43 0c"), "{w:?}");
    sb.cmd2()
        .args(["--allow-untested", "debounce", "31"])
        .assert()
        .failure();
    // A settings file asking for 12 ms is refused the same way.
    let file = sb.path("deb.toml");
    std::fs::write(
        &file,
        "schema = 1\n[pointer]\nreport_rate = 1000\nlift_off_mm = 1\nripple = true\nangle_snap = false\nmotion_sync = true\nscroll_reverse = false\ndebounce_ms = 12\n",
    )
    .unwrap();
    sb.cmd2()
        .arg("import")
        .arg(&file)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--allow-untested"));
}

#[test]
fn import_remembers_macros_and_does_not_resend_them() {
    let sb = Sandbox::new();
    let file = sb.path("macro.toml");
    std::fs::write(
        &file,
        "schema = 1\n[lighting]\neffect = \"static\"\ncolor = \"#00ff00\"\n\n[macros.key6]\nname = \"Custom 1\"\nloop = \"times:3\"\nfixed_delay_ms = 25\nevents = [\"a+\", \"47ms\", \"a-\"]\n",
    )
    .unwrap();
    sb.cmd2()
        .arg("import")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "applied 1 change(s) (2 report(s))",
        ));
    let w = sb.take_writes();
    assert_eq!(
        w.len(),
        2,
        "lighting already matches; macro = 0x54 + 0x53: {w:?}"
    );
    assert!(
        w[0].starts_with("52 54 05") && w[1].starts_with("52 53 05"),
        "{w:?}"
    );
    let state = std::fs::read_to_string(sb.path("state.toml")).unwrap();
    assert!(state.contains("[macros.key6]"), "{state}");
    assert!(
        state.contains("[sent_macros]") && state.contains("key6 = \"fnv1a64:"),
        "{state}"
    );

    // Second import (as `watch` does on every plug): nothing is sent.
    sb.cmd2()
        .arg("import")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("applied 0 change(s)"))
        .stdout(predicate::str::contains("1 macro(s) already on the mouse"));
    assert!(sb.take_writes().is_empty());

    // --force re-sends it.
    sb.cmd2()
        .arg("import")
        .arg(&file)
        .arg("--force")
        .assert()
        .success();
    assert!(sb.take_writes().iter().any(|l| l.starts_with("52 54 05")));

    // Remapping key6 forgets the macro and its fingerprint.
    sb.cmd2()
        .args(["buttons", "set", "key6", "middle-click"])
        .assert()
        .success();
    let state = std::fs::read_to_string(sb.path("state.toml")).unwrap();
    assert!(!state.contains("sent_macros"), "{state}");
}

#[test]
fn export_does_not_leak_state_only_fields() {
    let sb = Sandbox::new();
    sb.cmd2()
        .args(["macro", "key6", "a+", "47ms", "a-"])
        .assert()
        .success();
    let out = sb.cmd2().arg("export").assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("[macros.key6]"), "{text}");
    assert!(!text.contains("sent_macros"), "{text}");
}
