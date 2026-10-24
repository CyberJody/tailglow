#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Bluetooth screenshots, all private (never the desktop's display or buses):
#   - the GUI with the mouse on Bluetooth only (read-only pages, battery,
#     stale pairings), light and dark;
#   - the tray icon in a real xfce4-panel (systray plugin, StatusNotifierItem)
#     with its tooltip and menu showing the Bluetooth battery;
#   - the low-battery notification: the fake battery drops to 14 % and the
#     tray's org.freedesktop.Notifications call is recorded with dbus-monitor.
#
# Bluetooth comes from the fake BlueZ (crates/tailglow-hid/examples/fake-bluez.rs)
# on its own dbus-daemon; USB is TAILGLOW_FAKE=none (no cable).
#
# Usage: scripts/bluetooth-screenshots.sh [TARGET_DIR] [OUT_DIR]
#   TARGET_DIR defaults to target/release (needs tailglow-gui, tailglow-tray,
#   examples/fake-bluez: cargo build --release --workspace --examples).
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
bin="$(realpath "${1:-$here/../target/release}")"
out="$(realpath -m "${2:-$here/../screenshots/bluetooth}")"
disp="${TAILGLOW_TEST_DISPLAY:-:9}"
work="$(mktemp -d "${TMPDIR:-/tmp}/tailglow-bt.XXXXXX")"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT
mkdir -p "$out"
had_xfce_cfg=0; [ -e "$HOME/.config/xfce4" ] && had_xfce_cfg=1
if xdpyinfo -display "$disp" >/dev/null 2>&1; then
    echo "display $disp is already in use; set TAILGLOW_TEST_DISPLAY" >&2
    exit 2
fi
Xvnc "$disp" -geometry 1040x760 -depth 24 -SecurityTypes None -localhost -rfbport 0 \
    >"$work/xvnc.log" 2>&1 &
pids+=($!)
sleep 1.5

# Fake BlueZ: connected M1 at 95 %, one stale M1 pairing, headphones.
"$bin/examples/fake-bluez" --battery 95 >"$work/fake-bluez.out" 2>"$work/fake-bluez.err" &
pids+=($!)
for _ in $(seq 50); do grep -q TAILGLOW_BLUEZ_BUS "$work/fake-bluez.out" 2>/dev/null && break; sleep 0.1; done
bt_bus="$(sed -n 's/^TAILGLOW_BLUEZ_BUS=//p' "$work/fake-bluez.out")"
[ -n "$bt_bus" ] || { echo "fake-bluez did not start"; cat "$work/fake-bluez.err"; exit 1; }
echo "fake BlueZ on $bt_bus"

# ----- GUI ---------------------------------------------------------------
for theme in light dark; do
    mkdir -p "$work/gui-$theme"
    (cd "$work/gui-$theme" &&
        DISPLAY="$disp" WAYLAND_DISPLAY= TAILGLOW_FAKE=none TAILGLOW_BLUEZ_BUS="$bt_bus" \
            TAILGLOW_BT_LOG=1 TAILGLOW_STATE=state.toml TAILGLOW_GUI_CONFIG="$work/gui.toml" \
            DBUS_SESSION_BUS_ADDRESS="unix:path=$work/no-bus" \
            timeout 60 "$bin/tailglow-gui" --theme "$theme" --screenshot "$out/gui-$theme" 2>&1) |
        sed "s/^/  [gui $theme] /"
done

# ----- tray in xfce4-panel -----------------------------------------------
# Private session bus and private XDG dirs, so xfconfd/xfce4-panel never see
# the user's real configuration. Export the XDG dirs BEFORE starting the bus:
# D-Bus-activated services (xfconfd) inherit the bus daemon's environment.
export XDG_CONFIG_HOME="$work/config" XDG_CACHE_HOME="$work/cache" XDG_DATA_HOME="$work/data"
eval "$(dbus-daemon --session --fork --print-address=1 --print-pid=1 |
    { read -r a; read -r p; echo "sess_bus='$a'; sess_pid=$p"; })"
