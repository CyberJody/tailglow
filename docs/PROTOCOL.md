> Protocol notes as of 2026-10-24. Some evidence files cited below (`captures/...`: full decode, app screenshots, webcam series) are the original capture notes and are not shipped; the two captures themselves are `testdata/session1.pcap` and `testdata/session2.pcap` (with their action logs), and `scripts/decode.py` is described in `ADD-YOUR-MOUSE.md`.

# Keychron M1 protocol (wired, USB 3434:d035)

Clean-room notes from watching USB traffic. **Keychron Mouse 1.2.1.2** for Windows
(SHA-256 `2f4cc9b8…246a2e`, unsigned Inno Setup installer) ran in a Windows VM
with the mouse passed through; the Linux host captured the traffic with usbmon.
Mouse firmware: `1.0.0r-5`.

Raw evidence (decode with `scripts/decode.py <pcap> <actions.log> 13`):
- `testdata/session1.pcap` + `testdata/session1-actions.log`: lighting (2026-10-23 22:01-22:09)
- `testdata/session2.pcap` + `testdata/session2-actions.log`: every app page (22:34-23:34, 2504 packets, 0 dropped);
  full decode in `captures/session2-decoded.txt`; UI screenshots in `captures/ui/`; webcam series in `captures/cam/`.

Tags: **verified** = seen on the wire and the effect confirmed (UI, read-back or webcam);
**likely** = consistent with all captures but one interpretation step remains; **unknown** = seen, meaning not known.

## 1. Transport (verified)
- USB interface **2** (Linux: the hidraw node whose parent interface has bInterfaceNumber 2).
- HID **feature reports**, two report IDs:
  - **0x51**, 21 bytes: `51 <cmd> <args…>` zero padded. Settings, lighting, reads.
  - **0x52**, 65 bytes: `52 <cmd> <args…>` zero padded. Button map, macros, profile blob.
- Write = SET_REPORT(feature) (`HIDIOCSFEATURE`). Read = SET the command with zero args, then
  GET_REPORT(feature) of the same report ID (`HIDIOCGFEATURE`); the reply echoes `<id> <cmd>`.
- The app waits for each write to complete; consecutive writes are ~5-6 ms apart. No interrupt-OUT traffic.
- No ACK/status byte is read after writes. No checksum.

## 2. Commands seen (complete list for this app version)

| report | cmd | dir | purpose | status |
|---|---|---|---|---|
| 51 | 0x04 | read | firmware version string | verified |
| 51 | 0x06 | read | device info + battery | verified |
| 51 | 0x07 | read | DPI / report rate / sensor settings / debounce | verified (some bits likely) |
| 51 | 0x0b | write | reset: `01 01` buttons, `01 04` lighting, `aa` factory | verified |
| 51 | 0x12 | read | current lighting effect | verified |
| 51 | 0x13 | read | brightness per effect | verified |
| 51 | 0x14/0x15/0x16 | read | red / green / blue per effect | verified |
| 51 | 0x17 | read | speed per effect | verified |
| 51 | 0x22 | write | select effect | verified |
| 51 | 0x23 | write | brightness | verified |
| 51 | 0x27 | write | speed | verified |
| 51 | 0x28 | write | color | verified |
| 51 | 0x36 | write | sent once at every app launch, zero args, never read | unknown |
| 51 | 0x40 | write | DPI stages + active stage | verified |
| 51 | 0x41 | write | report rate | verified |
| 51 | 0x42 | write | lift-off, ripple, angle snap, motion sync, scroll direction | verified |
| 51 | 0x43 | write | debounce time | verified |
| 51 | 0x46 | write | key-pairing combination | likely |
| 52 | 0x52 | write | assign one button | verified |
| 52 | 0x53 | write | macro name for a button | verified |
| 52 | 0x54 | write | macro events for a button | verified (layout partly likely) |
| 52 | 0x57 | write | profile switch (bulk profile blob) | likely |
| 52 | 0x61 | read | which buttons are remapped | verified |
| 52 | 0x62 | read | one button's mapping | verified |

