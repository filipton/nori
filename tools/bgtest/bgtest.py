#!/usr/bin/env python3
"""Background-playback battery test: each player, cleaned, logged in, playing the same playlist
shuffled with the screen off, measured by batterystats over the same stretch.

    tools/bgtest/bgtest.py                                  every app, 15 min each, once
    tools/bgtest/bgtest.py --apps nori,musly --minutes 20 --repeat 3
    tools/bgtest/bgtest.py --apps nori --variants plain,automix,automix-ml
    tools/bgtest/bgtest.py --list                           apps and Nori's variants

Per run: stop every tested app, wipe the app's data and cache (`pm clear`), log in (server from
~/.music.pass: url, blank, user, password), set the variant's settings, shuffle the playlist, skip a
few songs (each skip checked to have moved on and to play), screen off, `dumpsys battery unplug` (so a
phone on USB counts as on battery), reset batterystats, wait, then read the app's figures. Playback is
checked every minute; a run where it stopped is kept but marked.

Nothing is left changed on the phone: see phone.py for what is changed and how it is put back, which
happens on a normal end, an error, Ctrl+C or SIGTERM/SIGHUP (and at the next start if even that was
missed). Results go to build/bgtest/results/<time>/: a table (results.md), one JSON line per run
(runs.jsonl) and each run's raw dumps.
"""
import argparse
import datetime
import json
import os
import re
import sys
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

import ui  # noqa: E402
from apps import StepFailed, all_apps  # noqa: E402
from phone import Phone  # noqa: E402

APKS = os.path.join(ROOT, "build", "bgtest", "apks")


def creds(server):
    """(url, user, password): the local test server (tools/bgtest/server.py) or the one in ~/.music.pass."""
    if server == "local":
        url_file = os.path.join(ROOT, "build", "bgtest", "server", "url")
        if not os.path.exists(url_file):
            sys.exit("no local test server: run tools/bgtest/server.py first")
        return open(url_file).read().strip(), "admin", "admin"
    lines = open(os.path.expanduser("~/.music.pass")).read().split("\n")
    return lines[0].strip(), lines[2].strip(), lines[3].strip()


# ---- reading batterystats -----------------------------------------------------------------------------
def uid_of(pkg):
    m = re.search(r"appId=(\d+)", ui.sh(f"dumpsys package {pkg}"))
    return f"u0a{int(m.group(1)) - 10000}" if m else None


def dur_s(text):
    """'14m 56s 545ms' → seconds."""
    total = 0.0
    for n, u in re.findall(r"(\d+)(h|ms|m|s)", text):
        total += int(n) * {"h": 3600, "m": 60, "s": 1, "ms": 0.001}[u]
    return total


def mb(value, unit):
    return float(value) * {"B": 1 / 1e6, "KB": 1 / 1e3, "MB": 1, "GB": 1e3}[unit]


