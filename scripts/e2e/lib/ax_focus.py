#!/usr/bin/env python3
"""Read an application's *real* focused window through Accessibility.

The e2e scenarios need this because request acceptance and API return codes cannot show that the
exact window got focus: only the app's own AXFocusedWindow can, mapped back to a CGWindowID.

It is a module (imported by the scenario blocks) and a self-testing script
(`python3 -B scripts/e2e/lib/ax_focus.py`), so an unreadable or misdeclared symbol cannot pass
silently as "no window focused".
"""

import ctypes
import sys

_APP_SERVICES = "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices"
_CORE_FOUNDATION = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
_HISERVICES = "/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices"

CF_STRING_UTF8 = 0x08000100
_AX_ERROR_SUCCESS = 0

app_services = ctypes.CDLL(_APP_SERVICES)
core_foundation = ctypes.CDLL(_CORE_FOUNDATION)
hiservices = ctypes.CDLL(_HISERVICES)

app_services.AXUIElementCreateApplication.restype = ctypes.c_void_p
app_services.AXUIElementCreateApplication.argtypes = [ctypes.c_int32]
app_services.AXUIElementCopyAttributeValue.restype = ctypes.c_int32
app_services.AXUIElementCopyAttributeValue.argtypes = [
    ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)
]
app_services.AXUIElementSetMessagingTimeout.restype = ctypes.c_int32
app_services.AXUIElementSetMessagingTimeout.argtypes = [ctypes.c_void_p, ctypes.c_float]
hiservices._AXUIElementGetWindow.restype = ctypes.c_int32
hiservices._AXUIElementGetWindow.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
core_foundation.CFStringCreateWithCString.restype = ctypes.c_void_p
core_foundation.CFStringCreateWithCString.argtypes = [
    ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32
]
core_foundation.CFRelease.argtypes = [ctypes.c_void_p]
app_services.AXValueGetValue.restype = ctypes.c_bool
app_services.AXValueGetValue.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p]
app_services.AXUIElementPerformAction.restype = ctypes.c_int32
app_services.AXUIElementPerformAction.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
core_foundation.CFArrayGetCount.restype = ctypes.c_long
core_foundation.CFArrayGetCount.argtypes = [ctypes.c_void_p]
core_foundation.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
core_foundation.CFArrayGetValueAtIndex.argtypes = [ctypes.c_void_p, ctypes.c_long]

# AXValueType: kAXValueCGPointType = 1, kAXValueCGSizeType = 2 (HIServices AXValue.h).
_AX_VALUE_CG_POINT = 1
_AX_VALUE_CG_SIZE = 2


