#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Copy and downscale the M1 render layers into crates/tailglow-gui/assets/render/<view>/.

Usage: scripts/prepare-render-assets.py RENDER_DIR

RENDER_DIR is the output folder of the render project (it holds hero/ and top/
with base_lights_off.png, zone_<name>.png and zone_<name>_direct.png). The app
embeds the result with include_bytes!, so every package carries the same files.

- hero (1600x1200) -> 800x600, top (1600x1600) -> 700x700, all layers with the
  same filter so they stay aligned.
- The base stays RGBA (straight alpha, transparent background, shadow in alpha);
  zone and coverage layers stay 8-bit grayscale.
- No metadata chunks are written (Pillow writes none unless asked).
The glow spill is not baked in here: the app blurs the zone layers once at
startup (see crates/tailglow-gui/src/render.rs), so the amount stays tunable.
"""
import os
import sys

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "crates", "tailglow-gui", "assets", "render")

VIEWS = {"hero": 800, "top": 700}
ZONES = [
    "wheel", "palm", "buttons_left", "buttons_right", "side_left", "side_right", "dpi",
    "strip_left_1", "strip_left_2", "strip_left_3", "strip_left_4",
    "strip_right_1", "strip_right_2", "strip_right_3", "strip_right_4",
]


def fit(img, width):
    h = round(img.height * width / img.width)
    # LANCZOS on every layer; Pillow resamples RGBA with premultiplied alpha.
    return img.resize((width, h), Image.LANCZOS)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    src = sys.argv[1]
    total = 0
    for view, width in VIEWS.items():
        vin = os.path.join(src, view)
        vout = os.path.join(OUT, view)
        os.makedirs(vout, exist_ok=True)
        jobs = [("base_lights_off.png", "RGBA")]
        for z in ZONES:
            jobs.append(("zone_%s.png" % z, "L"))
            jobs.append(("zone_%s_direct.png" % z, "L"))
        size = None
        for name, mode in jobs:
            img = Image.open(os.path.join(vin, name))
            if img.mode != mode:
                img = img.convert(mode)
            if size is None:
                size = img.size
            assert img.size == size, (view, name, img.size, size)
            out = fit(img, width)
            path = os.path.join(vout, name)
            out.save(path, optimize=True)
            n = os.path.getsize(path)
            total += n
        print("%s: %d files, %dx%d" % (view, len(jobs), out.width, out.height))
    print("total %.2f MB" % (total / 1e6))


if __name__ == "__main__":
    main()