def parse(stats, uid):
    r = {}
    m = re.search(r"Time on battery: ([^(]+)\([^,]*, ([^(]+)\(", stats)
    r["seconds"] = dur_s(m.group(1)) if m else None
    # uptime below realtime: the phone slept (suspended) for the difference.
    r["asleep_s"] = round(dur_s(m.group(1)) - dur_s(m.group(2)), 1) if m else None
    m = re.search(rf"UID {uid}: ([\d.]+)[^\n]*\n\s+([^\n]+)", stats)
    if m:
        r["total_mah"] = float(m.group(1))
        # "cpu=1.12 cpu:bg=1.12 audio=15.5 (10m 0s) wakelock=4.08 (9m 45s) …": the plain totals, not the
        # per-state ones (cpu:bg); the durations in brackets are dropped, not what follows them.
        for k, v in re.findall(r"(?<![:\w])(\w+)=([\d.]+)", re.sub(r"\([^)]*\)", "", m.group(2))):
            r[f"{k}_mah"] = float(v)
        # "audio" is the phone's fixed model of its audio hardware being on (power_profile), counted in
        # every run where anything plays: batterystats charges it to an app only when that app plays on
        # the offload path, and leaves it unassigned otherwise. The app's figure is compared without it.
        # "screen" likewise: the display's modelled cost (brightness × time, the same for every app at a
        # fixed brightness, blind to what is drawn), charged to whichever app is on top. Kept apart.
        r["app_mah"] = round(r["total_mah"] - r.get("audio_mah", 0.0) - r.get("screen_mah", 0.0), 4)
    # The system services that do part of a player's work, charged to themselves rather than to the app:
    # an app decoding through Android's MediaCodec has it done in the mediacodec service (UID 1046), and
    # every app's sound goes through audioserver (1041). Counted beside the app, so players that decode
    # in their own process (Nori, Symfonium) and those that do not are compared on the same work.
    for name, u in (("mediacodec", "1046"), ("audioserver", "1041")):
        m2 = re.search(rf"\n  UID {u}: ([\d.]+)", stats)
        r[f"{name}_mah"] = float(m2.group(1)) if m2 else 0.0
    r["with_system_mah"] = round(r.get("app_mah", 0) + r["mediacodec_mah"] + r["audioserver_mah"], 4)
    mc = re.search(r"Capacity: (\d+)", stats)
    r["capacity_mah"] = int(mc.group(1)) if mc else None
    m3 = re.search(r"\n\s+audio: ([\d.]+) apps: ([\d.]+)", stats)
    r["audio_model_mah"] = float(m3.group(1)) if m3 else None
    sec = re.search(rf"\n  {uid}:\n(.*?)(?=\n  \S|\n\S|\Z)", stats, re.S)
    if sec:
        s = sec.group(1)
        m = re.search(r"Wi-Fi network: ([\d.]+)(B|KB|MB|GB) received, ([\d.]+)(B|KB|MB|GB) sent", s)
        if m:
            r["wifi_rx_mb"], r["wifi_tx_mb"] = mb(m.group(1), m.group(2)), mb(m.group(3), m.group(4))
        m = re.search(r"Mobile network: ([\d.]+)(B|KB|MB|GB) received", s)
        if m:
            r["mobile_rx_mb"] = mb(m.group(1), m.group(2))
        m = re.search(r"Mobile radio active: ([^(]+)\(", s)
        if m:
            r["radio_active_s"] = dur_s(m.group(1))
        r["wakelocks"] = {n: dur_s(t) for n, t in re.findall(r"Wake lock (\S+): ([^p]+?) partial", s)}
        pm = re.search(r"Proc [^*][^\n]*:\n\s+CPU: ([^u]+)usr \+ ([^k]+)krn", s)
        if pm:
            r["process_cpu_s"] = round(dur_s(pm.group(1)) + dur_s(pm.group(2)), 1)
    return r


def threads(pkg):
    pid = ui.sh(f"pidof {pkg}").strip().split(" ")[0]
    if not pid:
        return []
    out = []
    for line in ui.sh(f"top -H -b -n 1 -p {pid} -o TID,TIME+,CMD").split("\n"):
        m = re.match(r"\s*(\d+)\s+([\d:.]+)\s+(.+)$", line)
        if m:
            out.append((m.group(3).strip(), m.group(2)))
    return sorted(out, key=lambda t: -sum(float(x) * 60 ** i for i, x in enumerate(reversed(t[1].split(":")))))[:8]


def pss_mb(pkg):
    m = re.search(r"TOTAL PSS:\s+(\d+)", ui.sh(f"dumpsys meminfo {pkg}"))
    return round(int(m.group(1)) / 1024, 1) if m else None


# Every setting that changes the playback path, per app (SETTINGS.md), and each app once more with the
# playlist downloaded first (Nori's with offload: the path that should cost least).
MATRIX = [
    ("nori", "plain", False), ("nori", "eq", False), ("nori", "automix", False), ("nori", "automix-ml", False),
    ("navic", "default", False), ("navic", "offload", False), ("navic", "eq", False),
    ("musly", "default", False), ("musly", "crossfade", False),
    ("symfonium", "default", False), ("symfonium", "crossfade", False), ("symfonium", "smartfades", False), ("symfonium", "eq", False),
    ("nori", "plain", True), ("navic", "default", True), ("musly", "default", True), ("symfonium", "default", True),
]


def really_plugged():
    """Whether a charger or cable powers the phone, as the hardware says (not a `dumpsys battery unplug`)."""
    ui.sh("dumpsys battery reset")
    return bool(re.search(r"(AC|USB|Wireless|Dock) powered: true", ui.sh("dumpsys battery")))


def battery_current_ma():
    """The battery's current draw now, mA, from `dumpsys battery`'s "current now" (Samsung prints it,
    negative while discharging). Read only while the phone is on battery, so its size is the draw."""
    m = re.search(r"current now: (-?\d+)", ui.sh("dumpsys battery"))
    if not m:
        return None
    v = int(m.group(1))
    # Some phones print µA; a phone does not draw amps while playing music with the screen off.
    if abs(v) > 20000:
        v //= 1000
    return abs(v)


