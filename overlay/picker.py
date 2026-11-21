#!/usr/bin/env python3
"""In-game macro picker overlay for the Steam Deck (gamescope).

Started by the plugin backend (main.py) with the *system* python3, as the
desktop user. It opens a transparent, full-screen X11 window on gamescope's
Xwayland (DISPLAY=:0) tagged with GAMESCOPE_EXTERNAL_OVERLAY, so gamescope
composites it on top of the running game (same technique as mangoapp; window
plumbing taken from powange/decky-timer). Only libX11, libXext and libcairo
are used, all present on SteamOS.

Protocol: one JSON object per line on stdin:
  {"menu": {"title": str, "items": [str, ...], "selected": int}}  -> show
  {"menu": null}                                                  -> hide
EOF on stdin (backend gone) makes it exit. Input is handled by the daemon,
which grabs the gamepad while the menu is open; this window never takes input.
"""

import ctypes
import ctypes.util
import json
import math
import os
import select
import sys
import time
from ctypes import (POINTER, Structure, byref, c_char_p, c_double, c_int,
                    c_long, c_uint, c_ulong, c_void_p)


# --------------------------------------------------------------------------- #
# Native libraries
# --------------------------------------------------------------------------- #

def _load(*names):
    for name in names:
        try:
            return ctypes.CDLL(name)
        except OSError:
            continue
    return None


x11 = _load("libX11.so.6", ctypes.util.find_library("X11") or "libX11.so")
cairo = _load("libcairo.so.2", ctypes.util.find_library("cairo") or "libcairo.so")
xext = _load("libXext.so.6", ctypes.util.find_library("Xext") or "libXext.so")

if x11 is None or cairo is None:
    sys.stderr.write("overlay: libX11 or libcairo not found\n")
    sys.exit(2)


class XVisualInfo(Structure):
    _fields_ = [
        ("visual", c_void_p), ("visualid", c_ulong), ("screen", c_int),
        ("depth", c_int), ("c_class", c_int), ("red_mask", c_ulong),
        ("green_mask", c_ulong), ("blue_mask", c_ulong),
        ("colormap_size", c_int), ("bits_per_rgb", c_int),
    ]


class XSetWindowAttributes(Structure):
    _fields_ = [
        ("background_pixmap", c_ulong), ("background_pixel", c_ulong),
        ("border_pixmap", c_ulong), ("border_pixel", c_ulong),
        ("bit_gravity", c_int), ("win_gravity", c_int),
        ("backing_store", c_int), ("backing_planes", c_ulong),
        ("backing_pixel", c_ulong), ("save_under", c_int),
        ("event_mask", c_long), ("do_not_propagate_mask", c_long),
        ("override_redirect", c_int), ("colormap", c_ulong),
        ("cursor", c_ulong),
    ]


class CairoTextExtents(Structure):
    _fields_ = [
        ("x_bearing", c_double), ("y_bearing", c_double),
        ("width", c_double), ("height", c_double),
        ("x_advance", c_double), ("y_advance", c_double),
    ]


def _sig(lib, name, restype, *argtypes):
    fn = getattr(lib, name)
    fn.restype = restype
    fn.argtypes = list(argtypes)
    return fn


XOpenDisplay = _sig(x11, "XOpenDisplay", c_void_p, c_char_p)
XDefaultScreen = _sig(x11, "XDefaultScreen", c_int, c_void_p)
XRootWindow = _sig(x11, "XRootWindow", c_ulong, c_void_p, c_int)
XDisplayWidth = _sig(x11, "XDisplayWidth", c_int, c_void_p, c_int)
XDisplayHeight = _sig(x11, "XDisplayHeight", c_int, c_void_p, c_int)
XMatchVisualInfo = _sig(x11, "XMatchVisualInfo", c_int, c_void_p, c_int, c_int, c_int, POINTER(XVisualInfo))
XCreateColormap = _sig(x11, "XCreateColormap", c_ulong, c_void_p, c_ulong, c_void_p, c_int)
XCreateWindow = _sig(x11, "XCreateWindow", c_ulong, c_void_p, c_ulong, c_int, c_int, c_uint, c_uint,
                     c_uint, c_int, c_uint, c_void_p, c_ulong, POINTER(XSetWindowAttributes))
XInternAtom = _sig(x11, "XInternAtom", c_ulong, c_void_p, c_char_p, c_int)
XChangeProperty = _sig(x11, "XChangeProperty", c_int, c_void_p, c_ulong, c_ulong, c_ulong, c_int, c_int,
                       c_void_p, c_int)
