#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Real-hardware test for tailglow on the Keychron M1.
#
# Holds the project mouse lock, reads everything, sets and reads back every
# writable setting, checks static colour / off / one animated effect with the
# webcam, restores the M1 baseline (scripts/compare_baseline.py) and finishes
# on Rainbow.
#
# Run from the repo root:  scripts/hw-test.sh [path/to/tailglow]
# Needs: the mouse on USB with the udev rule installed, ffmpeg + python3-pil
# for the webcam steps (skipped with TAILGLOW_NO_CAM=1), flock.
set -u -o pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
LOCK=${TAILGLOW_LOCK:-${XDG_RUNTIME_DIR:-/tmp}/tailglow-mouse.lock}
KC=${1:-$ROOT/target/release/tailglow}
OUT=${TAILGLOW_HW_OUT:-$ROOT/hw-test-out}
mkdir -p "$OUT"
export TAILGLOW_STATE="$OUT/state.toml"
export TAILGLOW_CAM_KEEP="$OUT/frames"
LOG="$OUT/hw-test.log"
: > "$LOG"

pass=0; fail=0
say() { echo "$*" | tee -a "$LOG"; }
step() {
    # step NAME cmd...
    local name=$1; shift
    say ""
    say "## $name"
    say "\$ $*"
    if "$@" 2>&1 | tee -a "$LOG"; then
        say "-> PASS: $name"; pass=$((pass+1))
    else
        say "-> FAIL: $name"; fail=$((fail+1))
    fi
}
cam() {
    if [ "${TAILGLOW_NO_CAM:-}" = 1 ]; then say "(webcam check skipped: TAILGLOW_NO_CAM=1)"; return 0; fi
    python3 "$HERE/cam_check.py" "$@"
}

if [ ! -x "$KC" ]; then echo "tailglow binary not found at $KC (cargo build --release first)"; exit 2; fi

# ---- lock ------------------------------------------------------------------
exec 9>"$LOCK"
if ! flock -w 600 9; then echo "could not get the mouse lock $LOCK"; exit 2; fi
say "tailglow hw-test $(date '+%F %T')  binary: $KC  lock: $LOCK"
trap 'say ""; say "lock released $(date +%T)"' EXIT

# ---- 1. read everything -----------------------------------------------------
step "list"        "$KC" list
step "dump before" "$KC" dump "$OUT/before.json"
step "info before" "$KC" info
step "export before" "$KC" export "$OUT/before.toml"

# ---- 1b. node identification: nothing is sent to a node
# that is not the vendor interface; refusal happens before the open.
VENDOR=$("$KC" list | awk '/Keychron M1/ && /vendor interface/ {print $1; exit}')
POINTER=$("$KC" list | awk '/Keychron M1/ && /input interface/ {print $1; exit}')
say "vendor node: ${VENDOR:-?}  pointer node: ${POINTER:-?}"
step "--device vendor node reads" "$KC" --device "$VENDOR" info
step "--device pointer node refused before open" bash -c \
    "out=\$('$KC' --device '$POINTER' info 2>&1); rc=\$?; echo \"\$out\"; [ \$rc -ne 0 ] && echo \"\$out\" | grep -q 'is USB interface [01] of the Keychron M1'"
step "--device /dev/null refused" bash -c "! '$KC' --device /dev/null info"
# Nothing differs from what was just exported: 0 writes, read-back compare clean.
step "import of the current state writes nothing" bash -c \
    "'$KC' import '$OUT/before.toml' | tee /dev/stderr | grep -q 'applied 0 change(s)'"

# ---- 2. set and read back each writable setting -------------------------------
# (every tailglow write re-reads the mouse and fails on a mismatch)
step "dpi set"       "$KC" dpi set 450 800 1600 4900 5000 --active 4
step "dpi active"    "$KC" dpi active 1
step "rate 500"      "$KC" rate 500
step "rate 125"      "$KC" rate 125
step "sensor all second options" "$KC" sensor --lift-off 2 --ripple off --angle-snap on --motion-sync off --scroll reverse
step "debounce 11"   "$KC" debounce 11
step "debounce 12 refused without --allow-untested" bash -c \
    "out=\$('$KC' debounce 12 2>&1); echo \"\$out\"; echo \"\$out\" | grep -q 'allow-untested' && [ \"\$('$KC' debounce)\" = 'debounce 11 ms' ]"
step "button key4 -> volume-up"  "$KC" buttons set key4 consumer:volume-up
step "button key5 -> ctrl+shift+k" "$KC" buttons set key5 key:ctrl+shift+k
step "macro key6 (experimental)" "$KC" macro key6 --loop times:3 --fixed-delay 25 a+ 47ms a- 3000ms b+ 32ms b-
# An unchanged macro is not re-sent (state.toml fingerprint).
step "export carries the macro" bash -c "'$KC' export '$OUT/with-macro.toml' && grep -q 'macros.key6' '$OUT/with-macro.toml'"
step "re-import skips the unchanged macro" bash -c \
    "'$KC' import '$OUT/with-macro.toml' | tee /dev/stderr | grep -q 'applied 0 change(s).*1 macro(s) already on the mouse'"
step "left-click guard refuses key1" bash -c "! '$KC' buttons set key1 disabled"
step "info after writes" "$KC" info

# ---- 3. lighting + webcam -----------------------------------------------------
step "static green"  "$KC" color 00ff00 --brightness 255 --speed 128
step "cam: green"    cam color 0 255 0
step "static red"    "$KC" color ff0000
step "cam: red"      cam color 255 0 0
step "lights off"    "$KC" effect off
step "cam: off"      cam off
step "breathing red (baseline params)" "$KC" effect breathing --color ff0000 --brightness 255 --speed 128
step "cam: breathing animated" cam animated 5

# ---- 4. restore the baseline ---------------------------------------------------
step "restore dpi"   "$KC" dpi set 400 800 1600 3200 5000 --active 3
step "restore rate"  "$KC" rate 1000
step "restore sensor" "$KC" sensor --lift-off 1 --ripple on --angle-snap off --motion-sync on --scroll forward
step "restore debounce" "$KC" debounce 8
step "restore key4"  "$KC" buttons reset key4
step "restore key5"  "$KC" buttons reset key5
step "restore key6"  "$KC" buttons reset key6
# A per-key reset after a macro left byte 20 of the 52 61 reply at 0x22 on the
# real M1; the vendor app's "Reset all button functions" (51 0b 01 01) clears it.
step "restore all buttons (vendor reset)" "$KC" buttons reset --all
step "restore static green slot" "$KC" color 00ff00 --brightness 255 --speed 128
step "finish on rainbow" "$KC" effect rainbow --brightness 255 --speed 128
step "cam: rainbow animated" cam animated 5
step "dump after"    "$KC" dump "$OUT/after.json"
step "compare with the baseline" python3 "$HERE/compare_baseline.py" "$OUT/after.json"

say ""
say "hw-test: $pass passed, $fail failed  ($(date +%T)); log $LOG"
[ "$fail" -eq 0 ]
