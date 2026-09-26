"""adb and a small UI driver over uiautomator for the background-playback battery harness.

Everything is found by what the screen says (text or content-desc), never by fixed coordinates, so the
same steps work at any resolution. A switch with no label of its own is found as the checkable node in
the row of a text (`switch`).
"""
import re
import subprocess
import time
import xml.etree.ElementTree as ET

SERIAL = None
# Values never to show in an error or a log (the server password: some apps put it on screen as text).
SECRETS = []


def redact(s):
    for x in SECRETS:
        if x:
            s = s.replace(x, "•" * 8)
    return s


def adb(*args, check=True, timeout=120):
    cmd = ["adb"] + (["-s", SERIAL] if SERIAL else []) + list(args)
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    if check and r.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)}: {r.stderr.strip() or r.stdout.strip()}")
    return r.stdout.replace("\r", "")


def sh(command, check=False, timeout=120):
    return adb("shell", command, check=check, timeout=timeout)


class Node:
    def __init__(self, e):
        self.text = e.get("text", "")
        self.desc = e.get("content-desc", "")
        self.rid = e.get("resource-id", "")
        self.cls = e.get("class", "")
        self.pkg = e.get("package", "")
        self.checkable = e.get("checkable") == "true"
        self.checked = e.get("checked") == "true"
        self.clickable = e.get("clickable") == "true"
        self.scrollable = e.get("scrollable") == "true"
        a, b, c, d = map(int, re.findall(r"\d+", e.get("bounds", "[0,0][0,0]")))
        self.box = (a, b, c, d)

    @property
    def label(self):
        return self.text or self.desc

    @property
    def centre(self):
        a, b, c, d = self.box
        return (a + c) // 2, (b + d) // 2

    def __repr__(self):
        return f"<{self.cls.split('.')[-1]} {self.label!r} {self.box}>"


def dump():
    """Every node on screen. uiautomator sometimes answers "null root node" right after a change."""
    for _ in range(5):
        sh("uiautomator dump /sdcard/bgtest-ui.xml >/dev/null 2>&1")
        x = sh("cat /sdcard/bgtest-ui.xml")
        if "<hierarchy" in x:
            return [Node(e) for e in ET.fromstring(x[x.index("<hierarchy"):]).iter("node")]
        time.sleep(1)
    raise RuntimeError("uiautomator gave no screen")


def _match(n, label, exact):
    if exact:
        return n.text == label or n.desc == label
    l = label.lower()
    return l in n.text.lower() or l in n.desc.lower()


def find(label, exact=True, nodes=None, n=1):
    hits = [x for x in (nodes or dump()) if _match(x, label, exact)]
    return hits[n - 1] if len(hits) >= n else None


def texts():
    return [n.label for n in dump() if n.label]


def has(label, exact=True):
    return find(label, exact) is not None


def tap_xy(x, y):
    sh(f"input tap {x} {y}")


def tap(label, exact=True, n=1, wait=10.0):
    node = wait_for(label, wait, exact, n)
    # A node reaching into the bottom strip lies under the navigation bar: a tap there is the system's
    # Back or Home, not the app's button. Scroll it up into view first.
    for _ in range(3):
        if node.box[3] < screen_size()[1] - NAV_BAR:
            break
        swipe_up(0.3)
        node = wait_for(label, wait, exact, n)
    tap_xy(*node.centre)
    time.sleep(0.8)


def try_tap(label, exact=True, wait=3.0):
    try:
        tap(label, exact, wait=wait)
        return True
    except TimeoutError:
        return False


def wait_for(label, timeout=20.0, exact=True, n=1):
    end = time.time() + timeout
    while True:
        node = find(label, exact, n=n)
        if node:
            return node
        if time.time() > end:
            raise TimeoutError(redact(f"not on screen after {timeout:.0f} s: {label!r}; screen says {texts()[:25]}"))
        time.sleep(1)


def wait_any(labels, timeout=30.0, exact=True):
    """The first of `labels` to appear."""
    end = time.time() + timeout
    while True:
        nodes = dump()
        for l in labels:
            if find(l, exact, nodes):
                return l
        if time.time() > end:
            raise TimeoutError(redact(f"none of {labels} after {timeout:.0f} s; screen says {[n.label for n in nodes if n.label][:25]}"))
        time.sleep(1)


def swipe_up(fraction=0.3):
    """Scrolls a list down by `fraction` of the screen. Starts at 68 % of the height, above any mini
    player or tab bar, and moves at a steady pace: a longer or faster swipe is taken by some lists
    (Musly's) as a drag or a fling that goes nowhere."""
    w, h = screen_size()
    sh(f"input swipe {w // 2} {int(h * 0.68)} {w // 2} {int(h * (0.68 - fraction))} 300")
    time.sleep(0.8)


