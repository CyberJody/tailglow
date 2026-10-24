#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare a read-only dump of the M1 with its baseline (below).

usage: compare_baseline.py DUMP.json
DUMP.json is the output of `tailglow dump` (hidden command):
{"reads": {"06": "51 06 …", …, "52-61": "52 61 …"}}.

The baseline is what the M1 answered at the start of capture session 2
(22:35:58). Expected end state: every reply identical to it except
- 0x12 effect byte 01 -> 03 (the test ends on Rainbow);
- 0x07 bytes 4-5 `60 40` -> `22 02` (the app's DPI write sets all three slots).
Additionally tolerated here, and reported: bit 5 of the 0x07 sensor-flags byte
(0x35 -> 0x15). PROTOCOL.md tags it as the "factory state" flag that any 0x42
write clears; the five sensor options it sits next to are compared exactly.
Exit 0 only when nothing else differs.
"""
import json
import sys

BASE = {
    "06": "51 06 01 00 34 34 35 d0 00 01 00 64 02",
    "07": "51 07 00 22 22 02 90 01 20 03 40 06 80 0c 88 13 35 05 08",
    "12": "51 12 01 06 03 ff ff cf cf cd cd cf cd",
    "13": "51 13 ff ff ff ff ff ff",
    "17": "51 17 80 80 80 80 80 80",
    "14": "51 14 00 ff ff ff ff ff",
    "15": "51 15 ff",
    "16": "51 16",
    "04": "51 04 09 31 2e 30 2e 30 72 2d 35",
    "52-61": "52 61",
}


def norm(h):
    return bytes.fromhex(h.replace(" ", "")).rstrip(b"\x00")


def main():
    got = json.load(open(sys.argv[1]))["reads"]
    ok = True
    for k, v in BASE.items():
        want = norm(v)
        have = norm(got.get(k, ""))
        if k == "07" and len(have) >= 17 and len(want) >= 17:
            w = bytearray(want)
            h = bytearray(have)
            if (w[16] & ~0x20) == (h[16] & ~0x20) and w[16] != h[16]:
                print(f"07     NOTE sensor-flags byte 0x{w[16]:02x} -> 0x{h[16]:02x}: only bit 5 (factory flag) differs; tolerated")
                h[16] = w[16]
                have = bytes(h)
        same = have == want
        if not same:
            ok = False
        print(f"{k:6} {'SAME' if same else 'DIFF'}\n   baseline {want.hex(' ')}\n   now      {have.hex(' ')}")
    print("RESULT:", "mouse matches the baseline end state" if ok else "UNEXPECTED differences")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
