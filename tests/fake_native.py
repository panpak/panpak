#!/usr/bin/python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A fake native application that owns a well-known name on the session bus and logs every method call it gets.
usage: fake_native.py NAME LOGFILE  (the object tree is whatever is called: any path, any interface)"""
import sys, gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib
name, log = sys.argv[1], sys.argv[2]
def note(s): open(log, "a").write(s + "\n")
def on_msg(conn, msg, incoming):
    if msg.get_message_type() == Gio.DBusMessageType.METHOD_CALL and incoming:
        note(f"call {msg.get_interface()}.{msg.get_member()} path={msg.get_path()} from {msg.get_sender()}")
        if msg.get_member() in ("Introspect", "Ping", "GetMachineId", "Get", "GetAll"): return msg
        r = Gio.DBusMessage.new_method_reply(msg); r.set_body(GLib.Variant("(s)", ("native-reply",))); conn.send_message(r, Gio.DBusSendMessageFlags.NONE); return None
    return msg
def on_bus(conn, n):
    conn.add_filter(on_msg); note(f"owned {n}")
def on_lost(conn, n): note(f"lost {n}")
Gio.bus_own_name(Gio.BusType.SESSION, name, Gio.BusNameOwnerFlags.NONE, on_bus, None, on_lost)
GLib.MainLoop().run()