XStoreName = _sig(x11, "XStoreName", c_int, c_void_p, c_ulong, c_char_p)
XMapWindow = _sig(x11, "XMapWindow", c_int, c_void_p, c_ulong)
XFlush = _sig(x11, "XFlush", c_int, c_void_p)
XPending = _sig(x11, "XPending", c_int, c_void_p)
XNextEvent = _sig(x11, "XNextEvent", c_int, c_void_p, c_void_p)
XConnectionNumber = _sig(x11, "XConnectionNumber", c_int, c_void_p)
XCloseDisplay = _sig(x11, "XCloseDisplay", c_int, c_void_p)
XUnmapWindow = _sig(x11, "XUnmapWindow", c_int, c_void_p, c_ulong)
XDestroyWindow = _sig(x11, "XDestroyWindow", c_int, c_void_p, c_ulong)
XSync = _sig(x11, "XSync", c_int, c_void_p, c_int)

c_surface_create = _sig(cairo, "cairo_xlib_surface_create", c_void_p, c_void_p, c_ulong, c_void_p, c_int, c_int)
c_create = _sig(cairo, "cairo_create", c_void_p, c_void_p)
c_push_group = _sig(cairo, "cairo_push_group", None, c_void_p)
c_pop_group_to_source = _sig(cairo, "cairo_pop_group_to_source", None, c_void_p)
c_set_operator = _sig(cairo, "cairo_set_operator", None, c_void_p, c_int)
c_paint = _sig(cairo, "cairo_paint", None, c_void_p)
c_set_rgba = _sig(cairo, "cairo_set_source_rgba", None, c_void_p, c_double, c_double, c_double, c_double)
c_new_path = _sig(cairo, "cairo_new_path", None, c_void_p)
c_new_sub_path = _sig(cairo, "cairo_new_sub_path", None, c_void_p)
c_close_path = _sig(cairo, "cairo_close_path", None, c_void_p)
c_arc = _sig(cairo, "cairo_arc", None, c_void_p, c_double, c_double, c_double, c_double, c_double)
c_rectangle = _sig(cairo, "cairo_rectangle", None, c_void_p, c_double, c_double, c_double, c_double)
c_fill = _sig(cairo, "cairo_fill", None, c_void_p)
c_fill_preserve = _sig(cairo, "cairo_fill_preserve", None, c_void_p)
c_stroke = _sig(cairo, "cairo_stroke", None, c_void_p)
c_set_line_width = _sig(cairo, "cairo_set_line_width", None, c_void_p, c_double)
c_select_font = _sig(cairo, "cairo_select_font_face", None, c_void_p, c_char_p, c_int, c_int)
c_set_font_size = _sig(cairo, "cairo_set_font_size", None, c_void_p, c_double)
c_text_extents = _sig(cairo, "cairo_text_extents", None, c_void_p, c_char_p, POINTER(CairoTextExtents))
c_move_to = _sig(cairo, "cairo_move_to", None, c_void_p, c_double, c_double)
c_show_text = _sig(cairo, "cairo_show_text", None, c_void_p, c_char_p)
c_surface_flush = _sig(cairo, "cairo_surface_flush", None, c_void_p)

CAIRO_OPERATOR_SOURCE = 1
CAIRO_OPERATOR_OVER = 2
CAIRO_FONT_SLANT_NORMAL = 0
CAIRO_FONT_WEIGHT_NORMAL = 0
CAIRO_FONT_WEIGHT_BOLD = 1

TRUE_COLOR = 4
INPUT_OUTPUT = 1
CW_BACK_PIXEL = 1 << 1
CW_BORDER_PIXEL = 1 << 3
CW_OVERRIDE_REDIRECT = 1 << 9
CW_COLORMAP = 1 << 13
XA_CARDINAL = 6
PROP_MODE_REPLACE = 0


# --------------------------------------------------------------------------- #
# Window
# --------------------------------------------------------------------------- #