class _CGPoint(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


class _CGSize(ctypes.Structure):
    _fields_ = [("w", ctypes.c_double), ("h", ctypes.c_double)]

# 50ms: a hung app must not stall the scenario, the same bound the app itself uses for AX reads.
_AX_MESSAGING_TIMEOUT_SECONDS = 0.05


def focused_window_id(pid):
    """The CGWindowID of `pid`'s focused window, or None when the read cannot answer.

    `None` covers both "this app reports no focused window" and "the read is not permitted"; a
    caller that needs to tell those apart probes a few apps before judging (see the scenarios).
    """
    key = core_foundation.CFStringCreateWithCString(None, b"AXFocusedWindow", CF_STRING_UTF8)
    if not key:
        return None
    app = app_services.AXUIElementCreateApplication(pid)
    if not app:
        core_foundation.CFRelease(key)
        return None
    try:
        app_services.AXUIElementSetMessagingTimeout(app, _AX_MESSAGING_TIMEOUT_SECONDS)
        value = ctypes.c_void_p()
        if app_services.AXUIElementCopyAttributeValue(app, key, ctypes.byref(value)) != _AX_ERROR_SUCCESS:
            return None
        if not value:
            return None
        try:
            window_id = ctypes.c_uint32(0)
            if hiservices._AXUIElementGetWindow(value, ctypes.byref(window_id)) != _AX_ERROR_SUCCESS:
                return None
            return int(window_id.value)
        finally:
            core_foundation.CFRelease(value)
    finally:
        core_foundation.CFRelease(app)
        core_foundation.CFRelease(key)


def _cfstring(text):
    return core_foundation.CFStringCreateWithCString(None, text.encode("utf-8"), CF_STRING_UTF8)


def _copy_attribute(element, name):
    """A +1 CF value for `name`, or None. The caller releases the returned pointer."""
    key = _cfstring(name)
    if not key:
        return None
    try:
        value = ctypes.c_void_p()
        if (
            app_services.AXUIElementCopyAttributeValue(element, key, ctypes.byref(value))
            != _AX_ERROR_SUCCESS
        ):
            return None
        return value if value.value else None
    finally:
        core_foundation.CFRelease(key)


def _application(pid):
    app = app_services.AXUIElementCreateApplication(pid)
    if app:
        app_services.AXUIElementSetMessagingTimeout(app, _AX_MESSAGING_TIMEOUT_SECONDS)
    return app


# AX error codes (HIServices AXError.h) that mean "this call does not apply to this element"
# rather than "the read did not answer": Finder's desktop element is one, it can never match by id,
# and it answers kAXErrorIllegalArgument (measured). Every other failure -- above all
# kAXErrorCannotComplete, a timed-out read -- may answer on a later call, so it must stay
# distinguishable from a real absence.
_AX_ERROR_ILLEGAL_ARGUMENT = -25201
_AX_ERROR_ATTRIBUTE_UNSUPPORTED = -25205
_AX_ERROR_NO_VALUE = -25212


def _window_number_from(error, value):
    """(window_id, readable) for an _AXUIElementGetWindow result (pure; see the selftest)."""
    if error == _AX_ERROR_SUCCESS:
        return int(value), True
    if error in (_AX_ERROR_ILLEGAL_ARGUMENT, _AX_ERROR_ATTRIBUTE_UNSUPPORTED, _AX_ERROR_NO_VALUE):
        return None, True
    return None, False


def _window_id_checked(window):
    window_id = ctypes.c_uint32(0)
    error = hiservices._AXUIElementGetWindow(window, ctypes.byref(window_id))
    return _window_number_from(error, window_id.value)


def _window_id(window):
    return _window_id_checked(window)[0]


def window_ids_checked(pid):
    """(ids, complete). complete is False when a read that could answer did not.

    A scenario that treats the set as a baseline must use this: a failed read mistaken for an empty
    baseline would turn a pre-existing window into a "new" one and later close it. An element with
    no window number at all (Finder's desktop element) is not a failed read and does not fail this.
    """
    app = _application(pid)
    if not app:
        return [], False
    ids = []
    complete = True
    try:
        array = _copy_attribute(app, "AXWindows")
        if not array:
            return [], False
        try:
            for index in range(max(0, core_foundation.CFArrayGetCount(array))):
                element = core_foundation.CFArrayGetValueAtIndex(array, index)
                if not element:
                    complete = False
                    continue
                window_id, readable = _window_id_checked(element)
                if not readable:
                    complete = False
                    continue
                # An element that genuinely has no window number (Finder's desktop element) is
                # skipped without failing the enumeration: it can never match by id, and a real
                # absence must not read the same as a read that did not answer.
                if window_id is not None:
                    ids.append(window_id)
        finally:
            core_foundation.CFRelease(array)
    finally:
        core_foundation.CFRelease(app)
    return ids, complete


def window_ids(pid):
    """Every AXWindow CGWindowID of `pid`; [] when it has none or the read cannot answer.

    Callers that must tell "no windows" from "the read failed" use `window_ids_checked`.
    """
    return window_ids_checked(pid)[0]


def rect_close(a, b, tolerance=2.0):
    """Whether two (x, y, w, h) rectangles agree within `tolerance` points."""
    return len(a) == len(b) == 4 and all(abs(x - y) <= tolerance for x, y in zip(a, b))


def target_reached(readings, expected, tolerance=2.0):
    """True only when the last two readings both match `expected`.

    Two equal readings that do NOT match the target are the counter-example: an animation's pause can
    produce them, so "the frame stopped changing" alone must never be read as "the settle finished".
    """
    return len(readings) >= 2 and all(
        rect_close(reading, expected, tolerance) for reading in readings[-2:]
    )


def window_frame(pid, cgwid):
    """(x, y, w, h) of `pid`'s window `cgwid` in AX coordinates, or None.

    The window element is borrowed from the AXWindows array and used while that array is alive, so
    there is no retain/release window for the caller to get wrong.
    """
    app = _application(pid)
    if not app:
        return None
    try:
        array = _copy_attribute(app, "AXWindows")
        if not array:
            return None
        try:
            for index in range(max(0, core_foundation.CFArrayGetCount(array))):
                element = core_foundation.CFArrayGetValueAtIndex(array, index)
                if not element or _window_id(element) != cgwid:
                    continue
                position = _copy_attribute(element, "AXPosition")
                size = _copy_attribute(element, "AXSize")
                if not position or not size:
                    if position:
                        core_foundation.CFRelease(position)
                    if size:
                        core_foundation.CFRelease(size)
                    return None
                try:
                    point = _CGPoint()
                    extent = _CGSize()
                    if not app_services.AXValueGetValue(
                        position, _AX_VALUE_CG_POINT, ctypes.byref(point)
                    ):
                        return None
                    if not app_services.AXValueGetValue(
                        size, _AX_VALUE_CG_SIZE, ctypes.byref(extent)
                    ):
                        return None
                    return (point.x, point.y, extent.w, extent.h)
                finally:
                    core_foundation.CFRelease(position)
                    core_foundation.CFRelease(size)
            return None
        finally:
            core_foundation.CFRelease(array)
    finally:
        core_foundation.CFRelease(app)


def close_window(pid, cgwid):
    """Press `pid`'s window `cgwid` close button. Only a window the caller owns must be passed."""
    app = _application(pid)
    if not app:
        return False
    try:
        array = _copy_attribute(app, "AXWindows")
        if not array:
            return False
        try:
            for index in range(max(0, core_foundation.CFArrayGetCount(array))):
                element = core_foundation.CFArrayGetValueAtIndex(array, index)
                if not element or _window_id(element) != cgwid:
                    continue
                button = _copy_attribute(element, "AXCloseButton")
                if not button:
                    return False
                try:
                    key = _cfstring("AXPress")
                    if not key:
                        return False
                    try:
                        return (
                            app_services.AXUIElementPerformAction(button, key)
                            == _AX_ERROR_SUCCESS
                        )
                    finally:
                        core_foundation.CFRelease(key)
                finally:
                    core_foundation.CFRelease(button)
            return False
        finally:
            core_foundation.CFRelease(array)
    finally:
        core_foundation.CFRelease(app)


def _selftest():
    # A pid that cannot exist must answer None instead of raising or inventing an id -- that is the
    # counter-example that makes this a gate rather than a hopeful read.
    bogus = 999_999
    assert focused_window_id(bogus) is None, "a bogus pid must not produce a window id"
    # A *live* process without a focused window must answer None as well: this is the shape a failed
    # switch takes ("the app is there, its focus window is not"), and it must not raise either.
    assert focused_window_id(1) is None, "a live process without a focused window must answer None"
    # And the call must be repeatable without leaking into the next read.
    assert focused_window_id(bogus) is None, "the second read must answer the same way"
    # The window enumeration/frame/close helpers must refuse an owner that has no such window rather
    # than invent one, and closing an unknown window must never be aimed at a guess.
    assert window_ids(bogus) == [], "a bogus pid must not produce windows"
    assert window_frame(bogus, 1) is None, "a bogus pid must not produce a frame"
    assert close_window(bogus, 1) is False, "closing an unknown window must be refused"
    # An incomplete enumeration must be distinguishable from an empty one, or a failed baseline
    # would promote a pre-existing window into a scenario-created one.
    assert window_ids_checked(bogus) == ([], False), "a bogus pid is an incomplete read"
    # A window that genuinely has no number must NOT read as a failed read: Finder's desktop element
    # answers kAXErrorNoValue, and counting that as incomplete defeated the baseline on every run
    # (the original finder-quick-actions failure). A read that did not answer must still fail it.
    assert _window_number_from(0, 48906) == (48906, True), "success carries the number"
    assert _window_number_from(-25201, 0) == (None, True), "illegal-argument is a real absence"
    assert _window_number_from(-25212, 0) == (None, True), "no value is a real absence"
    assert _window_number_from(-25205, 0) == (None, True), "unsupported is a real absence"
    assert _window_number_from(-25204, 0) == (None, False), "cannot-complete is unreadable"
    assert _window_number_from(-25202, 0) == (None, False), "an invalid element is unreadable"
    # The settle predicate rejects the early-equal-readings failure: equality alone is not settled.
    intermediate = (0.0, 0.0, 100.0, 100.0)
    target = (10.0, 10.0, 200.0, 200.0)
    assert not target_reached([intermediate, intermediate], target), "equal intermediate reads"
    assert not target_reached([target], target), "one reading is not a settle"
    assert not target_reached([intermediate, target], target), "one match is not a settle"
    assert target_reached([intermediate, target, target], target), "two matches are a settle"
    assert rect_close(target, target), "identical rectangles match"
    assert not rect_close(target, (10.0, 10.0, 220.0, 200.0), 2.0), "outside tolerance differs"
    if len(sys.argv) > 1:
        for raw in sys.argv[1:]:
            print(f"{raw}\t{focused_window_id(int(raw))}")
    print("ax_focus selftest: ok")


if __name__ == "__main__":
    _selftest()
