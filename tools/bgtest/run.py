#!/usr/bin/env python3
"""The one command for the battery tests: asks for everything, then runs bgtest.py.

    tools/bgtest/run.py

1. Picks the phone: every phone adb sees (by cable or Wi-Fi), or one paired now through Developer
   options > Wireless debugging (no cable at all: the phone is found on the network, only the pairing
   code is typed).
2. A phone on a cable is moved to adb over Wi-Fi, and you are asked to unplug it: the run waits until
   the phone itself says it is on battery, so nothing is charged while measuring.
3. Starts the local test server (server.py) when it is not answering.
4. Asks what to run, then hands over to bgtest.py, which puts the phone back as it was when it ends.

Every question has a flag, so an agent (or a script) runs it without a terminal; with no terminal it
never waits for typing, taking the flag or the default and saying so:

    tools/bgtest/run.py --serial <usb serial> --wifi --plan quick
    tools/bgtest/run.py --serial <phone ip>:5555 --plan matrix --server local --playlist bg-mp3
    tools/bgtest/run.py --serial ... --runs nori:eq,navic:offload,musly:default:cached --minutes 5

    --wifi            a phone on a cable is moved to adb over Wi-Fi, then the run waits (up to
                      --unplug-timeout s) for the cable to be pulled; it prints ACTION: lines for the
                      person at the phone
    --stay-plugged    measure on the cable (batterystats still counts it as on battery)
    --list-devices    print the phones adb sees and exit

The last line is "RESULTS: <folder>" (results.md, runs.jsonl, each run's dumps).
"""
import os
import re
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))


def adb(*a, serial=None, timeout=30):
    cmd = ["adb"] + (["-s", serial] if serial else []) + list(a)
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    return (r.stdout + r.stderr).replace("\r", "")


def devices():
    """[(serial, model, how)] of phones adb can use now."""
    out = []
    for line in adb("devices", "-l").split("\n")[1:]:
        m = re.match(r"(\S+)\s+device\b(.*)", line)
        if not m or m.group(1).startswith("emulator-"):
            continue
        model = re.search(r"model:(\S+)", m.group(2))
        wifi = ":" in m.group(1) or "_adb-tls-connect" in m.group(1)
        out.append((m.group(1), model.group(1) if model else "?", "Wi-Fi" if wifi else "cable"))
    return out


INTERACTIVE = sys.stdin.isatty()


def ask(prompt, default=None):
    if not INTERACTIVE:
        print(f"{prompt}: {default} (no terminal: default)")
        return default if default is not None else ""
    s = input(f"{prompt}{f' [{default}]' if default is not None else ''}: ").strip()
    return s or (default if default is not None else "")


def choose(title, options, default=1):
    print(f"\n{title}")
    for i, o in enumerate(options, 1):
        print(f"  {i}) {o}")
    if not INTERACTIVE:
        print(f"  → {options[default - 1]} (no terminal: default)")
        return default - 1
    while True:
        s = ask("choose", str(default))
        if s.isdigit() and 1 <= int(s) <= len(options):
            return int(s) - 1


def on_battery(serial):
    b = adb("shell", "dumpsys battery", serial=serial)
    powered = [l for l in b.split("\n") if re.search(r"(AC|USB|Wireless|Dock) powered: true", l)]
    level = re.search(r"level: (\d+)", b)
    return not powered, int(level.group(1)) if level else None


def phone_ip(serial):
    m = re.search(r"inet (\d+\.\d+\.\d+\.\d+)", adb("shell", "ip -4 addr show wlan0", serial=serial))
    return m.group(1) if m else None


def pair():
    """Wireless debugging, no cable: the phone's pairing and connect services are found over mDNS."""
    print("\nOn the phone: Settings > Developer options > Wireless debugging > on,")
    print("then 'Pair device with pairing code'. Keep that screen open.")
    for _ in range(20):
        svc = adb("mdns", "services")
        p = re.search(r"_adb-tls-pairing\._tcp\.?\s+(\d+\.\d+\.\d+\.\d+:\d+)", svc)
        if p:
            break
        time.sleep(1)
    else:
        addr = ask("the phone was not found on the network; type the IP:port shown on the pairing screen")
        p = re.match(r"(.*)", addr)
    target = p.group(1)
    code = ask(f"pairing code shown on the phone ({target})")
    print(adb("pair", target, code).strip())
    for _ in range(20):
        c = re.search(r"_adb-tls-connect\._tcp\.?\s+(\d+\.\d+\.\d+\.\d+:\d+)", adb("mdns", "services"))
        if c:
            print(adb("connect", c.group(1)).strip())
            return c.group(1)
        time.sleep(1)
    addr = ask("type the IP:port shown at the top of the Wireless debugging screen")
    print(adb("connect", addr).strip())
    return addr


