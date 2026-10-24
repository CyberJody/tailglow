# Adding your Keychron mouse

Only the Keychron M1 (wired, 3434:d035) is verified. The other mice listed in
`README.md` as *likely* share the vendor app and the vendor interface, but
nobody has looked at their traffic.

**Just want to say whether your mouse works?** Fill in the
[mouse report form](https://github.com/CyberJody/tailglow/issues/new?template=mouse-report.yml)
with the output of `tailglow info`; no capture needed. The rest of this guide
is for going further.

This guide is how to capture that traffic
from the vendor's Windows app, decode it, compare it with `docs/PROTOCOL.md`,
verify `tailglow` against your mouse on Linux, and report the result.

It is the exact procedure used for the M1 (two sessions, 2026-10-23).

## 1. Safety rules

Read these before plugging anything in.

1. **Only replay what the vendor app sends.** The point of a capture is to see
   the app's own bytes. Never send guessed commands, never sweep opcodes or
   argument values, never "try what 0x37 does". The mice have no checksum and
   no ACK; an unknown write may do anything.
2. **Never trigger a firmware update.** Keep the VM's network **off** for the
   whole session so the app cannot download firmware, and never click an
   update prompt. The M1 captures contain no firmware, bootloader or flash
   command, and this project will never implement one.
3. **Factory reset last, and only deliberately.** If you capture "Restore
   Factory Settings", do it as the final step of the session, after everything
   else, and only if you are willing to lose your settings.
4. **Restore your settings afterwards.** Note your lighting, DPI, rate and
   button map before you start (`tailglow info` and `tailglow export` if reads
   already work on your model) and put them back at the end.
5. Reads are safe on *likely* models; `tailglow` allows them without a flag.
   Writes to a *likely* model need `--allow-untested` and are at your own
   risk until the model is verified.

## 2. Capturing with usbmon and a Windows VM

The vendor app (Keychron Mouse, Windows) runs in a VM. The mouse is passed
through to the VM over USB and the Linux host records everything on that bus
with usbmon.

### Host setup

    sudo modprobe usbmon
    lsusb                          # find your mouse: Bus 00N Device 0MM: ID 3434:xxxx
    lsusb -v -d 3434: 2>/dev/null | grep -E 'bInterfaceNumber|iInterface|bcdDevice'

Note the **bus number** (N), the **device number** (MM) and the interface
list. You need read access to `/dev/usbmonN` (either `sudo` for tcpdump, or
`sudo chmod o+r /dev/usbmonN` for the session).

Start the capture **before** attaching the mouse to the VM so that the
enumeration and the app's first reads are included:

    tcpdump -i usbmonN -w capture.pcap

### Pass the mouse through (libvirt example)

A hostdev XML for a Keychron mouse (this one is for the M1; change the
product id for your model):

    <hostdev mode='subsystem' type='usb' managed='yes'>
      <source>
        <vendor id='0x3434'/>
        <product id='0xd035'/>
      </source>
    </hostdev>

Then, with the VM running and its network link down:

    virsh domif-setlink VM vnetX down            # no network for the VM during the session
    virsh attach-device VM hostdev.xml --live    # mouse goes to the VM
    ...
    virsh detach-device VM hostdev.xml --live    # mouse comes back to the host
    virsh domif-setlink VM vnetX up

The device number changes when the VM re-enumerates the mouse; `decode.py`
takes it as an argument (below), and the capture includes the enumeration so
you can find it.

### Drive the app and keep an action log

Run the Keychron app in the VM and click through **every** page and every
control: each lighting effect, each slider end and middle, each swatch, typing
an RGB value, DPI stages and Confirm, report rate, each sensor toggle, debounce
+/-, each button category and a few actions in each, a macro, profiles,
and resets (factory reset last, see rule 3).

While you do it, keep a timestamped action log on the host, one line per
click, so that the decode can line up bytes with what you did:

    log() { echo "$(date +%T.%N) $*" >> actions.log; }
    log CLICK lighting tab
    log CLICK effect Breathing
    log CLICK swatch blue
    log TYPE R field 18

`decode.py` expects `HH:MM:SS.fraction <free text>` (one line per action,
time first; `date +%T.%N` produces exactly that and is compared as a string
against the packet timestamps). Be generous: a note like `NOTE nothing
happened` is also useful. Wait a second or two between actions so that bursts
do not overlap.

Stop tcpdump when done. Keep the pcap, the action log and screenshots of each
app page (the M1 set is referenced from `docs/PROTOCOL.md`).

## 3. Decoding

`scripts/decode.py` (in this repository; Python 3 standard library only)
prints every SET_REPORT / GET_REPORT on the control endpoint and every vendor
interrupt transfer for one USB device, interleaved with your action log:

    python3 scripts/decode.py capture.pcap actions.log DEVNUM [--all]

Try it on the M1 captures first: `python3 scripts/decode.py
testdata/session2.pcap testdata/session2-actions.log 13`.

- `capture.pcap`: the usbmon capture (LINKTYPE_USB_LINUX_MMAPPED or the older
  48-byte usbmon header).
- `actions.log`: your timestamped log.
- `DEVNUM`: the USB device number of the mouse **as seen by the host** in that
  capture (from `lsusb` while the mouse was attached; defaults to 13, which was
  the M1's number on the capture machine, so set it).
- `--all`: also print the pointer-interface interrupt traffic and other
  control transfers (noisy; useful once, to confirm which interface is which).

Typical output lines:

    22:37:05.123456 SET_REPORT  if=2 rid=0x51 type=3 len=21: 51 22 01 02 00 ...
    22:37:05.129001 SET_REPORT  if=2 rid=0x51 type=3 len=21: 51 23 01 02 ff 00 ...
      CTL reply len=21: 51 12 01 06 02 ff ff cf cf cd cd cf cd 00 ...

`if=` is the interface number, `rid=` the report id, `type=3` a feature
report.

You can also feed the capture to `tailglow` itself: the fake transport can
replay a pcap, so

    TAILGLOW_FAKE=/path/capture.pcap tailglow info

shows whether the app's reads from your mouse parse with the M1 layouts
(firmware string, DPI table, lighting tables, button flags). If `info` prints
sensible values, the read side is the same protocol.

## 4. What to look for

Compare with `docs/PROTOCOL.md`; every claim there is tagged *verified*,
*likely* or *unknown*.

- **Transport:** feature reports **0x51** (21 bytes) and **0x52** (65 bytes)
  on the vendor interface (interface 2 on the M1, interface 1 on the 2.4 GHz
  receivers according to the app's configuration). Writes are SET_REPORT; a
  read is a SET_REPORT of the command with zero arguments followed by a
  GET_REPORT of the same report id. If your mouse uses different report ids
  (the 8K family uses output reports 0xB3/0xB5), it is a different protocol
  family and nothing in `tailglow` applies.
- **The launch sequence:** on the M1 the app always reads
  `51 06`, `51 07`, writes `51 36`, reads `52 61` (+ `52 62 KK` per flagged
  key), then `51 12 13 17 14 15 16` and `51 04`. Check that your mouse
  answers the same commands and that the reply layouts match (VID/PID in the
  0x06 reply, DPI table in 0x07, per-effect tables in 0x13-0x17).
- **One write burst per UI change:** a lighting change is four reports
  (`0x22 0x23 0x27 0x28`), a macro is two (`0x54 0x53`), everything else is
  one. Note anything your mouse does differently (extra commands, different
  lengths, a save/commit step, ACK reads).
- **Anything not in the table of commands** in `docs/PROTOCOL.md` is a
  finding. Record it with its bytes and the action that produced it, tagged
  *unknown*; do not implement it.
- Wireless models: note which connection mode you captured (cable, 2.4 GHz,
  Bluetooth); the M1 capture is cable-only and the three "slot" bytes in 0x07
  are only *likely* understood.

## 5. Verifying on Linux

With the mouse back on the host and the udev rule adapted to your product id
(copy the two lines of `packaging/70-keychron-m1.rules`, change `d035` and, if
needed, `bInterfaceNumber`):

1. **Reads:** `tailglow list` must show your mouse with its interface;
   `tailglow info` must print the same values the app showed. Compare against
   the replies in your decode byte for byte if anything looks off.
2. **Writes (with `--allow-untested`):** change one thing at a time and
   confirm the read-back (`tailglow` re-reads after every write and fails on a
   mismatch). Confirm in the vendor app too if you can.
3. **Lighting with a webcam:** point any webcam at the mouse, take a frame
   (`ffmpeg -f v4l2 -video_size 1280x720 -i /dev/video0 -frames:v 1 -ss 1.5 out.jpg`)
   and compute the mean RGB of the box that contains the mouse body. The mean
   tracks the LED colour well enough to tell red, green, blue and off apart,
   and a short series of frames shows whether an animated effect is running
   (frame-to-frame change near 0 when off, several units when animating).
   `scripts/cam_check.py` does this: `off`, `color R G B`, `animated [SECS]`,
   `measure`; set the box with `TAILGLOW_CAM_BOX=x1,y1,x2,y2`.
4. **Full run:** `scripts/hw-test.sh` sets and reads back every setting,
   checks lighting with the webcam and restores the M1 baseline. Its restore
   values are the M1's; read it before running it on another mouse and adapt
   the baseline to what you noted in step 1.
5. **Restore your settings** (rule 4).

## 6. Submitting

Open a [mouse report](https://github.com/CyberJody/tailglow/issues/new?template=mouse-report.yml)
or a pull request with:

- VID:PID, the vendor **interface number**, the firmware string
  (`tailglow info` prints it; `51 04` reply), connection mode (cable/2.4 GHz/BT);
- the pcap and the action log (they will become `testdata/` for a FakeMouse
  test; the M1 ones are `testdata/session1.pcap` and `testdata/session2.pcap`);
- which commands you verified (read, write, read-back, webcam), which differ
  from the M1, and anything you saw that is not in `docs/PROTOCOL.md`;
- a patch to `crates/tailglow-core/src/device.rs`: change your model's entry
  in `MODELS` from `SupportStatus::Likely` to `SupportStatus::Verified` (or
  add it if it is missing) and replace the evidence string with where the
  proof lives, for example
  `"testdata/m3-session1.pcap; confirmed on the mouse 2026-11-02"`. Keep the
  `vendor_interface` number you measured;
- a line for `packaging/70-keychron-m1.rules` (or a new rule file) for your
  product id.

A model is marked *verified* only after writes have been read back on the
hardware; a capture alone upgrades the evidence string but keeps *likely*.