Never seen: any firmware-update, bootloader, flash-erase or "save/commit" command.

## 3. Launch sequence (verified, identical on 6 launches)
`51 06`(R) → `51 07`(R) → `51 36`(W, no read) → `52 61`(R) → for each flagged key `52 62 <key>`(R) →
`51 12` → `51 13` → `51 17` → `51 14` → `51 15` → `51 16`(all R) → `51 04`(R).
Opening pages or switching tabs sends nothing. After a lighting reset the app re-reads 0x12-0x16.

## 4. Reads

### 0x06 device info (verified)
`51 06 01 00 34 34 35 d0 00 01 00 64 02` → bytes 4-5 VID LE (3434), 6-7 PID LE (d035), byte 11 battery % (0x64 = 100).
Bytes 2, 3, 8-10, 12 unknown (constant).

### 0x04 firmware (verified)
`51 04 09 "1.0.0r-5"` → byte 2 = string length, then ASCII.

### 0x07 pointer settings (verified unless marked)
```
51 07 PP S1 S2 S3 d1lo d1hi … d5lo d5hi FF NN DB
```
- `PP` profile index, always 00 here (likely).
- `S1 S2 S3` three "slot" bytes, one per connection mode (likely; this mouse was only used wired):
  - bits 0-3 = active DPI stage index (0-4): verified for S1 (factory 0x60 → stage 0 / 400 shown in UI;
    0x22 → stage 2 / 1600; 0x13 → stage 3 / 3200);
  - bits 4-5 = report-rate index 0=125, 1=500, 2=1000 Hz (verified for S1: 0x13 with 500 Hz, 0x22 with 1000 Hz);
  - bit 6 = set in the factory state (S1 0x60, S3 0x40), cleared by the first 0x40 write (likely "default" flag);
  - S3 never takes the rate bits (0x40/0x41 writes left it 0x02/0x03): likely a mode without selectable rate.
- `d1..d5` DPI stages, LE16 (verified).
- `FF` sensor flags (verified by toggling single settings across 4 relaunches: 0x35 factory, 0x4a, 0x59, 0x1d, 0x15):
  bits 0-1 lift-off (1 = 1 mm, 2 = 2 mm), bit 2 ripple on, bit 3 angle snap on, bit 4 motion sync on,
  bit 5 set only in the factory state (cleared by any 0x42 write; likely the 0x42 byte-6 field the app sends as 00),
  bit 6 scroll reverse.
- `NN` = 05, number of DPI stages (likely; never changed, the app has no add/remove).
- `DB` debounce ms (verified: 08 → 0b after three "+").

### 0x12-0x17 lighting state (verified)
- 0x12: `51 12 01 06 EE ff ff cf cf cd cd cf cd` → `EE` current effect id. `01 06` and the 8 trailing bytes never changed (unknown; `06` may be the number of effects).
- 0x13 brightness[6], 0x17 speed[6], 0x14 R[6], 0x15 G[6], 0x16 B[6]: one byte per effect, index = effect id - 1
  (Static, Breathing, Rainbow, Swirl, Snake, Christmas). Example after setting breathing to 12 34 56,
  brightness 0x89, speed 0x4e: 0x13 = `ff 89 ff…`, 0x17 = `80 4e 80…`, 0x14 = `ff 12 ff…`, 0x15 = `00 34 …`, 0x16 = `00 56 …`.
- The device stores parameters per effect. The app keeps its own copy too and sends its copy (not the
  read-back) on every change, so the two can disagree (it sent red for Static while the device held green).

### 0x52/0x61 remap flags, 0x52/0x62 one mapping (verified)
- `52 61` reply: bytes 2..10 = one byte per key 0..8, 01 = remapped. All zero = factory map.
- `52 62 KK` reply: `52 62 KK 00 <type> <payload…>` in the same format as the 0x52 write (below).
  Seen: key 4 → `52 62 04 00 01 04` (Middle-Click).

