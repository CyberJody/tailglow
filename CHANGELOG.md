# Changelog

## 0.1.0

First public release. Tested on Linux with a Keychron M1 over its USB cable
(firmware 1.0.0r-5): every setting below was written, read back from the
mouse, and lighting was checked with a webcam. The other models in the
README's table share the vendor app and interface, so they are *likely* to
work but nobody has tested one yet: reads work, writes need
`--allow-untested`. Please report how your mouse does.

What works:

- Lighting: 7 effects (off, static, breathing, rainbow, rainbow-swirl, snake,
  christmas), brightness, speed and colour, all read back from the mouse.
- DPI: 5 stages from 100 to 26000 in steps of 50, and the active stage.
- Report rate: 125, 500 or 1000 Hz.
- Sensor options: lift-off distance, ripple control, angle snapping, motion
  sync, scroll direction; button debounce time.
- Button remapping for every key and the scroll wheel (mouse buttons, keyboard
  shortcuts, media keys, DPI and lighting controls, auto-fire, disable), with
  a guard that keeps at least one Left-Click.
- Macros (experimental): short macros of up to 13 steps.
- Resets: buttons, lighting, or factory.
- Settings files: export and import a TOML file; import writes only what
  differs and checks the result.
- Apply-on-plug: `tailglow watch` (systemd user unit) re-applies your last
  settings when the mouse is plugged in.
- Desktop app (`tailglow-gui`) with Lighting, DPI, Buttons, Settings and About
  pages, light and dark themes that follow the desktop.
- Live lighting preview on an original 3D render of the M1 (3/4 and top views):
  every effect animates on the mouse's real light zones in your colour,
  brightness and speed, with soft glow on the background. Runs on the CPU, no
  GPU needed.
- Tray icon (`tailglow-tray`) with battery, connection, quick lighting and DPI
  menu, low-battery notification and per-application profiles (X11).
- Bluetooth: connection and battery level, read-only, from BlueZ; old
  pairings can be listed and removed. Keychron's firmware takes no settings
  over Bluetooth, so changing settings needs the USB cable.
- Safety: only commands seen from the vendor app are ever sent; no firmware
  updates; unknown and unsupported models are never opened.
- Packaging: udev rule, systemd user units, desktop entries, icon, AppStream
  metainfo and a `.deb` built with cargo-deb. A Flatpak manifest is included
  but untested.

Not supported: on-board profile switching, the key-pairing combination,
firmware updates, and the 8K / Keychron Launcher mice (a different protocol).
