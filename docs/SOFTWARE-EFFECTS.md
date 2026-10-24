# Software-driven lighting effects (deferred)

Design note for the "live effects" feature: colouring the mouse from the host,
for example hue following the time of day or CPU temperature. It is **not
built** and this note records why, what would unblock it, and the intended
shape.

## Why it is not built yet

Every lighting change is written to the mouse immediately. The captures show
no save, commit or flash opcode anywhere (`docs/PROTOCOL.md`, section 9,
verified), and settings persist across app restarts and reconnects
(verified). What is **unknown** is whether each `0x22`-`0x28` write lands in
flash (or EEPROM) or only in RAM that happens to survive while the mouse is
powered.

If writes are flash-backed, a live effect at even 1 Hz is 86 400 writes a
day, and small flash parts are typically rated for 10^4-10^5 erase cycles per
sector. That could wear out the mouse's settings memory in weeks. The vendor
app never streams (verified: slider drags produce one write on release, and
the most write-heavy action is typing in an RGB field, one 4-report burst per
keystroke), so nothing in the captures tells us the mouse tolerates streaming.

The deciding experiment is simple: set a non-factory state, power-cycle the
mouse (unplug and replug), read it back. It needs physical access and is
scheduled; it has not been run.

## Decision rule

- **Settings survive a power cycle:** the writes are persistent, so assume
  flash. Live effects are a wear risk. If built at all: debounce every change,
  cap writes hard (at most one write per 10 s, with a daily budget, for
  example 2 000 writes), count them, and make the feature opt-in with the risk
  stated in the UI. Even so, prefer "slow" sources (time of day) over "fast"
  ones (CPU load).
- **Settings do not survive a power cycle:** the writes are RAM-only and the
  only cost is USB traffic. Live effects at 1-5 Hz are safe; the GUI's
  existing 100 ms minimum spacing is enough of a limit.
- Either way, keep the vendor app's ordering rule for a lighting write
  (effect, brightness, speed, then colour for static/breathing/snake), and
  never send anything outside the verified command set.

## Proposed design

- `tailglow live --source clock|cpu-temp [--rate HZ] [--max-writes-per-day N]`
  as a foreground loop (same shape as `tailglow watch`), plus a GUI toggle on
  the Lighting page.
- Sources:
  - **clock**: hue = time of day mapped around the colour wheel (midnight red,
    noon cyan, or a user-set palette); changes slowly, few writes.
  - **cpu-temp**: read `/sys/class/hwmon/hwmon*/temp*_input` (pick the first
    `coretemp` / `k10temp` / `zenpower` device, overridable), map a
    temperature range (for example 40-90 C) to green-yellow-red. Only write
    when the mapped colour differs from the last written one by more than a
    threshold, so an idle machine produces no writes at all.
- Write cap and counter: a single rate-limiter shared with the GUI's
  `WRITE_INTERVAL`, a per-day counter persisted in the state file, and both
  values shown on the About / diagnostics page ("live effect: 37 writes
  today, limit 2 000"). Refuse to start when the budget is spent.
- Static effect only: a live effect writes `static` with the computed colour
  so the mouse itself does no animation; its own breathing/rainbow effects
  are the better choice when the user wants motion.
- Always restore: remember the lighting state read from the mouse before the
  loop starts (effect, brightness, speed, colour of that effect) and write it
  back on exit, including on SIGINT/SIGTERM and when the mouse disappears and
  comes back (reuse the `watch` detection loop).
- Respect model status: *likely* models need `--allow-untested` like every
  other write; *unsupported* models are never touched.

Until the power-cycle test is done, this stays a document.
