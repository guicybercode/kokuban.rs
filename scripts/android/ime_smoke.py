#!/usr/bin/env python3
"""Exercise an installed Latin IME by touch, including its accent popup.

ASCII adb keys only prepare a shell `read`. The tested text, Backspace and Enter
come from taps on the actual keyboard. Screenshots, UI XML, callback counts and
the shell's UTF-8 result are retained; preedit is claimed only if observed.
The strict composition check adds Korean 2-set in Gboard through its settings UI,
types Hangul by touch, and restores the active English layout. Requires an
installed debuggable Kokuban APK and an English Latin keyboard; strict mode
requires Gboard with its Korean layout available.
"""

import argparse
import json
from pathlib import Path
import re
import shlex
import time
import xml.etree.ElementTree as ET

from device import Device
from smoke import eventually


def node_bounds(node):
    match = re.fullmatch(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", node.get("bounds", ""))
    if not match:
        raise ValueError(f"Invalid UI bounds: {node.get('bounds')!r}")
    left, top, right, bottom = map(int, match.groups())
    if right <= left or bottom <= top:
        raise ValueError("UI node has no touchable area")
    return left, top, right, bottom


def find_node(root, labels, package):
    wanted = {label.casefold() for label in labels}
    candidates = []
    for node in root.iter("node"):
        if node.get("package") != package or node.get("enabled", "true") != "true":
            continue
        values = {node.get("text", "").casefold(), node.get("content-desc", "").casefold()}
        if not values.intersection(wanted):
            continue
        try:
            left, top, right, bottom = node_bounds(node)
            candidates.append(((right - left) * (bottom - top), node))
        except ValueError:
            continue
    if not candidates:
        raise LookupError(f"Keyboard/control node {sorted(wanted)!r} not found in {package}")
    return min(candidates, key=lambda item: item[0])[1]


def center(node):
    left, top, right, bottom = node_bounds(node)
    return (left + right) // 2, (top + bottom) // 2


def language_node(root, languages, package):
    """Language rows can include a region or layout after the language name."""
    labels = set()
    for node in root.iter("node"):
        if node.get("package") != package:
            continue
        for value in (node.get("text", ""), node.get("content-desc", "")):
            if any(re.match(rf"^{re.escape(language)}(?:$|[\s,(])", value, re.IGNORECASE)
                   for language in languages):
                labels.add(value)
    return find_node(root, labels or languages, package)


def callback_counts(log):
    return {
        operation: len(re.findall(rf"ime callback operation={operation} nonempty=true", log))
        for operation in ("preedit", "commit")
    }


def choice_active(node):
    # Gboard layout cards expose selected; the system picker exposes checked.
    return node.get("selected") == "true" or node.get("checked") == "true"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial")
    parser.add_argument("--package", default="com.kokuban.terminal")
    parser.add_argument("--ime", help="Installed input-method component; defaults to the device's current IME")
    parser.add_argument("--require-preedit", action="store_true", help="Also require real Korean 2-set composition with nonempty preedit callbacks")
    parser.add_argument("--output", type=Path, default=Path("target/android-evidence/ime"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    device = Device(args.serial)
    device.wait_boot()
    package = args.package
    component = device.shell("cmd", "package", "resolve-activity", "--brief", package).splitlines()[-1]
    if not component.startswith(package + "/"):
        raise RuntimeError(f"No Kokuban launcher: {component}")
    original = {
        ("secure", "default_input_method"): device.shell("settings", "get", "secure", "default_input_method"),
        ("secure", "show_ime_with_hard_keyboard"): device.shell("settings", "get", "secure", "show_ime_with_hard_keyboard"),
        ("system", "accelerometer_rotation"): device.shell("settings", "get", "system", "accelerometer_rotation"),
        ("system", "user_rotation"): device.shell("settings", "get", "system", "user_rotation"),
    }
    ime = args.ime or original[("secure", "default_input_method")]
    if "/" not in ime:
        raise RuntimeError(f"No default IME is installed: {ime}")
    ime_package = ime.split("/", 1)[0]
    private = device.shell("run-as", package, "pwd")
    marker = f"{private}/files/ime-smoke-result.txt"
    cjk_marker = f"{private}/files/ime-smoke-hangul.txt"
    trace = "files/config/kokuban/trace-frames"
    had_trace = device.shell("run-as", package, "sh", "-c", f"test -f {trace} && echo yes", check=False) == "yes"
    results = {"device": device.details(), "ime": ime, "checks": [], "status": "failed"}
    ime_details = device.shell("dumpsys", "package", ime_package)
    results["ime_version"] = {
        key: match.group(1) if (match := re.search(rf"\b{key}=([^\s]+)", ime_details)) else "unknown"
        for key in ("versionName", "versionCode")
    }
    results["system_locale"] = (device.shell("getprop", "persist.sys.locale")
                                or device.shell("getprop", "ro.product.locale"))
    start_time = device.shell("date", "+%m-%d %H:%M:%S.000")
    capture_index = 0
    held_pointer = None
    korean_added = False

    def save_results():
        (args.output / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n")

    def dump(label):
        nonlocal capture_index
        capture_index += 1
        results["last_capture"] = label
        save_results()
        remote = "/sdcard/kokuban-ime-ui.xml"
        # The focused app remains the active window while the IME is shown.
        # Include interactive windows to expose the keyboard and accent popup.
        device.shell("uiautomator", "dump", "--compressed", "--windows", remote, timeout=30)
        xml = device.shell("cat", remote)
        (args.output / f"{capture_index:02d}-{label}.xml").write_text(xml + "\n")
        return ET.fromstring(xml)

    def locate(label, alternatives, owner=ime_package):
        deadline = time.monotonic() + 20
        while True:
            root = dump(label)
            try:
                return root, find_node(root, alternatives, owner)
            except LookupError:
                if time.monotonic() >= deadline:
                    raise
                # Page transitions can briefly expose only SystemUI. Poll the
                # accessibility tree; never repeat the action that opened it.
                time.sleep(0.2)

    def tap(label, alternatives, owner=ime_package):
        root, node = locate(label, alternatives, owner)
        x, y = center(node)
        device.shell("input", "tap", str(x), str(y))
        return root

    def ime_visible():
        state = device.shell("dumpsys", "input_method")
        return "mInputShown=true" in state or "isInputViewShown=true" in state

    def return_to_terminal():
        for index in range(6):
            root = dump(f"return-terminal-{index}")
            if any(node.get("package") == package and node.get("class") == "android.widget.Button"
                   for node in root.iter("node")):
                return
            device.shell("input", "keyevent", "KEYCODE_BACK")
        raise AssertionError("Gboard settings did not return to the terminal")

    def switch_language(languages, label):
        if not ime_visible():
            tap(f"{label}-show-keyboard", ["Show or hide keyboard"], package)
            eventually(ime_visible, True, 30)
        root = dump(f"{label}-space")
        x, y = center(find_node(root, ["Space"], ime_package))
        device.shell("input", "swipe", str(x), str(y), str(x), str(y), "900")
        choices = dump(f"{label}-language-picker")
        device.screenshot(args.output / f"{label}-language-picker.png")
        try:
            choice = language_node(choices, languages, ime_package)
        except LookupError:
            # LatinIME delegates the Space long-press to the system's input
            # method picker, whose nodes belong to Android rather than Gboard.
            choice = language_node(choices, languages, "android")
        x, y = center(choice)
        device.shell("input", "tap", str(x), str(y))
        active_ime = device.shell("settings", "get", "secure", "default_input_method")
        if active_ime != ime:
            raise AssertionError(f"Language picker selected a different input method: {active_ime}")
        selected = dump(f"{label}-keyboard")
        device.screenshot(args.output / f"{label}-keyboard.png")
        return selected

    def add_korean_layout():
        nonlocal korean_added
        if ime_package != "com.google.android.inputmethod.latin":
            raise AssertionError("Strict Hangul fixture currently requires Gboard; composition is unverified for this IME")
        root = dump("gboard-toolbar")
        try:
            settings = find_node(root, ["Settings"], ime_package)
        except LookupError:
            tap("gboard-features", ["Open features menu"])
            settings = find_node(dump("gboard-settings-button"), ["Settings"], ime_package)
        device.shell("input", "tap", *map(str, center(settings)))
        tap("gboard-languages", ["Languages"])
        tap("gboard-add-keyboard", ["Add keyboard", "Add Keyboard"])
        root = dump("gboard-language-list")
        try:
            korean = language_node(root, ["Korean", "한국어"], ime_package)
        except LookupError:
            tap("gboard-search-languages", ["Search", "Search language", "Search languages"])
            # ASCII injection only navigates settings; tested Hangul uses real key taps.
            device.type_text("Korean")
            korean = language_node(dump("gboard-korean-search"), ["Korean", "한국어"], ime_package)
        device.shell("input", "tap", *map(str, center(korean)))
        _, layout = locate("gboard-korean-layout", ["2-set", "Dubeolsik", "두벌식"])
        if not choice_active(layout):
            device.shell("input", "tap", *map(str, center(layout)))
            _, layout = locate("gboard-korean-selected", ["2-set", "Dubeolsik", "두벌식"])
            if not choice_active(layout):
                raise AssertionError("Gboard did not select its Korean 2-set layout")
        tap("gboard-save-korean", ["Done"])
        korean_added = True
        device.screenshot(args.output / "gboard-configured-languages.png")
        return_to_terminal()

    try:
        device.shell("settings", "put", "secure", "show_ime_with_hard_keyboard", "1")
        device.shell("settings", "put", "system", "accelerometer_rotation", "0")
        device.shell("settings", "put", "system", "user_rotation", "0")
        if args.ime:
            device.shell("ime", "set", ime)
        device.shell("run-as", package, "mkdir", "-p", "files/config/kokuban")
        device.shell("run-as", package, "touch", trace)
        device.shell("run-as", package, "rm", "-f", marker)
        device.shell("am", "force-stop", package)
        device.shell("am", "start", "-W", "-n", component)
        process = device.pid(package)
        eventually(lambda: "first frame presented" in device.adb("logcat", "-d", "--pid", process), True, 45)

        controls = dump("controls")
        density = device.shell("wm", "density")
        densities = re.findall(r"density:\s*(\d+)", density)
        if not densities:
            raise AssertionError(f"Cannot determine Android density: {density}")
        scale = int(densities[-1]) / 160
        buttons = [node for node in controls.iter("node")
                   if node.get("package") == package and node.get("class") == "android.widget.Button"]
        if len(buttons) < 6:
            raise AssertionError("Native accessibility toolbar nodes were not exposed")
        for node in buttons:
            left, top, right, bottom = node_bounds(node)
            if min(right - left, bottom - top) + 1 < 48 * scale:
                raise AssertionError(f"Toolbar target smaller than 48dp: {node.attrib}")
        results["checks"].append("native accessible control bounds are at least 48dp")

        command = f'printf READY > {shlex.quote(marker)}; IFS= read -r K; printf "$K" > {shlex.quote(marker)}'
        device.type_text(command)
        device.shell("input", "keyevent", "KEYCODE_ENTER")
        eventually(lambda: device.shell("run-as", package, "cat", marker, check=False), "READY", 45)
        # NativeActivity/IME policies can show the keyboard on first focus.
        # This control toggles state, so only tap when it is currently hidden.
        if not ime_visible():
            tap("open-keyboard", ["Show or hide keyboard"], package)
        eventually(ime_visible, True, 30)
        device.screenshot(args.output / "keyboard-open.png")

        for letter in "caf":
            tap(f"key-{letter}", [letter])
        root = dump("accent-start")
        x, y = center(find_node(root, ["e"], ime_package))
        held_pointer = (x, y)
        device.shell("input", "motionevent", "DOWN", str(x), str(y))
        time.sleep(0.8)
        device.screenshot(args.output / "accent-popup.png")
        popup = dump("accent-popup")
        accent_x, accent_y = center(find_node(popup, ["é", "e, acute", "e acute", "e with acute"], ime_package))
        device.shell("input", "motionevent", "MOVE", str(accent_x), str(accent_y))
        # Hold on the desired key before lifting, and retain its visual state.
        # This distinguishes a missed popup selection from lost committed text.
        time.sleep(0.2)
        device.screenshot(args.output / "accent-selected.png")
        dump("accent-selected")
        device.shell("input", "motionevent", "UP", str(accent_x), str(accent_y))
        held_pointer = None
        tap("key-to-delete", ["x"])
        tap("backspace", ["delete", "backspace"])
        device.screenshot(args.output / "composed-before-enter.png")
        tap("enter", ["enter", "return", "new line", "done"])
        eventually(lambda: device.shell("run-as", package, "cat", marker, check=False), "café", 45)
        results["checks"].append("real IME taps, accent popup, Backspace and Enter produce café via PTY")
        results["utf8_result"] = "café"
        device.screenshot(args.output / "result.png")

        tap("hide-keyboard", ["Show or hide keyboard"], package)
        eventually(ime_visible, False, 30)
        device.screenshot(args.output / "keyboard-hidden.png")
        tap("reopen-keyboard", ["Show or hide keyboard"], package)
        eventually(ime_visible, True, 30)
        results["checks"].append("explicit keyboard control hides and reopens the IME")
        logs = device.adb("logcat", "-d", "--pid", process, "-T", start_time, check=False)
        counts = callback_counts(logs)
        results["latin_callbacks"] = counts
        if counts["commit"] == 0:
            raise AssertionError("Text arrived without a committed IME transaction")
        if args.require_preedit:
            add_korean_layout()
            command = f'printf READY > {shlex.quote(cjk_marker)}; IFS= read -r K; printf "$K" > {shlex.quote(cjk_marker)}'
            device.type_text(command)
            device.shell("input", "keyevent", "KEYCODE_ENTER")
            eventually(lambda: device.shell("run-as", package, "cat", cjk_marker, check=False), "READY", 45)
            switch_language(["Korean", "한국어"], "korean")
            baseline = callback_counts(device.adb("logcat", "-d", "--pid", process, "-T", start_time, check=False))
            tap("hangul-kiyeok", ["ㄱ", "기역", "Giyeok", "Kiyeok"])
            tap("hangul-a", ["ㅏ", "아"])
            device.screenshot(args.output / "hangul-before-enter.png")
            tap("hangul-enter", ["Enter", "Return", "New line", "Done"])
            eventually(lambda: device.shell("run-as", package, "cat", cjk_marker, check=False), "가", 45)
            current = callback_counts(device.adb("logcat", "-d", "--pid", process, "-T", start_time, check=False))
            results["hangul_callbacks"] = {key: current[key] - baseline[key] for key in current}
            results["hangul_utf8_result"] = "가"
            results["hangul_layout"] = "Korean 2-set"
            device.screenshot(args.output / "hangul-result.png")
            if results["hangul_callbacks"]["preedit"] == 0:
                raise AssertionError("Hangul reached the PTY without a nonempty preedit callback; composition remains unverified")
            results["checks"].append("real Korean 2-set taps compose ㄱ + ㅏ into 가 with preedit and Enter delivery via PTY")
        results["ime_callbacks"] = callback_counts(device.adb("logcat", "-d", "--pid", process, "-T", start_time, check=False))
        results["composition"] = "nonempty preedit and committed PTY text observed" if results["ime_callbacks"]["preedit"] else "intermediate preedit not observed"
        results["status"] = "passed"
    except Exception as error:
        results["error"] = str(error)
        raise
    finally:
        # An ADB failure can also make screenshot/settings restoration fail.
        # Persist the original scenario result before attempting device cleanup.
        save_results()
        cleanup_errors = []

        def cleanup(label, action):
            try:
                return action()
            except Exception as error:
                cleanup_errors.append(f"{label}: {error}")
                return None

        if held_pointer:
            cleanup("release pointer", lambda: device.shell("input", "motionevent", "UP", str(held_pointer[0]), str(held_pointer[1]), check=False))
        if results["status"] != "passed":
            cleanup("failure screenshot", lambda: device.screenshot(args.output / "failure.png"))
            cleanup("failure UI", lambda: dump("failure"))
        if korean_added:
            try:
                return_to_terminal()
                restored = switch_language(["English"], "restore-english")
                find_node(restored, ["q"], ime_package)
                results["restored_layout"] = "English"
            except Exception as error:
                results["restore_error"] = str(error)
                results["status"] = "failed"
        for (namespace, key), value in original.items():
            if key == "default_input_method" and value != "null":
                cleanup("restore input method", lambda: device.shell("ime", "set", value, check=False))
            elif value == "null":
                cleanup(f"restore {key}", lambda: device.shell("settings", "delete", namespace, key, check=False))
            else:
                cleanup(f"restore {key}", lambda: device.shell("settings", "put", namespace, key, value, check=False))
        if not had_trace:
            cleanup("remove trace flag", lambda: device.shell("run-as", package, "rm", "-f", trace, check=False))
        cleanup("remove UI dump", lambda: device.shell("rm", "-f", "/sdcard/kokuban-ime-ui.xml", check=False))
        cleanup("capture logcat", lambda: (args.output / "logcat.txt").write_text(device.adb("logcat", "-d", "-T", start_time, check=False)))
        if cleanup_errors:
            results["cleanup_errors"] = cleanup_errors
            results["status"] = "failed"
            results.setdefault("error", "IME cleanup or evidence capture failed")
        save_results()
    if results["status"] != "passed":
        raise AssertionError(results.get("restore_error", "IME validation failed"))
    print(json.dumps(results, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
