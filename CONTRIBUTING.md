# Contributing

## License

Tailglow is licensed GPL-3.0-or-later (`LICENSE`). Contributions are accepted
under the same license (inbound = outbound): by opening a pull request you
agree that your change is distributed under GPL-3.0-or-later. Start every new
source file with

    // SPDX-License-Identifier: GPL-3.0-or-later

(`#` instead of `//` in shell, Python and config files). New dependencies must
be GPL-3-compatible; run `scripts/check-licenses.py` after adding one. Bug
reports, capture files and test results are welcome at any time; the
[mouse report form](https://github.com/CyberJody/tailglow/issues/new?template=mouse-report.yml)
is the quickest way to say whether your mouse works.

## Clean-room rule

All protocol knowledge in this repository comes from **our own USB captures**
of the vendor's Windows app (`testdata/`, `docs/PROTOCOL.md`). Keep it that
way:

- Do not copy code from Keychron's software, nor disassemble it.
- Do not copy code from other open-source Keychron tools either. `chron_tui`
  (LGPL-3.0) and `keychron-tray-rs` (GPL-3.0) would be license-compatible,
  but both target the 8K/Launcher protocol family, a different protocol, and
  this project's protocol knowledge has to stay traceable to our own
  captures. Reading them to learn how that other protocol works is fine;
  bringing code or byte tables across is not.
- A new command, field or value enters the code base only with a capture (or
  a hardware read-back) behind it. Never invent opcodes.

## Evidence tags

Every protocol claim, in docs and in code comments, carries one of:

- **verified**: seen on the wire *and* its effect confirmed (vendor UI,
  read-back from the mouse, or webcam for lighting);
- **likely**: consistent with every capture, but one interpretation step
  remains untested;
- **unknown**: seen, meaning not understood. Document it, do not use it.

The device table (`crates/tailglow-core/src/device.rs`) uses the same idea:
`SupportStatus::Verified` / `Likely` / `Unsupported`, each with an `evidence`
string saying where the proof is. Writes to *likely* models are gated behind
`--allow-untested`; *unsupported* models are never opened (`tailglow_hid::vet`
checks VID:PID and the USB interface from OS metadata before any open,
including `--device PATH`). The same opt-in gates values that were never
captured (debounce outside 8-11 ms).

## Tests

- Tests pin bytes to captures. Protocol tests assert the exact report bytes
  the vendor app sent (from `testdata/session1.pcap` and
  `testdata/session2.pcap`) and the exact replies the mouse gave, through the
  `FakeMouse` transport in `tailglow-hid`. A test that checks "some bytes were
  written" is not enough; it must check *which* bytes.
- Before opening a PR:

      cargo fmt --all
      cargo clippy --workspace --all-targets -- -D warnings
      cargo test --workspace

  CI runs the same on Linux, Windows and macOS (`.github/workflows/ci.yml`).

- Hardware test (`scripts/hw-test.sh`, Linux, M1 only so far): takes the
  a mouse lock (`flock` on `$XDG_RUNTIME_DIR/tailglow-mouse.lock`, or
  `TAILGLOW_LOCK`, so two runs cannot interleave writes), reads everything, sets and reads back every
  writable setting (DPI, rate, sensor options, debounce, two button remaps, a
  macro, the left-click guard), checks static green / red / off / breathing /
  rainbow with the webcam (`scripts/cam_check.py`, skip with
  `TAILGLOW_NO_CAM=1`), restores the M1 baseline values
  (`scripts/compare_baseline.py`) and finishes on Rainbow. The run log and
  before/after dumps go to `hw-test-out/`.
  Run it after any change to `tailglow-core` or `tailglow-hid` if you have the
  hardware; say so in the PR if you do not.

## Code conventions

- Names: the project is **Tailglow** in prose and UI text; crates and
  binaries are `tailglow`, `tailglow-gui`, `tailglow-tray`, `tailglow-core`,
  `tailglow-hid`; the app id is `io.github.cyberjody.Tailglow`; environment
  variables start with `TAILGLOW_`; files live in `~/.config/tailglow`
  (`tailglow_hid::paths`).
- Bluetooth is status only and read-only. The BlueZ code
  (`crates/tailglow-hid/src/bluez.rs`) may read properties and listen for
  signals; the only method it calls besides `GetManagedObjects` is
  `Adapter1.RemoveDevice`, after an explicit user confirm. Never add GATT
  reads/writes, `Pair`, `Connect`, `Disconnect` or `Properties.Set`: the
  M1 has a firmware-update characteristic. The `only_allowed_methods_in_source`
  test and the fake-BlueZ tests enforce this; test against the fake BlueZ on
  a private bus, never the system bus.
- Value ranges live in `tailglow-core` (DPI 100-26000 step 50, brightness
  12-255, speed 20-255, ...) and come from the vendor app's own limits. Do not
  widen them without a capture showing the app sending the wider value.
- Every write is followed by a read-back and a comparison. Keep it that way
  for new commands; if a command has no readable counterpart (macros,
  key-pairing), say so in the docs and mark the feature experimental.
- Plain English, no emojis, in docs and commit messages.

## Commits

One topic per commit, present tense, prefixed with the area:

    core: parse 0x07 slot bytes (likely) and expose report rate
    cli: `dpi reset` sends the vendor default table
    docs: ADD-YOUR-MOUSE capture guide

Reference the evidence when a commit changes protocol behaviour (capture
file and timestamp, or hardware test run).