def to_wifi(serial):
    """Cable → adb over Wi-Fi (port 5555, until the phone restarts)."""
    ip = phone_ip(serial)
    if not ip:
        sys.exit("the phone has no Wi-Fi address: connect it to Wi-Fi first")
    adb("tcpip", "5555", serial=serial)
    time.sleep(3)
    target = f"{ip}:5555"
    for _ in range(10):
        if "connected" in adb("connect", target):
            break
        time.sleep(1)
    if target not in [d[0] for d in devices()]:
        sys.exit(f"could not reach {target} over Wi-Fi")
    print(f"  the phone answers over Wi-Fi at {target}")
    return target


def wait_unplugged(serial, timeout=300):
    print("\nACTION: unplug the USB cable from the phone now", flush=True)
    t0 = time.time()
    while time.time() - t0 < timeout:
        free, level = on_battery(serial)
        if free:
            print(f"  on battery ({level} %)", flush=True)
            return level
        time.sleep(2)
    sys.exit(f"the phone was still plugged in after {timeout} s: unplug it, or run with --stay-plugged")


def whole_phone_drain(default=200.0):
    """mAh/h the whole phone used in earlier runs measured unplugged with the fuel gauge (the median),
    or `default` before there are any."""
    import glob, json
    rates = []
    for f in glob.glob(os.path.join(ROOT, "build", "bgtest", "results", "*", "runs.jsonl")):
        for line in open(f):
            try:
                r = json.loads(line)
            except ValueError:
                continue
            if r.get("current_ma") and r.get("current_samples", 0) >= 3:
                rates.append(r["current_ma"])
            elif r.get("gauge_mah") and r.get("seconds", 0) >= 20 * 60:
                rates.append(r["gauge_mah"] * 3600 / r["seconds"])
    return sorted(rates)[len(rates) // 2] if rates else default


def server_up():
    f = os.path.join(ROOT, "build", "bgtest", "server", "url")
    if not os.path.exists(f):
        return False
    try:
        urllib.request.urlopen(open(f).read().strip() + "/ping", timeout=3)
        return True
    except Exception:
        return False


PLANS = [
    ("quick: every app, default settings, 3 min each", ["--minutes", "3"]),
    ("matrix: every app and every setting that changes the playback path, 5 min each (about 2 h)", ["--matrix", "--minutes", "5"]),
    ("custom", None),
]


def main():
    import argparse
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--serial", help="the phone (adb serial); asked, or the only one, when left out")
    p.add_argument("--wifi", action="store_true", help="move a cabled phone to adb over Wi-Fi and wait for the cable to be pulled")
    p.add_argument("--stay-plugged", action="store_true", help="measure on the cable / charger")
    p.add_argument("--unplug-timeout", type=int, default=300)
    p.add_argument("--server", choices=["local", "real"])
    p.add_argument("--plan", choices=["quick", "matrix"])
    p.add_argument("--runs", help="explicit runs, e.g. nori:eq,navic:offload,musly:default:cached")
    p.add_argument("--minutes", type=float)
    p.add_argument("--playlist", help="one or more, comma separated: every run once per playlist, e.g. bg-mp3,bg-quick")
    p.add_argument("--nori-pkg", help="dev.nori.music.perf (perf build, default) or dev.nori.music (a normal build)")
    p.add_argument("--list-devices", action="store_true")
    p.add_argument("--repeat", type=int)
    p.add_argument("--skips", type=int)
    p.add_argument("--power-save", choices=["on", "off"])
    p.add_argument("--scenario", help="screen-off, player, or both comma separated")
    p.add_argument("--brightness", type=int, help="brightness 1–255 in the player scenario (default 1)")
    p.add_argument("--volume", help="media volume step for every run (default 1), or 'keep'")
    a = p.parse_args()

    devs = devices()
    if INTERACTIVE and len(sys.argv) == 1:
        # No options in a terminal: everything is picked on one screen (tui.py).
        sys.path.insert(0, HERE)
        import tui
        from apps import all_apps
        from bgtest import MATRIX
        apps = all_apps()
        quick = [(n, list(c.variants)[0], False) for n, c in apps.items()]
        playlists = {"local": ["bg-mp3", "bg-quick", "bg-flac-44", "bg-flac-48", "bg-flac-96", "bg-48k", "bg-mixed"], "real": ["RockMix"]}
        batteries = {}
        for s, _, _ in devs:
            b = adb("shell", "dumpsys battery", serial=s)
            lv = re.search(r"level: (\d+)", b)
            cc = re.search(r"Charge counter: (\d+)", b)
            plugged = bool(re.search(r"(AC|USB|Wireless) powered: true", b))
            if lv:
                level = int(lv.group(1))
                cap = int(cc.group(1)) / 1000 * 100 / level if cc and level else None
                batteries[s] = (level, cap, plugged)
        got = tui.pick(devs, playlists, {n: list(c.variants) for n, c in apps.items()}, MATRIX, quick,
                       batteries, whole_phone_drain())
        if not got:
            return 130
        a.serial = pair() if got["serial"] == "pair" else got["serial"]
        a.wifi, a.stay_plugged = got["wifi"], not got["wifi"]
        a.server, a.playlist = got["server"], ",".join(got["playlists"])
        a.runs = ",".join(f"{n}:{v}{':cached' if c else ''}" for n, v, c in got["runs"])
        a.minutes, a.repeat, a.skips = got["minutes"], got["repeat"], got["skips"]
        a.power_save = "on" if got["power_save"] else "off"
        a.scenario = ",".join(got["scenarios"])
        a.brightness = got["brightness"]
        a.nori_pkg = got["nori_pkg"]
        a.volume = str(got["volume"])
        devs = devices()
    if a.list_devices:
        for s, m, how in devs:
            print(f"{s}\t{m}\t{how}")
        return 0

    # ---- the phone ----
    if a.serial:
        serial = a.serial
        how = next((h for s, _, h in devs if s == serial), None)
        if how is None:
            if ":" in serial:
                adb("connect", serial)
                how = "Wi-Fi"
            else:
                sys.exit(f"adb does not see {serial}: {[d[0] for d in devs]}")
    elif len(devs) == 1 and not INTERACTIVE:
        serial, _, how = devs[0]
    else:
        if not INTERACTIVE and not devs:
            sys.exit("no phone: connect one, or pair it (run this in a terminal to pair)")
        options = [f"{s}  ({m}, {h})" for s, m, h in devs] + ["pair a phone through Wireless debugging (no cable)"]
        if not INTERACTIVE:
            sys.exit("several phones: pick one with --serial (see --list-devices)")
        i = choose("Which phone?", options)
        serial, how = (pair(), "Wi-Fi") if i == len(devs) else (devs[i][0], devs[i][2])
    model = adb("shell", "getprop ro.product.model", serial=serial).strip()
    print(f"phone: {model} ({serial}, {how})")

    free, level = on_battery(serial)
    unplugged_here = False
    if how == "cable" and not a.stay_plugged:
        go = a.wifi or choose("It is on a cable, which charges it. Measure on battery over Wi-Fi?",
                              ["yes: switch adb to Wi-Fi, then I unplug the cable (recommended)",
                               "no: stay on the cable (batterystats still counts it as on battery)"],
                              default=1 if (INTERACTIVE or a.wifi) else 2) == 0
        if go:
            serial = to_wifi(serial)
            level = wait_unplugged(serial, a.unplug_timeout)
            unplugged_here = True
    elif not free and not a.stay_plugged:
        if a.wifi or choose("The phone is plugged into a charger. Unplug it for the run?",
                            ["yes, I unplug it now (recommended)", "no, measure plugged in"],
                            default=1 if INTERACTIVE else 2) == 0:
            level = wait_unplugged(serial, a.unplug_timeout)
            unplugged_here = True
    if level is not None and level < 30:
        print(f"warning: the battery is at {level} %; a long run may not finish")

    # ---- the server ----
    server = a.server or ["local", "real"][choose("Which music server?", ["local test server (tools/bgtest/server.py)", "the real one in ~/.music.pass"])]
    if server == "local" and not server_up():
        print("starting the local test server…", flush=True)
        subprocess.run([sys.executable, os.path.join(HERE, "server.py")], check=True)

    # ---- what to run ----
    if a.runs:
        args = ["--runs", a.runs, "--minutes", str(a.minutes or 5)]
    else:
        plan = a.plan or ["quick", "matrix", "custom"][choose("What to run?", [t for t, _ in PLANS])]
        if plan == "custom":
            runs = ask("runs (app:variant[:cached], comma separated)", "nori:eq,musly:default,navic:default,symfonium:default")
            args = ["--runs", runs, "--minutes", ask("minutes per run", "5")]
        else:
            args = list(dict(zip(["quick", "matrix"], [PLANS[0][1], PLANS[1][1]]))[plan])
            if a.minutes:
                args[args.index("--minutes") + 1] = str(a.minutes)
    # Everything asked for goes on to bgtest.py, whichever way the runs were chosen.
    for flag, v in (("--repeat", a.repeat), ("--skips", a.skips), ("--power-save", a.power_save), ("--scenario", a.scenario),
                    ("--brightness", a.brightness), ("--volume", a.volume)):
        if v is not None and flag not in args:
            args += [flag, str(v)]
    playlist = a.playlist or ask("playlists (comma separated: every run once per playlist)", "bg-mp3" if server == "local" else "RockMix")

    cmd = [sys.executable, "-u", os.path.join(HERE, "bgtest.py"), "--serial", serial, "--server", server, "--playlist", playlist] + args
    if a.nori_pkg:
        cmd += ["--nori-pkg", a.nori_pkg]
    print("\nrunning: " + " ".join(cmd[2:]) + "\n", flush=True)
    results = None
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    for line in proc.stdout:
        print(line, end="", flush=True)
        m = re.search(r"results in (\S+)", line)
        if m:
            results = m.group(1)
    rc = proc.wait()
    if unplugged_here:
        print("\nACTION: the phone can be plugged back in")
    print(f"RESULTS: {results or '(none)'}")
    return rc


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\nstopped")
        sys.exit(130)