pids+=("$sess_pid")
xml="$XDG_CONFIG_HOME/xfce4/xfconf/xfce-perchannel-xml"
mkdir -p "$xml" "$XDG_DATA_HOME/icons/hicolor/scalable/apps"
cp "$here/../packaging/icons/io.github.cyberjody.Tailglow.svg" "$XDG_DATA_HOME/icons/hicolor/scalable/apps/"
cat >"$xml/xfce4-panel.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<channel name="xfce4-panel" version="1.0">
  <property name="configver" type="int" value="2"/>
  <property name="panels" type="array">
    <value type="int" value="1"/>
    <property name="panel-1" type="empty">
      <property name="position" type="string" value="p=6;x=0;y=0"/>
      <property name="length" type="double" value="100"/>
      <property name="position-locked" type="bool" value="true"/>
      <property name="size" type="uint" value="36"/>
      <property name="icon-size" type="uint" value="24"/>
      <property name="plugin-ids" type="array">
        <value type="int" value="1"/>
        <value type="int" value="2"/>
      </property>
    </property>
  </property>
  <property name="plugins" type="empty">
    <property name="plugin-1" type="string" value="separator">
      <property name="expand" type="bool" value="true"/>
      <property name="style" type="uint" value="0"/>
    </property>
    <property name="plugin-2" type="string" value="systray">
      <property name="square-icons" type="bool" value="true"/>
    </property>
  </property>
</channel>
EOF
export DBUS_SESSION_BUS_ADDRESS="$sess_bus"
# A plain dark root so the panel and menu stand out.
DISPLAY="$disp" xsetroot -solid '#3a3f47' 2>/dev/null || true
DISPLAY="$disp" xfce4-panel >"$work/panel.log" 2>&1 &
pids+=($!)
sleep 3
dbus-monitor --address "$sess_bus" "interface='org.freedesktop.Notifications',member='Notify'" \
    >"$out/tray-notify-monitor.log" 2>&1 &
pids+=($!)
DISPLAY="$disp" TAILGLOW_FAKE=none TAILGLOW_BLUEZ_BUS="$bt_bus" \
    XDG_DATA_DIRS="$XDG_DATA_HOME:/usr/share" \
    "$bin/tailglow-tray" --no-apps --poll-secs 5 >"$work/tray.log" 2>&1 &
pids+=($!)
sleep 4

grab() { # name
    DISPLAY="$disp" xwd -root -silent >"$work/$1.xwd"
    python3 "$here/xwd2png.py" "$work/$1.xwd" "$out/$1.png"
    echo "saved $out/$1.png"
}
# The systray sits at the right end of the top panel: find the icon by
# hovering where StatusNotifier items go (right edge, panel height 36).
icon_x=$((1040 - 20)); icon_y=18
grab tray-panel
DISPLAY="$disp" xdotool mousemove "$icon_x" "$icon_y"; sleep 2.5
grab tray-tooltip
DISPLAY="$disp" xdotool click 3; sleep 1.5
grab tray-menu
DISPLAY="$disp" xdotool key Escape; sleep 0.5

# Low battery: 95 % → 14 %, the tray must notify exactly once.
gdbus call --address "$bt_bus" --dest org.bluez --object-path /fake \
    --method io.github.cyberjody.Tailglow.FakeBluez.SetBattery 14 >/dev/null
sleep 2
gdbus call --address "$bt_bus" --dest org.bluez --object-path /fake \
    --method io.github.cyberjody.Tailglow.FakeBluez.SetBattery 13 >/dev/null
sleep 2
DISPLAY="$disp" xdotool mousemove 500 400; sleep 0.5
DISPLAY="$disp" xdotool mousemove "$icon_x" "$icon_y"; sleep 2.5
grab tray-tooltip-low
calls="$(gdbus call --address "$bt_bus" --dest org.bluez --object-path /fake \
    --method io.github.cyberjody.Tailglow.FakeBluez.Calls)"
echo "fake BlueZ method calls seen: $calls"
echo "notifications sent: $(grep -c 'member=Notify' "$out/tray-notify-monitor.log" || true)"
grep -A3 'member=Notify' "$out/tray-notify-monitor.log" | grep 'string' | sed 's/^ */  /' || true
sed 's/^/  [tray] /' "$work/tray.log"
if [ "$had_xfce_cfg" = 0 ] && [ -e "$HOME/.config/xfce4" ]; then
    echo "ERROR: $HOME/.config/xfce4 appeared during the run (private XDG dirs leaked)" >&2
    exit 1
fi
