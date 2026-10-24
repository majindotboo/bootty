"""Owned native GTK fixture for the ignored Linux AT-SPI acceptance test."""
import json
import sys
import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, GLib, Gtk

cover = "cover" in sys.argv
GLib.set_prgname("bootty-atspi-cover" if cover else "bootty-atspi-probe")
window = Gtk.Window(title="Bootty AT-SPI cover" if cover else "Bootty AT-SPI probe")
window.set_default_size(480, 320)
if "offset" in sys.argv:
    window.move(700, 100)
window.set_accept_focus(cover)
window.set_focus_on_map(cover)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
window.add(box)
entry = Gtk.Entry(text="original")
entry.get_accessible().set_name("Probe text")
readonly = Gtk.Entry(text="read-only")
readonly.set_editable(False)
readonly.get_accessible().set_name("Probe readonly")
password = Gtk.Entry(text="do-not-expose")
password.set_visibility(False)
password.get_accessible().set_name("Probe password")
button = Gtk.Button(label="Probe press")
label = Gtk.Label(label="presses=0")
for widget in [entry, readonly, password, button, label]:
    box.pack_start(widget, False, False, 4)
pointer = Gtk.EventBox()
pointer.set_size_request(400, 80)
pointer.get_accessible().set_name("Probe pointer")
pointer.add(Gtk.Label(label="Independent pointer input"))
pointer.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_RELEASE_MASK)
box.pack_start(pointer, True, True, 0)
presses = 0

def report(event):
    print(json.dumps({"event": event, "active": window.is_active(),
                      "display": Gdk.Display.get_default().__gtype__.name}), flush=True)
    return False

def pressed(_button):
    global presses
    presses += 1
    label.set_text(f"presses={presses}")
    report("pressed")

def input_event(_widget, event):
    print(json.dumps({"event": "input", "type": event.type.value_nick,
                      "button": getattr(event, "button", None) if event.type in [Gdk.EventType.BUTTON_PRESS, Gdk.EventType.BUTTON_RELEASE] else None,
                      "key": Gdk.keyval_name(event.keyval) if event.type in [Gdk.EventType.KEY_PRESS, Gdk.EventType.KEY_RELEASE] else None,
                      "state": int(event.state), "active": window.is_active()}), flush=True)
    return True

pointer.connect("button-press-event", input_event)
pointer.connect("button-release-event", input_event)
window.connect("key-press-event", input_event)
window.connect("key-release-event", input_event)

button.connect("clicked", pressed)
entry.connect("changed", lambda _entry: report("changed"))
window.connect("map-event", lambda *_args: GLib.idle_add(report, "ready") and False)
window.connect("notify::is-active", lambda *_args: report("focus"))
window.connect("destroy", Gtk.main_quit)
window.show_all()
Gtk.main()
