#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""A fake xdg-desktop-portal Settings service, for testing theme following.

Run it on a PRIVATE session bus (never the desktop's):

    dbus-daemon --session --fork --print-address=1 --print-pid=1
    DBUS_SESSION_BUS_ADDRESS=... python3 scripts/fake-portal.py 1

It owns org.freedesktop.portal.Desktop and serves
/org/freedesktop/portal/desktop with org.freedesktop.portal.Settings:
Read, ReadOne, ReadAll and the SettingChanged signal, holding one value,
org.freedesktop.appearance color-scheme (0 no preference, 1 dark, 2 light;
"none" = the key does not exist, like a desktop without the setting).

Change it while running (emits SettingChanged like a real desktop switch):

    gdbus call --session --dest org.freedesktop.portal.Desktop \
      --object-path /org/freedesktop/portal/desktop \
      --method io.github.cyberjody.Tailglow.FakePortal.Set 2

Needs python3-dbus and python3-gi (Debian packages).
"""
import sys

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

NS, KEY = "org.freedesktop.appearance", "color-scheme"
SETTINGS = "org.freedesktop.portal.Settings"


class Portal(dbus.service.Object):
    def __init__(self, bus, value):
        super().__init__(bus, "/org/freedesktop/portal/desktop")
        self.value = value

    def _get(self, ns, key):
        if ns == NS and key == KEY and self.value is not None:
            return dbus.UInt32(self.value)
        raise dbus.exceptions.DBusException(
            f"Requested setting {ns} {key} not found",
            name="org.freedesktop.portal.Error.NotFound",
        )

    @dbus.service.method(SETTINGS, in_signature="ss", out_signature="v")
    def Read(self, ns, key):
        # The old API wraps the value in one more variant (v inside v):
        # out_signature "v" adds the outer one, variant_level=1 the inner.
        return dbus.UInt32(int(self._get(ns, key)), variant_level=1)

    @dbus.service.method(SETTINGS, in_signature="ss", out_signature="v")
    def ReadOne(self, ns, key):
        return self._get(ns, key)

    @dbus.service.method(SETTINGS, in_signature="as", out_signature="a{sa{sv}}")
    def ReadAll(self, namespaces):
        if self.value is None:
            return dbus.Dictionary({}, signature="sa{sv}")
        inner = dbus.Dictionary({KEY: dbus.UInt32(self.value)}, signature="sv")
        return dbus.Dictionary({NS: inner}, signature="sa{sv}")

    @dbus.service.signal(SETTINGS, signature="ssv")
    def SettingChanged(self, ns, key, value):
        pass

    @dbus.service.method("io.github.cyberjody.Tailglow.FakePortal", in_signature="u", out_signature="")
    def Set(self, value):
        self.value = int(value)
        print(f"fake-portal: color-scheme = {self.value}", flush=True)
        self.SettingChanged(NS, KEY, dbus.UInt32(self.value, variant_level=1))

    @dbus.service.method("io.github.cyberjody.Tailglow.FakePortal", in_signature="", out_signature="")
    def Quit(self):
        GLib.idle_add(LOOP.quit)


def main():
    arg = sys.argv[1] if len(sys.argv) > 1 else "1"
    value = None if arg == "none" else int(arg)
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName("org.freedesktop.portal.Desktop", bus, do_not_queue=True)
    portal = Portal(bus, value)
    print(f"fake-portal: ready, color-scheme = {arg}", flush=True)
    LOOP.run()
    del name, portal


LOOP = GLib.MainLoop()

if __name__ == "__main__":
    main()