def wait_scrolling(label, timeout=60.0, exact=True):
    """Waits for `label`, scrolling down the list to look for it (back to the top every few swipes, for
    a list still filling in)."""
    end = time.time() + timeout
    swipes = 0
    while True:
        node = find(label, exact)
        if node:
            return node
        if time.time() > end:
            raise TimeoutError(redact(f"not found scrolling after {timeout:.0f} s: {label!r}; screen says {texts()[:25]}"))
        if swipes < 6:
            swipe_up()
            swipes += 1
        else:
            w, h = screen_size()
            for _ in range(6):
                sh(f"input swipe {w // 2} {int(h * 0.3)} {w // 2} {int(h * 0.9)} 200")
            swipes = 0
            time.sleep(2)


def scroll_to(label, exact=True, max_swipes=12):
    for _ in range(max_swipes):
        node = find(label, exact)
        if node:
            return node
        swipe_up()
    raise TimeoutError(f"not found scrolling down: {label!r}")


_size = None
# The bottom strip the system's navigation bar may cover (3 buttons on a 2340 px Samsung: 144 px).
NAV_BAR = 170


def screen_size():
    global _size
    if not _size:
        w, h = re.findall(r"(\d+)x(\d+)", sh("wm size"))[-1]
        _size = (int(w), int(h))
    return _size


def switch(label, on, exact=True):
    """Sets the switch in the row of `label` (the checkable node overlapping its line) to `on`."""
    nodes = dump()
    anchor = find(label, exact, nodes)
    if not anchor:
        anchor = scroll_to(label, exact)
        nodes = dump()
        anchor = find(label, exact, nodes)
    top, bottom = anchor.box[1], anchor.box[3]
    row = [n for n in nodes if n.checkable and n.box[1] <= bottom + 40 and n.box[3] >= top - 40]
    if not row:
        # A row that is itself the checkable (the whole line toggles).
        row = [n for n in nodes if n.checkable and n.box[1] <= top and n.box[3] >= bottom]
    if not row:
        raise RuntimeError(f"no switch in the row of {label!r}")
    sw = min(row, key=lambda n: abs((n.box[1] + n.box[3]) / 2 - (top + bottom) / 2))
    if sw.checked != on:
        tap_xy(*sw.centre)
        time.sleep(0.8)
    return True


def type_into(label, value, exact=True):
    """Focuses the field labelled `label` (its hint, text or the field after a label) and types `value`."""
    tap(label, exact)
    sh("input keyevent KEYCODE_MOVE_END")
    for _ in range(60):
        sh("input keyevent KEYCODE_DEL")
    # `input text` takes the value as one shell word: quote it for the phone's shell.
    q = value.replace("'", "'\\''")
    sh(f"input text '{q}'")
    time.sleep(0.5)


def text(value, hide=True):
    """Types `value` into whatever has focus (no keyboard needed: `input text` injects key events), then
    closes the on-screen keyboard unless `hide` is False (a form moved through with Tab needs its focus)."""
    q = value.replace("'", "'\\''")
    sh(f"input text '{q}'")
    time.sleep(0.4)
    if hide:
        hide_keyboard()


def keyboard_shown():
    return "mInputShown=true" in sh("dumpsys input_method | grep -m1 mInputShown")


def hide_keyboard():
    """Closes the on-screen keyboard if one is up: it covers half the screen, so what is under it can
    neither be found nor tapped. One Back, and only when it is up: the platform says it is gone a
    moment late, and a second Back would leave the screen."""
    if not keyboard_shown():
        return
    sh("input keyevent KEYCODE_BACK")
    end = time.time() + 3
    while keyboard_shown() and time.time() < end:
        time.sleep(0.3)
    time.sleep(0.5)


def clear_focused():
    sh("input keycombination 113 29; input keyevent KEYCODE_DEL")  # Ctrl+A, delete


def fields():
    return [n for n in dump() if "EditText" in n.cls]


def back():
    sh("input keyevent KEYCODE_BACK")
    time.sleep(0.8)


def pixel(x, y):
    """(r, g, b) of one pixel on screen, from the raw framebuffer (no image library needed): for a
    switch that does not tell uiautomator whether it is on."""
    raw = subprocess.run(["adb"] + (["-s", SERIAL] if SERIAL else []) + ["exec-out", "screencap"],
                         capture_output=True, timeout=30).stdout
    w = int.from_bytes(raw[0:4], "little")
    # Header: width, height, format (4 bytes each), and a colour-space word on newer Android.
    header = 16 if len(raw) >= 16 + w * int.from_bytes(raw[4:8], "little") * 4 else 12
    i = header + (y * w + x) * 4
    return raw[i], raw[i + 1], raw[i + 2]


def switch_looks_on(node):
    """A Material switch whose state is not exposed: on when its track is brightly coloured, off when it
    is dark or grey. Samples the track's right end, where an on switch has its bright thumb or track."""
    a, b, c, d = node.box
    r, g, bl = pixel(c - (d - b) // 2, (b + d) // 2)
    return max(r, g, bl) - min(r, g, bl) > 40 or max(r, g, bl) > 200


def screenshot(path):
    with open(path, "wb") as f:
        f.write(subprocess.run(["adb"] + (["-s", SERIAL] if SERIAL else []) + ["exec-out", "screencap", "-p"],
                               capture_output=True, timeout=30).stdout)