### Factory state (verified: identical in session-1 first launch and after `51 0b aa`)
0x07 `00 60 60 40 90 01 20 03 40 06 80 0c 88 13 35 05 08`, 0x12 effect 03 (Rainbow), brightness all ff,
speed all 80, colors all ff 00 00 (red), `52 61` all zero.

## 5. Lighting writes (verified)
`51 <cmd> 01 <effect> <args>`. Byte 2 is always 01 (meaning unknown: write flag or profile 1).

| cmd | args | meaning |
|---|---|---|
| 0x22 | — | select effect (alone for Lights Off) |
| 0x23 | `BB` | brightness; app slider range 0x0c/0x14…0xff, linear in slider position |
| 0x27 | `SS` | speed; app slider 0x14 (min) … 0x89 (mid) … 0xff (max); default 0x80 |
| 0x28 | `RR GG BB` | color, only for effects 1, 2, 5 |

Effect ids: 0 off · 1 static · 2 breathing · 3 rainbow · 4 rainbow swirl · 5 snake · 6 christmas.

**Ordering rule (verified):** on *every* lighting change (effect, brightness, speed, swatch, RGB field, hue,
saturation) the app sends the full sequence 0x22 → 0x23 → 0x27 (→ 0x28 for effects 1/2/5), even when only
one value changed. Static and Rainbow-family effects still get 0x27 (static sends the stored 0x80).

App-side color handling (verified): saturation and hue are mixed into RGB by the app; the device only sees RGB.
Swatches: red ff0000, magenta f000ff, blue 000cff, cyan 00fffc, green 00ff06, yellow ffde00, orange ff4800,
white ffffff. Typing in an R/G/B field sends a full sequence **per keystroke** (typing "18" sends 01 then 12).

**Webcam timing** (C920, mean RGB of the mouse area, `captures/cam/SUMMARY.md`; likely, ±1 frame):

| effect | speed 0x14 | 0x4e | 0x80/0x89 | 0xc3 | 0xff |
|---|---|---|---|---|---|
| breathing period | 7.25 s | 5.9 s | 3.75 s | 1.8 s | 0.7 s |
| rainbow color cycle | 7.4 s | | 3.8 s | | 1.3 s (may alias) |
| rainbow swirl | 7.5 s | | 3.9 s | | 0.7 s |
| snake (brightness ripple) | 2.75 s | | 1.4 s | | 0.2 s (aliased) |
| christmas (R/G alternation) | 3.75 s | | 1.9 s | | ≤0.2 s (aliased) |

Lights Off: frame-to-frame change 0.12 (flat); Rainbow: 3.6-6.2.

## 6. Pointer writes

### 0x40 DPI (verified)
`51 40 A A A d1lo d1hi d2lo d2hi d3lo d3hi d4lo d4hi d5lo d5hi 05`
- `A` = active stage index, repeated 3× (one per slot). All five stages + count 05 are always sent.
- Range 100-26000 (slider ends: 0x0064, 0x6590). Typed values are floored to a multiple of 50 (1234 → 1200, 450 kept); 0 → 100.
- Sent only when **Confirm** is clicked (editing a stage field or clicking the slider sends nothing).
- "Reset DPI" sends a normal 0x40 with `02 02 02` 400/800/1600/3200/5000 05 (no reset opcode).
- Stage indicator colors (white/green/blue/yellow/red) are fixed in the app; clicking them does nothing. No DPI-lighting option exists.

### 0x41 report rate (verified)
`51 41 R R R 7d 00 f4 01 e8 03` → `R` = index (0 = 125, 1 = 500, 2 = 1000 Hz) ×3, then the rate table LE16. Sent immediately on click.

### 0x42 sensor / scroll settings (verified)
`51 42 LO RP AS MS 00 SD` — each 1 = first option, 2 = second:
`LO` lift-off 1 = 1 mm / 2 = 2 mm, `RP` ripple 1 = on / 2 = off, `AS` angle snap 1 = on / 2 = off,
`MS` motion sync 1 = on / 2 = off, byte 6 always 00 (unknown), `SD` scroll 1 = forward / 2 = reverse.
All fields are sent on every click.