def frames_start(pkg):
    """Starts counting frames: SurfaceFlinger's per-layer stats (any app, Flutter too) and hwui's."""
    ui.sh("dumpsys SurfaceFlinger --timestats -disable; dumpsys SurfaceFlinger --timestats -clear; "
          "dumpsys SurfaceFlinger --timestats -enable")
    ui.sh(f"dumpsys gfxinfo {pkg} reset")


def frames_end(pkg, seconds):
    """{fps: frames the app put on screen per second, janky_pct, refresh: the display's rate most of the
    time}. An app redrawing only what changes (a progress bar once a second) shows a few fps; one that
    animates constantly shows the display's refresh rate."""
    out = {}
    ts = ui.sh("dumpsys SurfaceFlinger --timestats -dump", timeout=60)
    ui.sh("dumpsys SurfaceFlinger --timestats -disable")
    layers = []
    for block in ts.split("layerName = ")[1:]:
        if pkg in block.split("\n", 1)[0]:
            m = re.search(r"totalFrames = (\d+)", block)
            layers.append(int(m.group(1)) if m else 0)
    # An app may draw into several layers (its window, a SurfaceView, a blur): the busiest one is how
    # often it redraws, the sum how much composing it causes.
    out["fps"] = round(max(layers) / seconds, 1) if layers and seconds else (0.0 if seconds else None)
    out["fps_all_layers"] = round(sum(layers) / seconds, 1) if seconds else None
    out["layers"] = len(layers)
    # The display's rate: per-config times where the phone lists them, else the global timeline's rate.
    rates = re.findall(r"([\d.]+)\s*fps\s*=\s*(\d+)\s*ms", ts)
    if rates:
        out["refresh_hz"] = round(float(max(rates, key=lambda x: int(x[1]))[0]))
    else:
        g = re.search(r"Global aggregated jank payload[\s\S]*?displayRefreshRate = ([\d.]+) fps", ts)
        if g:
            out["refresh_hz"] = round(float(g.group(1)))
    return out


def wlan_rx():
    # "wlan0:" at a line's start: not "swlan0:" (the hotspot), which also contains it.
    m = re.search(r"(?m)^\s*wlan0:\s*(\d+)", ui.sh("cat /proc/net/dev"))
    return int(m.group(1)) if m else 0


def wait_downloads(log, start_rx, quiet_kb=150, quiet_s=20, max_s=900):
    """Until the phone's Wi-Fi has been quiet (under quiet_kb over quiet_s) after the downloads began.
    `start_rx`: the Wi-Fi counter before they were asked for (on a LAN they may be over by now)."""
    t0, last, still = time.time(), wlan_rx(), 0
    got = last - start_rx
    while time.time() - t0 < max_s:
        time.sleep(5)
        now = wlan_rx()
        got += now - last
        if now - last < quiet_kb * 1000 * 5 / quiet_s:
            still += 5
            if still >= quiet_s and time.time() - t0 > 30:
                log(f"    downloads done: {got / 1e6:.0f} MB in {time.time() - t0:.0f} s")
                return got
        else:
            still = 0
        last = now
    log(f"    downloads still going after {max_s} s ({got / 1e6:.0f} MB): measuring anyway")
    return got


def track_state(app_uid):
    """The app's active track as AudioFlinger sees it: (sample rate, gain dB text, underrun frames), or
    None. An app can say it plays while its track starves (underruns growing) or is turned down to
    nothing (gain -inf): what is actually heard."""
    n = int(app_uid[3:]) + 10000
    for line in ui.sh("dumpsys media.audio_flinger").split("\n"):
        m = re.search(rf"\byes\s+\d+/\s*{n}\s+\d+\s+\d+\s+\S\s+\S+\s+\S+\s+\S+\s+(\d+)\s+\d+\s+\d+\s+\d+"
                      rf"\s+(\S+)\s+\S+\s+\S+\s+\S+\s+\S+\s+\S+\s+\S+\s+\d+\s+\d+\s+\S\s+(\d+)", line)
        if m:
            return int(m.group(1)), m.group(2), int(m.group(3))
    return None


def offloaded(app_uid):
    """Whether the app has an active track on an offload output (the audio chip decodes)."""
    n = int(app_uid[3:]) + 10000
    thread_off = False
    for line in ui.sh("dumpsys media.audio_flinger").split("\n"):
        if line.startswith("Output thread"):
            thread_off = "OFFLOAD" in line
        elif thread_off and re.search(rf"\byes\s+\d+/\s*{n}\b", line):
            return True
    return False


