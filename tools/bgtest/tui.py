"""The picker run.py shows when started in a terminal with no options: one screen, arrow keys to move,
space to choose, +/- for numbers, Enter on "Start". Only the standard library (curses).

    ↑/↓ or j/k  move         space   choose / tick
    +/-         numbers      m q a n  runs: the matrix / a quick set / all / none
    Enter       start (on "Start"), or choose        Esc/Ctrl+C  quit
"""
import curses
import datetime


class Row:
    def __init__(self, kind, label, key=None, value=None, group=None, lo=None, hi=None, step=None, hint=""):
        self.kind, self.label, self.key, self.value = kind, label, key, value
        self.group, self.lo, self.hi, self.step, self.hint = group, lo, hi, step, hint

    @property
    def selectable(self):
        return self.kind != "head"


def pick(devices, playlists, apps_variants, matrix, quick, batteries=None, drain_mah_h=200.0):
    """devices: [(serial, model, how)]; playlists: {server: [names]}; apps_variants: {app: [variants]};
    matrix / quick: [(app, variant, cached)]. Returns a dict of choices, or None when left."""
    rows = [Row("head", "Phone")]
    for i, (s, m, how) in enumerate(devices):
        rows.append(Row("radio", f"{m}  {s}  ({how})", "serial", s, "serial"))
    rows.append(Row("radio", "pair a phone with Wireless debugging (no cable)", "serial", "pair", "serial"))
    rows.append(Row("check", "measure on battery: move adb to Wi-Fi and unplug the cable", "wifi", True,
                    hint="batterystats counts a cabled phone as on battery anyway; the fuel gauge only works unplugged"))

    rows.append(Row("head", "Nori build"))
    rows.append(Row("radio", "normal build (dev.nori.music)", "nori_pkg", "dev.nori.music", "nori_pkg"))
    rows.append(Row("radio", "perf build (dev.nori.music.perf)", "nori_pkg", "dev.nori.music.perf", "nori_pkg"))

    rows.append(Row("head", "Music server"))
    rows.append(Row("radio", "local test server (tools/bgtest/server.py)", "server", "local", "server"))
    rows.append(Row("radio", "the real one in ~/.music.pass", "server", "real", "server"))

    rows.append(Row("head", "Playlists (tick several: every run once per playlist)"))
    seen = []
    for server, names in playlists.items():
        for n in names:
            if n not in seen:
                seen.append(n)
                rows.append(Row("check", f"{n}  ({server})", "playlist", n == "bg-mp3", group=n,
                                hint="several: the whole set of runs goes once per ticked playlist"))

    rows.append(Row("head", "Scenarios (tick several: every run once per scenario)"))
    rows.append(Row("check", "screen off, music in the background", "scenario", True, group="screen-off"))
    rows.append(Row("check", "full-screen player on screen (screen on at the lowest brightness)", "scenario", False, group="player"))

    rows.append(Row("head", "Runs   (m: matrix  q: quick  a: all  n: none)"))
    for app, variants in apps_variants.items():
        for v in variants:
            for cached in (False, True):
                rows.append(Row("check", f"{app:10} {v}{'  (downloaded first)' if cached else ''}", "run", False, group=(app, v, cached)))

    rows.append(Row("head", "Settings"))
    rows.append(Row("number", "minutes per run", "minutes", 5, lo=1, hi=120, step=1))
    rows.append(Row("number", "repeat each", "repeat", 1, lo=1, hi=10, step=1))
    rows.append(Row("number", "skips before measuring", "skips", 3, lo=0, hi=10, step=1))
    rows.append(Row("number", "brightness with the player on screen (1–255)", "brightness", 1, lo=1, hi=255, step=10))
    rows.append(Row("number", "media volume for every run (steps, 1 = nearly silent)", "volume", 1, lo=0, hi=15, step=1))
    rows.append(Row("check", "power saving on (as on the S22 runs)", "power_save", True))
    rows.append(Row("start", "▶ Start"))

    # Defaults: the first phone, the local server, the first playlist, the quick set.
    for key in ("serial", "server", "nori_pkg"):
        first = next((r for r in rows if r.kind == "radio" and r.key == key), None)
        if first:
            first.value = (first.value, True)
    for r in rows:
        if r.kind == "radio" and not isinstance(r.value, tuple):
            r.value = (r.value, False)

    def set_runs(which):
        for r in rows:
            if r.key == "run":
                r.value = which(r.group)

    set_runs(lambda g: g in quick)
    return curses.wrapper(_loop, rows, set_runs, matrix, quick, batteries or {}, drain_mah_h)


def battery_line(serial, batteries, minutes_total, drain_mah_h, on_battery):
    """The selected phone's battery now and at the end of the planned runs, as one line."""
    b = batteries.get(serial)
    if not b:
        return ""
    level, capacity, plugged = b
    if not on_battery and plugged:
        return f"battery {level} % now; on the cable it charges during the runs"
    if not capacity:
        return f"battery {level} % now"
    used = drain_mah_h * minutes_total / 60
    end = level - 100 * used / capacity
    warn = "  ⚠ too little: charge first or run fewer" if end < 15 else ""
    return f"battery {level} % now → about {max(end, 0):.0f} % at the end (~{drain_mah_h:.0f} mAh/h for the whole phone){warn}"