### 0x43 debounce (verified)
`51 43 MS` debounce in ms (8 default; "+"/"−" step 1).

### 0x46 key-pairing combination (likely)
`51 46 MASK 03` → `MASK` HID button bits (LBtn 01, MBtn 04, Bt4 08, Bt5 10): LBtn+Bt4 = 09, LBtn+MBtn+Bt5 = 15.
Second byte always 03 (likely the 3-second hold in the UI text). Sent on **Save**. Not readable by any app read.

## 7. Buttons (report 0x52)

### 0x52 assign (verified)
`52 52 KK 00 TT <payload>` → `KK` key index 0-8 in the app's list order Key1 … Key7, Scr Up, Scr Dn
(verified for Key 4 = 03 and Key 5 = 04; the rest likely).
Sent immediately on selection (Keyboard and Game Enhancement on their **Save**).

| TT | category | payload | examples |
|---|---|---|---|
| 00 | default for that key | — | "reset current button" sends `52 52 KK` + zeros |
| 01 | mouse | `BTN WHEEL HWHEEL` | Left 01, Right 02, Middle 04, "Forward" 08, "Backward" 10, Double-Click 80; Scroll Up `00 02`, Down `00 fe`, Left `00 00 fe`, Right `00 00 02` |
| 02 | keyboard | `MOD KEY` (HID modifier bits, usage) | Ctrl+Shift+K `03 0e`, F7 `00 40` |
| 03 | consumer | usage LE16 | Vol+ e9, Vol− ea, Mute e2, Player 0183, Play/Pause cd, Prev b6, Next b5, Stop b7 |
| 05 | DPI | 01 loop, 02 DPI+, 03 DPI− | |
| 06 | lighting | 01 effect, 02 speed, 03 color, 04 brightness+, 05 brightness− | |
| 07 | game enhancement (auto-fire) | `SRC VAL RATE_LE16 TIMES` | mouse(01) left(01), 37 ms, ×5 → `01 01 25 00 05` (keyboard source not captured) |
| 08 | system shortcut | `0c USAGE_LE16` (consumer) or `07 MOD KEY` (keyboard) | Screen Bright+ `0c 6f`, − `0c 70`, Calculator `0c 92 01`, My Computer `0c 94 01`, Website `0c 23 02`, Mail `0c 8a 01`, Refresh `07 00 3e`, Copy `07 01 06`, Cut `07 01 1b`, Paste `07 01 19`, Switch App `07 04 2b 00 00 01` (trailing `00 00 01` unknown) |
| 09 | disabled | — | |
| (macro) | see 0x54 | | |

Mouse-button bit names follow the app's labels ("Forward" = 0x08 = HID button 4). The app locks Key 1
(Left-Click) by default; never remapped here.

### 0x53 macro name, 0x54 macro events (verified bytes, layout likely)
Assigning a macro to a key sends **0x54 then 0x53** (no 0x52):
```
52 54 KK 00 LEN  H0 LC LM DLlo DLhi Nlo Nhi  <events…>
52 53 KK NAMELEN "Custom 1"
```
Observed header for a 7-event macro (A down, 47 ms, A up, 3000 ms, B down, 32 ms, B up), LEN = 0x22:
- Loop Times 3 + Fixed Delay 25 ms: `01 03 00 19 00 07 00`
- Loops Until Stop Pressing + No Delay: `01 01 20 00 00 07 00`
- Loops Until Any Click + Record Delay: `01 01 40 00 00 07 00`
→ `LC` loop count, `LM` loop mode (00 times, 20 until release, 40 until any click), `DL` fixed delay ms LE16
(0 when not fixed), `N` event count LE16 (likely). "No Delay" vs "Record Delay" are not distinguished in the header (unknown).
Events, 4 bytes each: key down `81 <usage> 00 00`, key up `01 <usage> 00 00`, delay `0f <ms LE24>` (verified values).
LEN 0x22 = 34 vs 35 payload bytes counted after it (exact LEN rule unknown). One chunk was enough for 7 events;
multi-chunk layout for long macros (offset byte = 00 here) not captured.
Editing or deleting a macro in the Macro Editor sends nothing; the device keeps it until the key is reassigned.