class Overlay:
    def __init__(self):
        display_name = os.environ.get("DISPLAY", ":0").encode()
        self.dpy = XOpenDisplay(display_name)
        if not self.dpy:
            raise RuntimeError(f"cannot open display {display_name.decode()}")
        screen = XDefaultScreen(self.dpy)
        root = XRootWindow(self.dpy, screen)
        self.width = XDisplayWidth(self.dpy, screen)
        self.height = XDisplayHeight(self.dpy, screen)

        vinfo = XVisualInfo()
        if not XMatchVisualInfo(self.dpy, screen, 32, TRUE_COLOR, byref(vinfo)):
            raise RuntimeError("no 32-bit ARGB visual")

        attrs = XSetWindowAttributes()
        attrs.colormap = XCreateColormap(self.dpy, root, vinfo.visual, 0)
        attrs.background_pixel = 0
        attrs.border_pixel = 0
        attrs.override_redirect = 1
        mask = CW_COLORMAP | CW_BACK_PIXEL | CW_BORDER_PIXEL | CW_OVERRIDE_REDIRECT

        self.win = XCreateWindow(self.dpy, root, 0, 0, self.width, self.height, 0, 32,
                                 INPUT_OUTPUT, vinfo.visual, mask, byref(attrs))
        XStoreName(self.dpy, self.win, b"Decky Macro picker")

        # Ask gamescope to composite this window on top of everything.
        one = c_long(1)
        for atom_name in (b"GAMESCOPE_EXTERNAL_OVERLAY",):
            atom = XInternAtom(self.dpy, atom_name, 0)
            XChangeProperty(self.dpy, self.win, atom, XA_CARDINAL, 32,
                            PROP_MODE_REPLACE, byref(one), 1)

        # Empty input shape: the overlay never steals touches or clicks.
        if xext is not None:
            try:
                shape = _sig(xext, "XShapeCombineRectangles", None, c_void_p, c_ulong, c_int, c_int,
                             c_int, c_void_p, c_int, c_int, c_int)
                shape(self.dpy, self.win, 2, 0, 0, None, 0, 0, 0)  # ShapeInput, ShapeSet
            except AttributeError:
                pass

        XMapWindow(self.dpy, self.win)
        XFlush(self.dpy)

        self.surface = c_surface_create(self.dpy, self.win, vinfo.visual, self.width, self.height)
        self.cr = c_create(self.surface)
        self.fd = XConnectionNumber(self.dpy)

    def drain_events(self):
        ev = ctypes.create_string_buffer(256)  # sizeof(XEvent) is 192
        while XPending(self.dpy):
            XNextEvent(self.dpy, ev)

    # ----------------------------------------------------------------- draw

    def _text_width(self, text, size, bold):
        cr = self.cr
        c_select_font(cr, FONT_MONO if bold else FONT_SANS, CAIRO_FONT_SLANT_NORMAL,
                      CAIRO_FONT_WEIGHT_BOLD if bold else CAIRO_FONT_WEIGHT_NORMAL)
        c_set_font_size(cr, size)
        ext = CairoTextExtents()
        c_text_extents(cr, text.encode(), byref(ext))
        return ext.x_advance

    def _text(self, text, x, y, size, bold, rgba):
        cr = self.cr
        c_select_font(cr, FONT_MONO if bold else FONT_SANS, CAIRO_FONT_SLANT_NORMAL,
                      CAIRO_FONT_WEIGHT_BOLD if bold else CAIRO_FONT_WEIGHT_NORMAL)
        c_set_font_size(cr, size)
        c_set_rgba(cr, *rgba)
        c_move_to(cr, x, y)
        c_show_text(cr, text.encode())

    def _rounded(self, x, y, w, h, r):
        cr = self.cr
        c_new_path(cr)
        c_new_sub_path(cr)
        c_arc(cr, x + w - r, y + r, r, -math.pi / 2, 0)
        c_arc(cr, x + w - r, y + h - r, r, 0, math.pi / 2)
        c_arc(cr, x + r, y + h - r, r, math.pi / 2, math.pi)
        c_arc(cr, x + r, y + r, r, math.pi, 3 * math.pi / 2)
        c_close_path(cr)

    def render(self, menu):
        cr = self.cr
        c_push_group(cr)
        c_set_operator(cr, CAIRO_OPERATOR_SOURCE)
        c_set_rgba(cr, 0, 0, 0, 0)
        c_paint(cr)
        c_set_operator(cr, CAIRO_OPERATOR_OVER)

        if menu:
            items = [str(i) for i in menu.get("items", [])] or ["(no macro)"]
            selected = max(0, min(int(menu.get("selected", 0)), len(items) - 1))
            title = str(menu.get("title") or "Choose a macro")
            hint = "D-pad / stick: choose    A: play    B: close"

            scale = max(0.8, self.height / 800.0)
            title_size = 22 * scale
            item_size = 19 * scale
            hint_size = 12 * scale
            row_h = item_size * 1.9
            pad = 20 * scale
            radius = 14 * scale

            # Show at most MAX_ROWS items, scrolled around the selection.
            first = 0
            if len(items) > MAX_ROWS:
                first = min(max(0, selected - MAX_ROWS // 2), len(items) - MAX_ROWS)
            shown = items[first:first + MAX_ROWS]

            text_w = max([self._text_width(title, title_size, True),
                          self._text_width(hint, hint_size, False)] +
                         [self._text_width(it, item_size, False) for it in shown])
            w = max(text_w + pad * 2 + 24 * scale, 360 * scale)
            h = pad + title_size + 14 * scale + len(shown) * row_h + 12 * scale + hint_size + pad
            x = (self.width - w) / 2
            y = (self.height - h) / 2

            # dim the game slightly behind the menu
            c_set_rgba(cr, 0, 0, 0, 0.35)
            c_rectangle(cr, 0, 0, self.width, self.height)
            c_fill(cr)

            self._rounded(x, y, w, h, radius)
            c_set_rgba(cr, 0.07, 0.08, 0.10, 0.94)
            c_fill_preserve(cr)
            c_set_line_width(cr, 1.5 * scale)
            c_set_rgba(cr, 1, 1, 1, 0.15)
            c_stroke(cr)

            ty = y + pad + title_size
            self._text(title, x + pad, ty, title_size, True, (1, 1, 1, 1))
            ry = ty + 14 * scale
            for i, it in enumerate(shown):
                idx = first + i
                if idx == selected:
                    self._rounded(x + pad * 0.5, ry, w - pad, row_h - 4 * scale, 8 * scale)
                    c_set_rgba(cr, 0.10, 0.45, 0.95, 0.95)
                    c_fill(cr)
                    color = (1, 1, 1, 1)
                else:
                    color = (0.85, 0.86, 0.9, 0.9)
                self._text(it, x + pad, ry + row_h * 0.5 + item_size * 0.35, item_size, idx == selected, color)
                ry += row_h
            if first > 0:
                self._text("\u25b2", x + w - pad, y + pad + title_size, hint_size, False, (1, 1, 1, 0.6))
            if first + len(shown) < len(items):
                self._text("\u25bc", x + w - pad, ry, hint_size, False, (1, 1, 1, 0.6))
            self._text(hint, x + pad, ry + 12 * scale + hint_size, hint_size, False, (0.7, 0.72, 0.78, 0.9))

        c_pop_group_to_source(cr)
        c_set_operator(cr, CAIRO_OPERATOR_SOURCE)
        c_paint(cr)
        c_surface_flush(self.surface)
        XFlush(self.dpy)


FONT_SANS = b"sans-serif"
FONT_MONO = b"sans-serif"
MAX_ROWS = 8


def main():
    overlay = Overlay()
    menu = None
    buf = b""
    last_key = None
    stdin_fd = sys.stdin.fileno()
    os.set_blocking(stdin_fd, False)
    overlay.render(None)

    while True:
        readable, _, _ = select.select([stdin_fd, overlay.fd], [], [], 0.5)
        if stdin_fd in readable:
            try:
                chunk = os.read(stdin_fd, 65536)
            except BlockingIOError:
                chunk = None
            if chunk == b"":
                sys.stderr.write("picker: stdin closed, exiting\n")
                break  # backend closed the pipe
            if chunk:
                buf += chunk
                *lines, buf = buf.split(b"\n")
                for line in lines:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        menu = json.loads(line).get("menu")
                        sys.stderr.write("picker: hide\n" if menu is None else "")
                    except (ValueError, AttributeError):
                        sys.stderr.write(f"picker: bad message {line[:200]!r}\n")
        if overlay.fd in readable:
            overlay.drain_events()

        key = json.dumps(menu, sort_keys=True)
        if key != last_key:
            overlay.render(menu)
            last_key = key

    # Leave a transparent frame and remove the window explicitly, so gamescope
    # can't keep showing the last menu frame after we exit.
    overlay.render(None)
    XUnmapWindow(overlay.dpy, overlay.win)
    XDestroyWindow(overlay.dpy, overlay.win)
    XSync(overlay.dpy, 0)
    XCloseDisplay(overlay.dpy)
    sys.stderr.write("picker: window destroyed\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # logged by the backend
        sys.stderr.write(f"picker: fatal: {exc!r}\n")
        sys.exit(1)