# ---- one run ----------------------------------------------------------------------------------------
def run_one(phone, app, variant, minutes, skips, outdir, log, cached=False, scenario="screen-off", brightness=1):
    rundir = os.path.join(outdir, f"{app.name}-{variant}{'-cached' if cached else ''}-{datetime.datetime.now():%H%M%S}")
    os.makedirs(rundir, exist_ok=True)
    app.shots = rundir
    rec = {"app": app.name, "variant": variant, "cached": cached, "scenario": scenario, "minutes": minutes, "start": datetime.datetime.now().isoformat(timespec="seconds")}
    phone.wake()
    ui.sh("logcat -c")
    app.clean()
    app.login()
    app.configure(variant)
    if cached:
        rx0 = wlan_rx()
        app.download_playlist()
        rec["downloaded_mb"] = round(wait_downloads(log, rx0) / 1e6)
    app.start_playlist()
    app.wait_playing(90)
    if app.variants[variant].get("no_skips"):
        skips = 0
    rec["skips"] = skips
    app.skip(skips)
    if scenario == "player":
        app.open_player()
    log(f"    {app.name}: playing after {skips} skips; {'full-screen player, screen on' if scenario == 'player' else 'screen off'}, measuring {minutes} min")

    uid = uid_of(app.pkg)
    # The real power state first: after `dumpsys battery unplug` the phone reports itself unplugged
    # whatever the cable does.
    rec["really_plugged_before"] = really_plugged()
    # Only a phone on a cable is told it is unplugged (so batterystats counts it as on battery):
    # `dumpsys battery unplug` also stops the battery's updates until `reset`, which would freeze the
    # percentage and the fuel gauge that a phone really on battery is measured by.
    if rec["really_plugged_before"]:
        ui.sh("dumpsys battery unplug")
    if scenario == "player":
        phone.screen_on_for(minutes, brightness)
        rec["brightness"] = brightness
        frames_start(app.pkg)
    else:
        phone.screen_off()
    ui.sh("dumpsys batterystats --reset")
    rec["phone_before"] = phone.info()
    stopped_at = None
    checks = []
    starved_s, muted, last = 0.0, 0, track_state(uid)
    currents = []
    t0 = time.time()
    while time.time() - t0 < minutes * 60:
        time.sleep(min(60, minutes * 60 - (time.time() - t0)))
        checks.append(offloaded(uid))
        if not rec["really_plugged_before"]:
            ma = battery_current_ma()
            if ma is not None:
                currents.append(ma)
        now = track_state(uid)
        if now:
            if last and now[2] >= last[2]:
                starved_s += (now[2] - last[2]) / max(now[0], 1)
            muted += now[1] == "-inf"
        last = now
        state, _ = app.session()
        if state != 3 and stopped_at is None:
            stopped_at = round((time.time() - t0) / 60, 1)
            log(f"    {app.name}: NOT PLAYING at minute {stopped_at} (session state {state})")

    stats = ui.sh(f"dumpsys batterystats --charged {app.pkg}", timeout=180)
    open(os.path.join(rundir, "batterystats.txt"), "w").write(stats)
    rec.update(parse(stats, uid))
    if scenario == "player":
        rec.update(frames_end(app.pkg, rec.get("seconds") or minutes * 60))
    # The app's own log over the run, kept beside its figures (the phone's log is overwritten within hours).
    open(os.path.join(rundir, "logcat.txt"), "w").write(ui.redact(ui.sh(f"logcat -d --uid={int(uid[3:]) + 10000}", timeout=120)))
    rec["threads"] = threads(app.pkg)
    rec["pss_mb"] = pss_mb(app.pkg)
    rec["phone_after"] = phone.info()
    rec["stopped_at_min"] = stopped_at
    rec["offloaded_checks"] = f"{sum(checks)}/{len(checks)}"
    rec["starved_s"] = round(starved_s, 1)
    rec["muted_checks"] = f"{muted}/{len(checks)}"
    # The real drain from the fuel gauge, when the phone ran on its battery all along (the count moves in
    # steps of about 4 mAh: only a long run says much).
    ui.sh("dumpsys battery reset")
    rec["really_plugged_after"] = really_plugged()
    c0, c1 = rec["phone_before"].get("charge_uah"), rec["phone_after"].get("charge_uah")
    plugged = rec["really_plugged_before"] or rec["really_plugged_after"]
    rec["on_cable"] = plugged
    rec["gauge_mah"] = round((c0 - c1) / 1000, 1) if c0 and c1 and not plugged else None
    # The whole phone's real current, read once a minute on battery (the fuel gauge's average): mA = mAh/h.
    rec["current_ma"] = round(sum(currents) / len(currents)) if currents and not plugged else None
    rec["current_samples"] = len(currents)
    if app.name == "nori":
        lines = [l for l in ui.sh("logcat -d -s nori:I").split("\n") if "measuring" in l and "took" in l]
        rec["songs_measured"] = len(lines)
        open(os.path.join(rundir, "measuring.txt"), "w").write("\n".join(lines))
    ui.sh("dumpsys battery reset")
    app.stop()
    return rec


