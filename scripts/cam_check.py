#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Webcam check of the mouse LEDs (Logitech C920 at /dev/video0, 1280x720).

The mouse body occupies roughly (345,125)-(1000,720) in the frame as the
camera sat for the M1 tests (the capture sessions used (385,0)-(915,355); override with
TAILGLOW_CAM_BOX=x1,y1,x2,y2). Colour is judged on the *lit* pixels (any
channel > 80) so a dark room or a matte shell does not matter; "off" means
no lit pixels and a dark mean.

usage:
  cam_check.py off                 one frame; passes if the box is dark
  cam_check.py color R G B         one frame; passes if the dominant channel matches
  cam_check.py animated [SECS]     short series; passes if luminance varies
  cam_check.py measure             print the mean RGB and exit 0

Exit code 0 = pass, 1 = fail, 2 = camera error. Prints one line of evidence.
"""
import glob
import os
import subprocess
import sys
import tempfile

try:
    from PIL import Image
except ImportError:  # pragma: no cover
    print("cam_check: python3-pil is needed", file=sys.stderr)
    sys.exit(2)

BOX = tuple(int(v) for v in os.environ.get("TAILGLOW_CAM_BOX", "345,125,1000,720").split(","))
DEVICE = os.environ.get("TAILGLOW_CAM", "/dev/video0")
BRIGHT = 80


def stats(path):
    """(mean RGB, fraction of lit pixels, mean RGB of lit pixels)."""
    im = Image.open(path).crop(BOX).convert("RGB").resize((132, 120))
    px = list(im.getdata())
    n = len(px)
    mean = tuple(round(sum(p[i] for p in px) / n, 1) for i in range(3))
    lit = [p for p in px if max(p) > BRIGHT]
    lit_mean = (
        tuple(round(sum(p[i] for p in lit) / len(lit), 1) for i in range(3)) if lit else None
    )
    return mean, len(lit) / n, lit_mean


def mean_rgb(path):
    return stats(path)[0]


def frame(full=False):
    d = tempfile.mkdtemp(dir=os.environ.get("TMPDIR"))
    out = os.path.join(d, "frame.jpg")
    r = subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-y", "-f", "v4l2", "-video_size", "1280x720",
         "-i", DEVICE, "-frames:v", "1", "-ss", "1.5", out],
        capture_output=True, text=True,
    )
    if r.returncode != 0 or not os.path.exists(out):
        print("cam_check: camera error:", r.stderr.strip())
        sys.exit(2)
    keep = os.environ.get("TAILGLOW_CAM_KEEP")
    if keep:
        os.makedirs(keep, exist_ok=True)
        import shutil
        shutil.copy(out, os.path.join(keep, f"{sys.argv[1]}-{int(__import__('time').time())}.jpg"))
    return stats(out) if full else mean_rgb(out)


def series(secs, fps=8):
    d = tempfile.mkdtemp(dir=os.environ.get("TMPDIR"))
    n = int(secs * fps)
    r = subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-y", "-f", "v4l2", "-framerate", "30",
         "-video_size", "1280x720", "-i", DEVICE, "-ss", "1.0",
         "-vf", f"fps={fps}", "-frames:v", str(n), f"{d}/f%04d.jpg"],
        capture_output=True, text=True,
    )
    files = sorted(glob.glob(f"{d}/f*.jpg"))
    if r.returncode != 0 or len(files) < n // 2:
        print("cam_check: camera error:", r.stderr.strip())
        sys.exit(2)
    return [mean_rgb(f) for f in files]


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    mode = sys.argv[1]
    if mode == "measure":
        print("cam_check measure meanRGB", frame())
        return
    if mode == "off":
        m, lit, _ = frame(full=True)
        lum = sum(m) / 3
        ok = lum < 55 and lit < 0.01
        print(
            f"cam_check off: meanRGB {m} lum {lum:.1f}, lit pixels {lit:.1%} "
            f"(need lum < 55 and lit < 1%) -> {'PASS' if ok else 'FAIL'}"
        )
        sys.exit(0 if ok else 1)
    if mode == "color":
        want = [int(x) for x in sys.argv[2:5]]
        m, lit, lit_mean = frame(full=True)
        dom = max(range(3), key=lambda i: want[i])
        others = [i for i in range(3) if i != dom]
        names = "RGB"
        ok = lit >= 0.02 and lit_mean is not None and all(lit_mean[dom] > lit_mean[o] + 25 for o in others)
        print(
            f"cam_check color {want}: meanRGB {m}, lit pixels {lit:.1%} with mean {lit_mean} "
            f"(need >= 2% lit and {names[dom]} dominant by 25 among lit pixels) -> {'PASS' if ok else 'FAIL'}"
        )
        sys.exit(0 if ok else 1)
    if mode == "animated":
        secs = float(sys.argv[2]) if len(sys.argv) > 2 else 4.0
        vals = series(secs)
        lum = [sum(v) / 3 for v in vals]
        rng = max(lum) - min(lum)
        steps = [abs(a - b) for a, b in zip(lum, lum[1:])]
        mean_step = sum(steps) / max(1, len(steps))
        # Per-channel ranges catch colour cycling at constant luminance (rainbow).
        ch_rng = max(max(v[i] for v in vals) - min(v[i] for v in vals) for i in range(3))
        ok = rng > 8 or ch_rng > 20
        print(
            f"cam_check animated: {len(vals)} frames, lum {min(lum):.1f}-{max(lum):.1f} (range {rng:.1f}), "
            f"mean frame step {mean_step:.2f}, max channel range {ch_rng:.1f} (need lum range > 8 or channel range > 20) -> {'PASS' if ok else 'FAIL'}"
        )
        sys.exit(0 if ok else 1)
    print(__doc__)
    sys.exit(2)


if __name__ == "__main__":
    main()
