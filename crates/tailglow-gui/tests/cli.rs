// SPDX-License-Identifier: GPL-3.0-or-later
//! `tailglow-gui --help` / `--version` print and exit without opening a
//! window. The display variables are cleared, so a window attempt would fail
//! (no display) instead of passing; the timeout catches a hang.

use std::time::Duration;

use assert_cmd::Command;

fn gui() -> Command {
    let mut c = Command::cargo_bin("tailglow-gui").unwrap();
    c.env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("TAILGLOW_FAKE", "none")
        .env("TAILGLOW_GUI_CONFIG", "/nonexistent/tailglow-gui-test.toml")
        .timeout(Duration::from_secs(10));
    c
}

#[test]
fn help_prints_usage_and_exits() {
    for flag in ["--help", "-h"] {
        let out = gui().arg(flag).assert().success().get_output().clone();
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.starts_with("tailglow-gui: desktop app"), "{stdout}");
        assert!(stdout.contains("--theme <MODE>"), "{stdout}");
        assert!(stdout.contains("--screenshot <DIR>"), "{stdout}");
        assert!(
            out.stderr.is_empty(),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Help wins even after other flags.
    gui().args(["--theme", "dark", "--help"]).assert().success();
}

#[test]
fn version_prints_and_exits() {
    for flag in ["--version", "-V"] {
        let out = gui().arg(flag).assert().success().get_output().clone();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("tailglow-gui {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn bad_arguments_fail_with_a_hint_and_no_window() {
    let out = gui().arg("--bogus").assert().code(2).get_output().clone();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("unexpected argument \"--bogus\""),
        "{stderr}"
    );
    assert!(stderr.contains("tailglow-gui --help"), "{stderr}");
    gui().args(["--theme", "purple"]).assert().code(2);
}