def _loop(scr, rows, set_runs, matrix, quick, batteries, drain_mah_h):
    curses.curs_set(0)
    curses.use_default_colors()
    for i, c in enumerate((curses.COLOR_CYAN, curses.COLOR_GREEN, curses.COLOR_YELLOW), 1):
        curses.init_pair(i, c, -1)
    cur = next(i for i, r in enumerate(rows) if r.selectable)
    top = 0
    while True:
        h, w = scr.getmaxyx()
        scr.erase()
        n_pl = max(1, sum(1 for r in rows if r.key == "playlist" and r.value)) * max(1, sum(1 for r in rows if r.key == "scenario" and r.value))
        n_runs = sum(1 for r in rows if r.key == "run" and r.value) * n_pl
        minutes = next(r.value for r in rows if r.key == "minutes")
        repeat = next(r.value for r in rows if r.key == "repeat")
        est = sum((minutes + (3 if r.group[2] else 2)) for r in rows if r.key == "run" and r.value) * repeat * n_pl
        end = (datetime.datetime.now() + datetime.timedelta(minutes=est)).strftime("%H:%M")
        long = f"{est // 60} h {est % 60:02d} min" if est >= 60 else f"{est} min"
        title = f" Nori battery tests — {n_runs * repeat} runs, about {long}, done around {end} "
        scr.addnstr(0, 0, title.ljust(w), w - 1, curses.A_REVERSE)
        serial = next((r.value[0] for r in rows if r.kind == "radio" and r.key == "serial" and r.value[1]), None)
        wifi = next((r.value for r in rows if r.key == "wifi"), True)
        bl = battery_line(serial, batteries, est, drain_mah_h, wifi)
        scr.addnstr(1, 0, " " + bl, w - 1, curses.color_pair(3) | curses.A_BOLD if "⚠" in bl else curses.A_DIM)
        view = h - 4
        if cur < top:
            top = cur
        if cur >= top + view:
            top = cur - view + 1
        for y, i in enumerate(range(top, min(len(rows), top + view)), 2):
            r = rows[i]
            if r.kind == "head":
                text, attr = f"{r.label}", curses.color_pair(1) | curses.A_BOLD
            elif r.kind == "radio":
                text, attr = f"  ({'•' if r.value[1] else ' '}) {r.label}", 0
            elif r.kind == "check":
                text, attr = f"  [{'x' if r.value else ' '}] {r.label}", curses.color_pair(2) if r.value else 0
            elif r.kind == "number":
                text, attr = f"  {r.label}: < {r.value} >", 0
            else:
                text, attr = f"  {r.label}", curses.color_pair(3) | curses.A_BOLD
            if i == cur:
                attr |= curses.A_REVERSE
            scr.addnstr(y, 0, text, w - 1, attr)
        hint = rows[cur].hint or "↑↓ move · space choose · +/- numbers · m/q/a/n runs · Enter on Start · Esc quit"
        scr.addnstr(h - 1, 0, hint, w - 1, curses.A_DIM)
        scr.refresh()

        k = scr.getch()
        r = rows[cur]
        if k in (curses.KEY_UP, ord("k")):
            cur = _move(rows, cur, -1)
        elif k in (curses.KEY_DOWN, ord("j")):
            cur = _move(rows, cur, 1)
        elif k == curses.KEY_PPAGE:
            for _ in range(view):
                cur = _move(rows, cur, -1)
        elif k == curses.KEY_NPAGE:
            for _ in range(view):
                cur = _move(rows, cur, 1)
        elif k in (27, 3):
            return None
        elif k in (ord("m"), ord("q"), ord("a"), ord("n")):
            set_runs({"m": lambda g: g in matrix, "q": lambda g: g in quick,
                      "a": lambda g: True, "n": lambda g: False}[chr(k)])
        elif r.kind == "number" and k in (ord("+"), ord("="), curses.KEY_RIGHT, ord("l")):
            r.value = min(r.hi, r.value + r.step)
        elif r.kind == "number" and k in (ord("-"), curses.KEY_LEFT, ord("h")):
            r.value = max(r.lo, r.value - r.step)
        elif k in (ord(" "), 10, 13, curses.KEY_ENTER):
            if r.kind == "start":
                if not all(any(x.key == k and x.value for x in rows) for k in ("run", "playlist", "scenario")):
                    curses.flash()
                    continue
                return _result(rows)
            if r.kind == "check":
                r.value = not r.value
            elif r.kind == "radio":
                for x in rows:
                    if x.kind == "radio" and x.group == r.group:
                        x.value = (x.value[0], x is r)


def _move(rows, cur, d):
    i = cur
    while 0 <= i + d < len(rows):
        i += d
        if rows[i].selectable:
            return i
    return cur


def _result(rows):
    out = {"runs": [], "playlists": [], "scenarios": []}
    for r in rows:
        if r.kind == "radio" and r.value[1]:
            out[r.key] = r.value[0]
        elif r.key == "run" and r.value:
            out["runs"].append(r.group)
        elif r.key == "playlist" and r.value:
            out["playlists"].append(r.group)
        elif r.key == "scenario" and r.value:
            out["scenarios"].append(r.group)
        elif r.kind in ("check", "number") and r.key not in ("run", "playlist", "scenario"):
            out[r.key] = r.value
    return out