def per_hour(rec, key):
    v, s = rec.get(key), rec.get("seconds")
    return round(v * 3600 / s, 1) if v is not None and s else None


def table(recs):
    rows = ["| playlist | app | variant | measured (min) | **score** | battery / memory / playback | % battery/h (app) | hours on a full battery (whole phone) | phone mA (measured) | net MB/h | screen model | fps (refresh) | **app + decoder + audioserver** | app | cpu | wakelock | wifi | radio | mediacodec | audioserver | Wi-Fi MB | process CPU s | PSS MB | songs measured | offloaded | asleep s | gauge mAh/h | note |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in recs:
        if "error" in r:
            rows.append(f"| {r.get('playlist', '')} | {r['app']} | {r['variant']}{' cached' if r.get('cached') else ''}{' [player]' if r.get('scenario') == 'player' else ''} | | | | | | | | | | | | | | | | | | | | | | | | | FAILED: {r['error'][:80]} |")
            continue
        f = lambda k: "" if per_hour(r, k) is None else f"{per_hour(r, k)}"
        notes = []
        if r.get("stopped_at_min") is not None:
            notes.append(f"stopped at min {r['stopped_at_min']}")
        if r.get("starved_s", 0) > 5:
            notes.append(f"starved {r['starved_s']:.0f} s")
        if r.get("muted_checks", "0/").split("/")[0] not in ("0", ""):
            notes.append(f"muted {r['muted_checks']}")
        note = ", ".join(notes)
        sc, parts = score(r)
        rows.append(f"| {r.get('playlist', '')} | {r['app']} | {r['variant']}{' cached' if r.get('cached') else ''}{' [player]' if r.get('scenario') == 'player' else ''} | {r['seconds'] / 60:.1f} | "
                    f"**{sc:.0f}** | {parts['battery']} / {parts['memory']} / {parts['playback']} | "
                    + " | ".join("" if x is None else str(x) for x in battery_use(r)) + f" | {r.get('current_ma') or ''} | {net_mb_h(r)} | {f('screen_mah')} | "
                    + (f"{r['fps']} ({r.get('refresh_hz', '?')} Hz)" if r.get("fps") is not None else "") + " |"
                    + f" **{f('with_system_mah')}** | {f('app_mah')} | {f('cpu_mah')} | {f('wakelock_mah')} | "
                    f"{f('wifi_mah')} | {f('mobile_radio_mah')} | {f('mediacodec_mah')} | {f('audioserver_mah')} | {r.get('wifi_rx_mb', 0):.0f} | {r.get('process_cpu_s', '')} | {r.get('pss_mb', '')} | "
                    f"{r.get('songs_measured', '')} | {r.get('offloaded_checks', '')} | {r.get('asleep_s', '')} | {f('gauge_mah')} | {note} |")
    return ("mAh per hour of playback (batterystats estimates; CPU times are measured). The phone's fixed "
            "audio-hardware model (about the same in every run, charged to an app only on the offload path) "
            "is left out of every column.\n\n" + "\n".join(rows) + "\n")


# The combined score: one number per run for a quick comparison, from parts that can be checked. Fixed
# references rather than "relative to the best of this session", so scores from different sessions compare.
WEIGHTS = {"battery": 0.70, "memory": 0.15, "playback": 0.15}
FLOOR_MAH_H = 25.0  # the "phone awake" wakelock every CPU player pays on the S21 FE: scores 100
REF_PSS_MB = 100.0  # at or under this much memory scores 100


def score(r, weights=None):
    """(total, {part: 0–100}) for a run, or None for a failed one.
    battery  = 100 × FLOOR / (app + mediacodec + audioserver, mAh/h), at most 100
    memory   = 100 × REF_PSS / PSS, at most 100
    playback = 100 × (1 − starved seconds / measured seconds − muted checks / checks), at least 0"""
    if "error" in r or not r.get("seconds"):
        return None
    w = weights or WEIGHTS
    parts = {}
    total = per_hour(r, "with_system_mah")
    parts["battery"] = min(100.0, 100 * FLOOR_MAH_H / total) if total else 0.0
    parts["memory"] = min(100.0, 100 * REF_PSS_MB / r["pss_mb"]) if r.get("pss_mb") else 100.0
    muted, checks = (int(x) for x in (r.get("muted_checks") or "0/0").split("/"))
    silent = r.get("starved_s", 0) / r["seconds"] + (muted / checks if checks else 0)
    parts["playback"] = max(0.0, 100 * (1 - silent))
    return round(sum(w[k] * parts[k] for k in w) / sum(w.values()), 1), {k: round(v) for k, v in parts.items()}


def battery_use(r):
    """(% of the battery per hour the app costs, hours a full battery lasts playing like this, or None).
    The first is the app's own share (batterystats); the second is the whole phone, from the fuel gauge,
    only for a run of 20+ minutes unplugged (the gauge moves in steps of about 4 mAh)."""
    cap, total = r.get("capacity_mah"), per_hour(r, "with_system_mah")
    pct = round(100 * total / cap, 2) if cap and total else None
    # The whole phone: the current sampled each minute on battery, else the fuel gauge over a long run.
    whole = r.get("current_ma") if r.get("current_samples", 0) >= 3 else None
    if whole is None and r.get("seconds", 0) >= 20 * 60:
        whole = per_hour(r, "gauge_mah")
    hours = round(cap / whole, 1) if cap and whole else None
    return pct, hours


def battery_levels(r):
    """"98→97 %" from the phone's own percentage before and after the measuring, "(cable)" when it ran on one."""
    a, z = (r.get("phone_before") or {}).get("battery_level"), (r.get("phone_after") or {}).get("battery_level")
    if a is None or z is None:
        return ""
    return f"{a}→{z} %" + (" (cable)" if r.get("on_cable") else "")


def net_mb_h(r):
    """What the app downloaded while measured, MB per hour (Wi-Fi and mobile data)."""
    mb_total = (r.get("wifi_rx_mb") or 0) + (r.get("mobile_rx_mb") or 0)
    return f"{mb_total * 3600 / r['seconds']:.0f}" if r.get("seconds") else ""


def terminal_table(recs):
    """The results for a terminal: the columns that tell most, lined up, sorted per playlist by the
    total (cheapest first). The full table is results.md."""
    cols = [("playlist", 10), ("app", 9), ("variant", 16), ("measured", 8), ("SCORE", 5), ("%/h", 5), ("hours", 5), ("mA", 4), ("net MB/h", 8), ("total", 6), ("app", 6), ("cpu", 6),
            ("wake", 6), ("codec", 6), ("cpu s", 6), ("PSS", 5), ("offl", 4), ("note", 28)]
    head = "  ".join(f"{n:>{w}}" if i >= 3 and i < 17 else f"{n:<{w}}" for i, (n, w) in enumerate(cols))
    wt = ", ".join(f"{k} {v:.0%}" for k, v in WEIGHTS.items())
    lines = ["mAh/h (batterystats): total = app + mediacodec + audioserver, without the audio-hardware model",
             f"SCORE 0–100, higher is better: {wt} (battery: {FLOOR_MAH_H:.0f} mAh/h = 100, memory: {REF_PSS_MB:.0f} MB = 100,"
             " playback: less starved or muted time = more)",
             "%/h: the app's own share of the battery per hour; hours: a full battery playing like this, whole phone,"
             " from its real current measured each minute (runs on battery only); mA: that current;"
             " net MB/h: the app's downloads (Wi-Fi + mobile) per hour", "",
             head, "-" * len(head)]
    ph = lambda r, k: "" if per_hour(r, k) is None else f"{per_hour(r, k):.1f}"
    order = sorted(recs, key=lambda r: (r.get("playlist", ""), "error" in r, -(score(r) or (0,))[0]))
    last = None
    for r in order:
        if last is not None and r.get("playlist") != last:
            lines.append("")
        last = r.get("playlist")
        name = r["variant"] + (" cached" if r.get("cached") else "") + (" [player]" if r.get("scenario") == "player" else "")
        if "error" in r:
            vals = [r.get("playlist", ""), r["app"], name] + [""] * 14 + ["FAILED: " + r["error"][:60]]
        else:
            notes = []
            if r.get("stopped_at_min") is not None:
                notes.append(f"stopped min {r['stopped_at_min']}")
            if r.get("starved_s", 0) > 5:
                notes.append(f"starved {r['starved_s']:.0f}s")
            if r.get("muted_checks", "0/").split("/")[0] not in ("0", ""):
                notes.append(f"muted {r['muted_checks']}")
            if r.get("fps") is not None:
                notes.insert(0, f"{r['fps']} fps@{r.get('refresh_hz', '?')}Hz" + (f" jank {r['janky_pct']:.0f}%" if r.get("janky_pct") else ""))
            vals = [r.get("playlist", ""), r["app"], name, f"{r['seconds'] / 60:.0f} min", f"{score(r)[0]:.0f}",
                    *(("" if x is None else f"{x}") for x in battery_use(r)), r.get("current_ma") or "", net_mb_h(r), ph(r, "with_system_mah"),
                    ph(r, "app_mah"), ph(r, "cpu_mah"), ph(r, "wakelock_mah"), ph(r, "mediacodec_mah"),
                    f"{r.get('process_cpu_s', '')}", f"{r.get('pss_mb') or '':.0f}" if r.get("pss_mb") else "",
                    r.get("offloaded_checks", ""), ", ".join(notes)]
        lines.append("  ".join((f"{str(v)[:w]:>{w}}" if 3 <= i < 17 else f"{str(v)[:w]:<{w}}") for i, (v, (_, w)) in enumerate(zip(vals, cols))))
    return "\n".join(lines) + "\n"


def main():
    apps = all_apps()
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--serial", default=os.environ.get("ANDROID_SERIAL"), help="the phone's adb serial (default $ANDROID_SERIAL)")
    p.add_argument("--apps", default="nori,musly,navic,symfonium")
    p.add_argument("--variants", default="plain", help="Nori's variants, comma separated (the others have one)")
    p.add_argument("--minutes", type=float, default=15)
    p.add_argument("--repeat", type=int, default=1)
    p.add_argument("--skips", type=int, default=3)
    p.add_argument("--server", choices=["local", "real"], default="local",
                   help="local: tools/bgtest/server.py's Navidrome (default); real: the server in ~/.music.pass")
    p.add_argument("--playlist", default=None, help="one or more, comma separated: the whole plan runs once per playlist "
                   "(default bg-mp3 on the local server, RockMix on the real one)")
    p.add_argument("--order", choices=["play", "shuffle"], default="play",
                   help="play: the list in order, the same songs in every app (default); shuffle: each app's own shuffle")
    p.add_argument("--power-save", choices=["on", "off"], default="on")
    p.add_argument("--matrix", action="store_true", help="every app in every setting that changes the playback path, plus a cached run each (MATRIX)")
    p.add_argument("--cached", action="store_true", help="also run each chosen app/variant with the playlist downloaded first")
    p.add_argument("--runs", default=None, help="explicit runs, e.g. nori:eq,navic:offload,musly:default:cached")
    p.add_argument("--weights", default=None, help="score weights, e.g. battery=0.7,memory=0.15,playback=0.15")
    p.add_argument("--scenario", default="screen-off",
                   help="screen-off (music in the background), player (the full-screen player on screen, fixed "
                        "brightness), or both, comma separated: every run once per scenario")
    p.add_argument("--brightness", type=int, default=1, help="screen brightness 1–255 in the player scenario (default 1, the dimmest)")
    p.add_argument("--nori-pkg", default="dev.nori.music.perf",
                   help="which Nori build to drive: dev.nori.music.perf (the perf build) or dev.nori.music (a normal build)")
    p.add_argument("--list", action="store_true")
    a = p.parse_args()
    if a.list:
        for k, c in apps.items():
            print(f"{k:10} {c.pkg:28} variants: {', '.join(c.variants)}")
        return 0

    if a.weights:
        WEIGHTS.update({k: float(v) for k, v in (x.split("=") for x in a.weights.split(","))})
    # Both Nori builds are stopped before every run (only the one under test may play); the chosen one is driven.
    apps["nori"].pkg = a.nori_pkg
    ui.SERIAL = a.serial
    url, user, password = creds(a.server)
    playlists = (a.playlist or ("bg-mp3" if a.server == "local" else "RockMix")).split(",")
    ui.SECRETS = [password]
    chosen = [apps[x.strip()] for x in a.apps.split(",")]
    outdir = os.path.join(ROOT, "build", "bgtest", "results", f"{datetime.datetime.now():%Y%m%d-%H%M%S}")
    os.makedirs(outdir, exist_ok=True)
    logf = open(os.path.join(outdir, "log.txt"), "a")

    def log(msg):
        line = f"{datetime.datetime.now():%H:%M:%S} {ui.redact(msg)}"
        print(line, flush=True)
        logf.write(line + "\n")
        logf.flush()

    phone = Phone(sorted({c.pkg for c in apps.values()} | {"dev.nori.music", "dev.nori.music.perf"}))
    phone.save()
    log(f"phone: {phone.info()}")
    vn = re.search(r"versionName=(\S+)", ui.sh(f"dumpsys package {a.nori_pkg}"))
    log(f"nori: {a.nori_pkg} {vn.group(1) if vn else '(not installed)'}")
    log(f"server {a.server} ({url}), playlists {', '.join(playlists)}, {a.order}, {a.minutes} min, {a.skips} skips, power save {a.power_save}")
    for c in chosen:
        installed = c.pkg in ui.sh(f"pm list packages {c.pkg}")
        if not installed:
            apk = os.path.join(APKS, f"{c.pkg}.apk")
            log(f"installing {c.pkg} from {apk}")
            ui.adb("install", "-r", "-g", apk, timeout=300)
    phone.prepare(a.power_save == "on")
    for c in apps.values():
        ui.sh(f"am force-stop {c.pkg}")

    recs = []
    if a.runs:
        runs = [(x.split(":")[0], x.split(":")[1], x.endswith(":cached")) for x in a.runs.split(",")]
    elif a.matrix:
        names = [c.name for c in chosen]
        runs = [r for r in MATRIX if r[0] in names]
    else:
        runs = [(c.name, v, False) for c in chosen for v in (a.variants.split(",") if c.name == "nori" else ["default"])]
        if a.cached:
            runs += [(n, v, True) for n, v, _ in runs]
    for n, v, _ in runs:
        if v not in apps[n].variants:
            sys.exit(f"{n} has no variant {v!r}: {', '.join(apps[n].variants)}")
    scenarios = a.scenario.split(",")
    plan = [(apps[n], v, cached, pl, sc) for sc in scenarios for pl in playlists for _ in range(a.repeat) for n, v, cached in runs]
    # A run is its minutes of measuring plus the set-up around it (wipe, log in, settings, skips), about
    # 2 min, 3 when the playlist is downloaded first; once runs have finished, their real time is used.
    guess = lambda k: a.minutes + (3 if k else 2)
    total = sum(guess(k) for _, _, k, _, _ in plan)
    t_start = time.time()
    fmt = lambda m: f"{int(m // 60)} h {int(m % 60):02d} min" if m >= 60 else f"{m:.0f} min"
    at = lambda m: (datetime.datetime.now() + datetime.timedelta(minutes=m)).strftime("%H:%M")
    log(f"plan: {len(plan)} runs, about {fmt(total)}, done around {at(total)}: "
        + ", ".join(f"{c.name}:{v}{':cached' if k else ''}@{pl}/{sc}" for c, v, k, pl, sc in plan))
    for i, (c, variant, cached, playlist, scenario) in enumerate(plan, 1):
        done_min = (time.time() - t_start) / 60
        if i > 1:
            # Scale the guess for what is left by how the finished runs compared to theirs.
            ratio = done_min / sum(guess(k) for _, _, k, _, _ in plan[:i - 1])
            left = ratio * sum(guess(k) for _, _, k, _, _ in plan[i - 1:])
        else:
            left = total
        log(f"— {i}/{len(plan)} · {fmt(done_min)} done · about {fmt(left)} left · done around {at(left)} —")
        app = c((url, user, password), playlist, log, outdir)
        app.start_button = "Play" if a.order == "play" else "Shuffle"
        log(f"run {i}/{len(plan)}: {app.name} ({variant}{', cached' if cached else ''}) on {playlist}, {scenario}")
        try:
            for other in phone.packages:
                ui.sh(f"am force-stop {other}")
            rec = run_one(phone, app, variant, a.minutes, a.skips, outdir, log, cached, scenario, a.brightness)
            rec["playlist"] = playlist
            log(f"    {app.name}: {per_hour(rec, 'with_system_mah')} mAh/h with decoder and audioserver, app {per_hour(rec, 'app_mah')}, cpu {per_hour(rec, 'cpu_mah')}, "
                f"wakelock {per_hour(rec, 'wakelock_mah')}, wifi {per_hour(rec, 'wifi_mah')}")
        except (StepFailed, TimeoutError, RuntimeError) as e:
            log(f"    FAILED: {e}")
            rec = {"app": app.name, "variant": variant, "cached": cached, "playlist": playlist, "scenario": scenario, "error": ui.redact(str(e))}
            ui.sh("dumpsys battery reset")
            try:
                app.stop()
            except Exception:
                pass
        recs.append(rec)
        with open(os.path.join(outdir, "runs.jsonl"), "a") as f:
            f.write(json.dumps(rec) + "\n")
        with open(os.path.join(outdir, "results.md"), "w") as f:
            f.write(table(recs))
    print("\n" + terminal_table(recs))
    log(f"finished {len(plan)} runs in {fmt((time.time() - t_start) / 60)}")
    log(f"results in {outdir}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception:
        traceback.print_exc()
        sys.exit(1)