### Resets (verified)
- "Reset current button" → `52 52 KK` + zeros (type 00).
- "Reset all button functions" → `51 0b 01 01`.
- "Reset light data" → `51 0b 01 04`, then the app re-reads 0x12-0x16 (device returns factory lighting: Rainbow).
- "Restore Factory Settings" → `51 0b aa`. No re-read and no USB re-enumeration; next launch reads the factory state above.

## 8. Profiles (likely)
Five fixed app profiles (no rename/copy; Import/Export are app files). Switching sends
`52 57 PP S S S <5×DPI LE16> 00 PAIR DB` followed by `00 f0 00 00` ×11 (truncated at 65 bytes; unknown),
then the profile's lighting sequence. With identical profiles: `52 57 01 22 22 22 90 01 … 88 13 00 15 08 …`.
Whether the mouse stores more than one profile is unknown (0x07 byte 2 stayed 00).

## 9. Write timing and flash-wear evidence
- **Every change is written immediately** (live, no preview/commit split). The only "Save/Confirm" buttons
  (DPI Confirm, Keyboard Save, Game-Enhancement Save, Key-Pairing Save) are app-side gates before one write.
  No save/commit/flash opcode exists in any capture (verified).
- One UI change = one write burst: 4 reports for a lighting change (1 for Lights Off), 2 for a macro
  assignment, 1 for everything else (verified).
- **No streaming during slider drags** (verified for what the app received): five synthetic hue/brightness drags
  (2-5 s, 40-200 pointer moves) each produced exactly one sequence ~25 ms after button release, with the value
  of the press position; the app did not track the drag at all. Typing in RGB fields is the most write-heavy
  action: one 4-report sequence per keystroke.
- Settings survive app restarts and are readable back (they live in the mouse). Persistence across a power
  cycle and whether each write hits flash: **unknown** (not tested; no root to re-plug).
  Recommendation for software effects: assume each 0x22-0x28 write may be stored; rate-limit and test wear before
  streaming (Phase 2 item 10).

## 10. Not covered / unknown
- 0x36 meaning; 0x12 bytes 2-3 and trailing 8 bytes; 0x06 constant bytes; byte 2 of lighting writes.
- Slots S2/S3 semantics (wireless modes not tested); 2.4 GHz receiver and Bluetooth protocols.
- Game Enhancement keyboard source; macros > 1 chunk; macro Import/Export file format (app-side).
- Host-side GET_DESCRIPTOR(string) reads at 23:01:00 (Windows, not the app's vendor protocol).
- Windows System Settings panel (mouse speed, scroll lines, double-click) is Windows-only, not touched.
- Autorun, Language, Left-Click lock: app-side, not touched.

## 11. Device IDs listed in the app's own config (read from config.xml in the VM; not captured traffic)
M1: USB 3434:d035 vendor interface MI_02; 2.4 GHz receiver 3434:d031 (Type-C) / 3434:d030 (Type-A) on MI_01;
Bluetooth 3434:d035. The same app also lists M2, M3, M3 mini, M6, M7 (MI_02 family) and 4K-polling models
M2 4K, M3 4K, M3 mini 4K, M4, M6 4K (MI_03, receiver d038/d043). Shared protocol across them: unknown.

## Verified on Linux (2026-10-23)
- A small test script (not shipped) set static red/blue/green and off (session 1, webcam-confirmed).
- A read-only script (read commands only, not shipped) reproduced the app's launch replies byte-for-byte before and after session 2.
- Final state: Rainbow, webcam color cycle 3.75 s with frame-to-frame change 5.5 (vs 0.12 when off).
