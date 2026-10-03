"""Persistent X11 observer for compare-terminal-frame-latency.py.

ABI source: x.org release headers, not private Display/XImage internals:
libX11-1.8.12/include/X11/{Xlib.h,Xutil.h}, libXtst-1.2.5/.../XTest.h.
Only the public XImage prefix through blue_mask is read; the function table is
never declared, allocated or accessed. XDestroyImage releases the real object.
"""

import ctypes as C
import ctypes.util
import hashlib
from pathlib import Path
import struct
import time


I, U, L, UL, P = C.c_int, C.c_uint, C.c_long, C.c_ulong, C.c_void_p


class XImagePrefix(C.Structure):
    _fields_ = [(name, I) for name in ("width", "height", "xoffset", "format")] + [
        ("data", P)] + [(name, I) for name in (
        "byte_order", "bitmap_unit", "bitmap_bit_order", "bitmap_pad", "depth", "bytes_per_line",
        "bits_per_pixel")] + [(name, UL) for name in ("red_mask", "green_mask", "blue_mask")]


class Visual(C.Structure):
    _fields_ = [("ext_data", P), ("visualid", UL), ("visual_class", I),
                ("red_mask", UL), ("green_mask", UL), ("blue_mask", UL),
                ("bits_per_rgb", I), ("map_entries", I)]


class WindowAttributes(C.Structure):
    _fields_ = [(name, I) for name in ("x", "y", "width", "height", "border_width", "depth")] + [
        ("visual", C.POINTER(Visual)), ("root", UL)] + [(name, I) for name in (
        "window_class", "bit_gravity", "win_gravity", "backing_store")] + [
        ("backing_planes", UL), ("backing_pixel", UL), ("save_under", I), ("colormap", UL),
        ("map_installed", I), ("map_state", I), ("all_event_masks", L), ("your_event_mask", L),
        ("do_not_propagate_mask", L), ("override_redirect", I), ("screen", P)]


class XErrorEvent(C.Structure):
    _fields_ = [("type", I), ("display", P), ("resourceid", UL), ("serial", UL),
                ("error_code", C.c_ubyte), ("request_code", C.c_ubyte), ("minor_code", C.c_ubyte)]


ERROR_HANDLER = C.CFUNCTYPE(I, P, C.POINTER(XErrorEvent))
ABI_SOURCES = {
    "libX11": {"url": "https://www.x.org/releases/individual/lib/libX11-1.8.12.tar.xz",
               "sha256": "fa026f9bb0124f4d6c808f9aef4057aad65e7b35d8ff43951cef0abe06bb9a9a"},
    "libXtst": {"url": "https://www.x.org/releases/individual/lib/libXtst-1.2.5.tar.xz",
                "sha256": "b50d4c25b97009a744706c1039c598f4d8e64910c9fde381994e1cae235d9242"}}


def bind(library, name, result, *arguments):
    function = getattr(library, name)
    function.restype, function.argtypes = result, list(arguments)


def libraries():
    x11 = C.CDLL(C.util.find_library("X11") or "libX11.so.6")
    xtst = C.CDLL(C.util.find_library("Xtst") or "libXtst.so.6")
    bind(x11, "XOpenDisplay", P, C.c_char_p)
    bind(x11, "XCloseDisplay", I, P)
    bind(x11, "XSetErrorHandler", P, P)
    bind(x11, "XSync", I, P, I)
    bind(x11, "XFlush", I, P)
    bind(x11, "XGetInputFocus", I, P, C.POINTER(UL), C.POINTER(I))
    bind(x11, "XQueryKeymap", I, P, C.POINTER(C.c_char))
    bind(x11, "XQueryPointer", I, P, UL, C.POINTER(UL), C.POINTER(UL),
         C.POINTER(I), C.POINTER(I), C.POINTER(I), C.POINTER(I), C.POINTER(U))
    bind(x11, "XKeysymToKeycode", C.c_ubyte, P, UL)
    bind(x11, "XGetWindowAttributes", I, P, UL, C.POINTER(WindowAttributes))
    bind(x11, "XGetImage", C.POINTER(XImagePrefix), P, UL, I, I, U, U, UL, I)
    bind(x11, "XDestroyImage", I, C.POINTER(XImagePrefix))
    bind(x11, "XServerVendor", C.c_char_p, P)
    bind(x11, "XVendorRelease", I, P)
    bind(xtst, "XTestQueryExtension", I, P, C.POINTER(I), C.POINTER(I), C.POINTER(I), C.POINTER(I))
    bind(xtst, "XTestFakeKeyEvent", I, P, U, I, UL)
    return x11, xtst


