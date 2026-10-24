#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Live theme-following test for tailglow-gui, entirely private:
#   - its own X display (Xvnc :9 by default, never the desktop's),
#   - its own D-Bus session bus with scripts/fake-portal.py on it,
#   - the fake mouse (TAILGLOW_FAKE=session2) and throw-away config/state files.
#
# Usage: scripts/theme-live-test.sh [GUI_BINARY] [OUT_DIR]
# Prints PASS/FAIL per check and exits non-zero on any failure.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
gui="${1:-$here/../target/release/tailglow-gui}"
out="${2:-$(mktemp -d "${TMPDIR:-/tmp}/tailglow-theme.XXXXXX")}"
disp="${TAILGLOW_TEST_DISPLAY:-:9}"
mkdir -p "$out"
fails=0
pids=()

cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
    [ -n "${bus_pid:-}" ] && kill "$bus_pid" 2>/dev/null
}
trap cleanup EXIT

check() { # name, command...
    local name="$1"; shift
    if "$@"; then echo "PASS  $name"; else echo "FAIL  $name"; fails=$((fails + 1)); fi
}

# Private X display (reuse one that is already up, e.g. from a screenshot run).
if ! xdpyinfo -display "$disp" >/dev/null 2>&1; then
    Xvnc "$disp" -geometry 1040x760 -depth 24 -SecurityTypes None -localhost -rfbport 0 \
        >"$out/xvnc.log" 2>&1 &
    pids+=($!)
    sleep 1.5
fi

# Private session bus.
read -r bus_addr bus_pid < <(dbus-daemon --session --fork --print-address=1 --print-pid=1 | paste -sd' ')
export DBUS_SESSION_BUS_ADDRESS="$bus_addr"
echo "private bus: $bus_addr (pid $bus_pid), display $disp, output $out"

portal_set() {
    gdbus call --session --dest org.freedesktop.portal.Desktop \
        --object-path /org/freedesktop/portal/desktop \
        --method io.github.cyberjody.Tailglow.FakePortal.Set "$1" >/dev/null
}

start_gui() { # log file, extra args...
    local log="$1"; shift
    DISPLAY="$disp" WAYLAND_DISPLAY= TAILGLOW_FAKE=session2 TAILGLOW_THEME_LOG=1 \
        TAILGLOW_STATE="$out/state.toml" TAILGLOW_GUI_CONFIG="$out/gui.toml" \
        "$gui" "$@" >"$log" 2>&1 &
    gui_pid=$!
    pids+=("$gui_pid")
    sleep 3
}

stop_gui() { kill "$gui_pid" 2>/dev/null; wait "$gui_pid" 2>/dev/null; }

grab() { # name -> png + pixel report (content background, top right)
    xwd -root -display "$disp" -silent >"$out/$1.xwd"
    python3 "$here/xwd2png.py" "$out/$1.xwd" "$out/$1.png" 1020,8 | tee -a "$out/pixels.txt" |
        sed "s/^/  $1: /"
    rm -f "$out/$1.xwd"
}

logged() { grep -q -- "$2" "$1"; }
shot_dark() { grep -q "(dark)" < <(grep "^pixel" "$out/pixels.txt" | tail -1); }
shot_light() { grep -q "(light)" < <(grep "^pixel" "$out/pixels.txt" | tail -1); }

# --- 1. live switching with a portal that starts at 1 (prefer dark) ---------
python3 "$here/fake-portal.py" 1 >"$out/portal.log" 2>&1 &
pids+=($!)
sleep 1
start_gui "$out/live.log"
check "starts dark (portal 1)" logged "$out/live.log" "start: mode=System system=Dark -> dark"
grab 1-start-dark; check "screen is dark" shot_dark

portal_set 2; sleep 1.5
check "switches to light on SettingChanged 2" logged "$out/live.log" "desktop changed: mode=System system=Light -> light"
grab 2-switched-light; check "screen is light" shot_light

portal_set 0; sleep 1.5
check "0 (no preference) gives light" logged "$out/live.log" "desktop changed: mode=System system=NoPreference -> light"
grab 3-no-preference; check "screen is light" shot_light

portal_set 1; sleep 1.5
check "back to dark on SettingChanged 1" logged "$out/live.log" "desktop changed: mode=System system=Dark -> dark"
grab 4-back-dark; check "screen is dark" shot_dark
check "no errors in the GUI log" bash -c "! grep -qiE 'error|panic|warn' '$out/live.log'"
stop_gui

# --- 2. a manual choice ignores the desktop, but the note still follows ---------
start_gui "$out/manual.log" --theme light
portal_set 1; sleep 0.5; portal_set 2; sleep 0.5; portal_set 1; sleep 1.5
check "--theme light stays light while the desktop flips" \
    bash -c "! grep -q -- '-> dark' '$out/manual.log' && grep -q 'mode=Light system=Dark -> light' '$out/manual.log'"
grab 5-manual-light; check "screen is light" shot_light
stop_gui
gdbus call --session --dest org.freedesktop.portal.Desktop --object-path /org/freedesktop/portal/desktop \
    --method io.github.cyberjody.Tailglow.FakePortal.Quit >/dev/null 2>&1
sleep 0.5

# --- 3. portal without the key ---------------------------------------------------
python3 "$here/fake-portal.py" none >"$out/portal-none.log" 2>&1 &
pids+=($!)
sleep 1
start_gui "$out/nokey.log"
check "portal without color-scheme gives light" logged "$out/nokey.log" "start: mode=System system=NoPreference -> light"
check "  ...silently" bash -c "[ \$(grep -vc '^theme:' '$out/nokey.log') -eq 0 ]"
stop_gui
gdbus call --session --dest org.freedesktop.portal.Desktop --object-path /org/freedesktop/portal/desktop \
    --method io.github.cyberjody.Tailglow.FakePortal.Quit >/dev/null 2>&1
sleep 0.5

# --- 4. a bus with no portal at all, and no bus at all ----------------------------
start_gui "$out/noportal.log"
check "no portal on the bus gives light" logged "$out/noportal.log" "start: mode=System system=NoPreference -> light"
check "  ...silently" bash -c "[ \$(grep -vc '^theme:' '$out/noportal.log') -eq 0 ]"
grab 6-no-portal; check "screen is light" shot_light
stop_gui
DBUS_SESSION_BUS_ADDRESS="unix:path=$out/no-such-bus" start_gui "$out/nobus.log"
check "no session bus gives light" logged "$out/nobus.log" "start: mode=System system=NoPreference -> light"
check "  ...silently" bash -c "[ \$(grep -vc '^theme:' '$out/nobus.log') -eq 0 ]"
stop_gui

echo "logs and screenshots: $out"
if [ "$fails" -eq 0 ]; then echo "theme-live-test: all checks passed"; else echo "theme-live-test: $fails FAILED"; fi
exit "$fails"