def image_xwd(image):
    """Copy owned XImage storage to a self-contained TrueColor XWD snapshot."""
    masks = [image.red_mask, image.green_mask, image.blue_mask]
    if (image.format != 2 or image.xoffset != 0 or image.byte_order not in (0, 1)
            or image.bits_per_pixel not in (24, 32) or image.depth not in (24, 32)
            or min(image.width, image.height) <= 0 or not image.data
            or image.bytes_per_line < image.width * (image.bits_per_pixel // 8)
            or image.bytes_per_line * image.height > 64 * 1024 * 1024
            or len(set(masks)) != 3
            or any(mask not in (0xff, 0xff00, 0xff0000, 0xff000000) for mask in masks)):
        raise ValueError("unsupported or invalid XImage storage")
    header = [101, 7, 2, image.depth, image.width, image.height, 0, image.byte_order,
              image.bitmap_unit, image.bitmap_bit_order, image.bitmap_pad,
              image.bits_per_pixel, image.bytes_per_line, 4, *masks, 8, 256, 0,
              image.width, image.height, 0, 0, 0]
    return struct.pack(">25I", *header) + b"\0" + C.string_at(image.data, image.bytes_per_line * image.height)


class XlibObserver:
    def __init__(self, window, frame_type, api=None):
        self.x11, self.xtst = api if api is not None else libraries()
        self.window, self.frame_type = int(window), frame_type
        self.display, self.previous_handler = None, None
        self.handler_installed = False
        self.errors, self.pressed = [], set()
        self.handler = ERROR_HANDLER(self.on_error)
        try:
            self.display = self.x11.XOpenDisplay(None)
            if not self.display:
                raise RuntimeError("XOpenDisplay failed")
            self.previous_handler = self.x11.XSetErrorHandler(C.cast(self.handler, P))
            self.handler_installed = True
            event, error, major, minor = I(), I(), I(), I()
            if not self.xtst.XTestQueryExtension(self.display, C.byref(event), C.byref(error),
                                                C.byref(major), C.byref(minor)):
                raise RuntimeError("XTEST extension unavailable")
            self.keys = {key: self.x11.XKeysymToKeycode(self.display, ord(key)) for key in ("a", "b")}
            if not all(self.keys.values()) or len(set(self.keys.values())) != 2:
                raise RuntimeError("distinct a/b keycodes unavailable")
            self.x11.XSync(self.display, 0)
            self.check_errors()
            self.provenance = {"type": "xlib", "xtest_version": [major.value, minor.value],
                               "server_vendor": self.x11.XServerVendor(self.display).decode(errors="replace"),
                               "server_release": self.x11.XVendorRelease(self.display),
                               "keycodes": self.keys, "abi_sources": ABI_SOURCES}
        except BaseException:
            self.close()
            raise

    def on_error(self, display, pointer):
        error = pointer.contents
        self.errors.append({"code": error.error_code, "request": error.request_code,
                            "minor": error.minor_code, "resource": error.resourceid})
        return 0

    def check_errors(self):
        if self.errors:
            raise RuntimeError(f"X11 protocol error: {self.errors}")

    def library_provenance(self):
        loaded = {}
        for line in Path("/proc/self/maps").read_text().splitlines():
            path = Path(line.split(None, 5)[-1])
            if path.is_absolute() and path.name.startswith(("libX11.so.", "libXtst.so.")):
                path = path.resolve()
                if str(path) not in loaded:
                    loaded[str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
        if len(loaded) < 2:
            raise RuntimeError("could not identify loaded X11/XTest libraries for provenance")
        return {**self.provenance, "loaded_libraries_sha256": loaded}

    def prepare_input(self):
        """Check focus, pressed keys and locked modifiers outside the interval."""
        focus, revert = UL(), I()
        self.x11.XGetInputFocus(self.display, C.byref(focus), C.byref(revert))
        keys = C.create_string_buffer(32)
        self.x11.XQueryKeymap(self.display, keys)
        root, child, mask = UL(), UL(), U()
        coordinates = [I() for _ in range(4)]
        pointer_status = self.x11.XQueryPointer(
            self.display, self.window, C.byref(root), C.byref(child),
            *(C.byref(value) for value in coordinates), C.byref(mask))
        self.check_errors()
        if focus.value != self.window:
            raise AssertionError("benchmark window lost keyboard focus")
        if not pointer_status or any(keys.raw) or mask.value & 0xff:
            raise AssertionError("release all keys and disable locked modifiers for the Xlib observer")

    def inject(self, key):
        code = self.keys[key]
        self.pressed.add(code)
        if not self.xtst.XTestFakeKeyEvent(self.display, code, 1, 0):
            raise RuntimeError("XTest key press failed")
        if not self.xtst.XTestFakeKeyEvent(self.display, code, 0, 0):
            raise RuntimeError("XTest key release failed")
        self.pressed.remove(code)
        self.x11.XFlush(self.display)
        self.check_errors()

    def capture(self, timeout):
        # Xlib calls are synchronous; the enclosing process/job owns the timeout.
        started = time.perf_counter_ns()
        attrs = WindowAttributes()
        status = self.x11.XGetWindowAttributes(self.display, self.window, C.byref(attrs))
        self.check_errors()
        if not status or not attrs.visual or attrs.visual.contents.visual_class != 4:
            raise RuntimeError("window attributes unavailable or visual is not TrueColor")
        if min(attrs.width, attrs.height) <= 0 or attrs.width * attrs.height > 16 * 1024 * 1024:
            raise ValueError("unsupported window dimensions")
        image = self.x11.XGetImage(self.display, self.window, 0, 0, attrs.width, attrs.height,
                                  UL(-1).value, 2)
        try:
            self.check_errors()
            if not image:
                raise RuntimeError("XGetImage returned no image")
            data = image_xwd(image.contents)
            finished = time.perf_counter_ns()
        finally:
            if image:
                self.x11.XDestroyImage(image)
        return self.frame_type(data), started, finished

    def close(self):
        if self.display:
            try:
                try:
                    for key in self.pressed:
                        self.xtst.XTestFakeKeyEvent(self.display, key, 0, 0)
                finally:
                    self.x11.XCloseDisplay(self.display)
            finally:
                self.display = None
                if self.handler_installed:
                    self.x11.XSetErrorHandler(self.previous_handler)
                    self.handler_installed = False
