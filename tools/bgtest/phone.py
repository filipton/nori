"""The phone's state around a test session: what the harness changes, remembered first and put back
whatever way the session ends (finished, failed, Ctrl+C, killed with SIGTERM/SIGHUP).

Changed for the session and restored after:
  - the on-screen keyboard is left alone: ui.text() closes it after typing (a Samsung falls back to
    another keyboard, then to voice typing, whichever one is disabled),
  - "stay awake while charging" and the screen-off timeout (the screen stays on while setting apps up),
  - battery power-save mode (set to what the run asks for),
  - the battery's plugged state as batterystats sees it (`dumpsys battery unplug` during a measurement,
    so a phone on USB is counted as on battery),
  - the apps' own language (English, so the labels the steps look for are always the same).
"""
import atexit
import json
import os
import signal
import sys
import time

import ui

STATE_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".phone-state.json")


class Phone:
    def __init__(self, packages):
        self.packages = packages
        self.saved = None
        self.restored = False

    # ---- remember and put back ----------------------------------------------------------------------
    def save(self):
        if os.path.exists(STATE_FILE):
            # A session before this one died without putting things back (e.g. the computer lost power):
            # its record of the phone as it was is the true one, not the half-changed phone now.
            with open(STATE_FILE) as f:
                self.saved = json.load(f)
            print("  a previous run left the phone changed: restoring from its record first")
            self.restore()
            self.restored = False
        s = {
            "imes": [l for l in ui.sh("ime list -s").split("\n") if l.strip()],
            # Every installed keyboard, not only the enabled one: with one disabled, Android falls back
            # to the next (a Samsung has SwiftKey besides its own).
            "all_imes": [l for l in ui.sh("ime list -a -s").split("\n") if l.strip()],
            "disabled_pkgs": [l.split(":", 1)[1] for l in ui.sh("pm list packages -d").split("\n") if ":" in l],
            "ime": ui.sh("settings get secure default_input_method").strip(),
            "stay_on": ui.sh("settings get global stay_on_while_plugged_in").strip(),
            "timeout": ui.sh("settings get system screen_off_timeout").strip(),
            "low_power": ui.sh("settings get global low_power").strip(),
            "brightness": ui.sh("settings get system screen_brightness").strip(),
            "brightness_mode": ui.sh("settings get system screen_brightness_mode").strip(),
            "locales": {p: ui.sh(f"cmd locale get-app-locales {p}").strip() for p in self.packages},
        }
        self.saved = s
        with open(STATE_FILE, "w") as f:
            json.dump(s, f, indent=1)
        atexit.register(self.restore)
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            signal.signal(sig, self._on_signal)

    def _on_signal(self, signum, frame):
        print(f"\n  stopped by {signal.Signals(signum).name}: putting the phone back")
        self.restore()
        sys.exit(130)

    def restore(self):
        if self.restored or not self.saved:
            return
        self.restored = True
        s = self.saved
        steps = [
            "dumpsys battery reset",
            *[f"am force-stop {p}" for p in self.packages],
            # Only the keyboards that were on stay on (an earlier version switched others on).
            *[f"ime disable {i}" for i in s.get("all_imes", []) if i not in s["imes"]],
            *[f"ime enable {i}" for i in s["imes"]],
            f"ime set {s['ime']}" if s["ime"] not in ("", "null") else "",
            f"settings put global stay_on_while_plugged_in {s['stay_on'] if s['stay_on'] != 'null' else 0}",
            f"settings put system screen_off_timeout {s['timeout']}" if s["timeout"] != "null" else "",
            f"settings put global low_power {s['low_power'] if s['low_power'] != 'null' else 0}",
            f"settings put system screen_brightness_mode {s['brightness_mode']}" if s.get("brightness_mode", "null") != "null" else "",
            f"settings put system screen_brightness {s['brightness']}" if s.get("brightness", "null") != "null" else "",
        ]
        for p, loc in s["locales"].items():
            # "Locales for <pkg> for user 0 are [en-US]" or "[]"
            inner = loc[loc.find("[") + 1: loc.rfind("]")] if "[" in loc else ""
            steps.append(f"cmd locale set-app-locales {p} --locales '{inner}'")
        for c in steps:
            if c:
                try:
                    ui.sh(c, timeout=30)
                except Exception as e:  # keep putting the rest back
                    print(f"  could not restore ({c}): {e}")
        if os.path.exists(STATE_FILE):
            os.remove(STATE_FILE)
        print("  phone restored: keyboards, screen timeout, stay-awake, brightness, power saving, battery state, app languages")

    # ---- set up for a session -----------------------------------------------------------------------
    def prepare(self, power_save):
        ui.sh("settings put global stay_on_while_plugged_in 7")  # AC, USB, wireless
        ui.sh("settings put system screen_off_timeout 600000")
        ui.sh(f"settings put global low_power {1 if power_save else 0}")
        for p in self.packages:
            ui.sh(f"cmd locale set-app-locales {p} --locales en-US")
        self.wake()

    def wake(self):
        ui.sh("settings put global stay_on_while_plugged_in 7")
        ui.sh("input keyevent KEYCODE_WAKEUP")
        time.sleep(0.5)
        ui.sh("wm dismiss-keyguard")
        time.sleep(0.8)

    def screen_on_for(self, minutes, brightness=1):
        """The screen kept on for a measurement with it on: a fixed brightness, the lowest by default (the
        display then costs least, so what the apps draw weighs most; manual, so the room's light does not
        decide it) and a timeout past the measurement; stay-awake-while-charging stays off, so a
        phone on a cable and one on battery keep the screen on the same way."""
        ui.sh("settings put system screen_brightness_mode 0")
        ui.sh(f"settings put system screen_brightness {brightness}")
        ui.sh("settings put global stay_on_while_plugged_in 0")
        ui.sh(f"settings put system screen_off_timeout {int((minutes + 5) * 60 * 1000)}")
        self.wake()

    def screen_off(self):
        """Screen off for real: nothing may keep it on during a measurement."""
        ui.sh("settings put global stay_on_while_plugged_in 0")
        ui.sh("input keyevent KEYCODE_SLEEP")
        time.sleep(2)

    # ---- facts ---------------------------------------------------------------------------------------
    def info(self):
        g = lambda p: ui.sh(f"getprop {p}").strip()
        bat = ui.sh("dumpsys battery")
        pick = lambda k: next((l.split(":", 1)[1].strip() for l in bat.split("\n") if l.strip().startswith(k + ":")), "?")
        return {
            "model": g("ro.product.model"), "android": g("ro.build.version.release"), "build": g("ro.build.PDA"),
            "battery_level": pick("level"), "battery_temp_c": int(pick("temperature")) / 10 if pick("temperature").isdigit() else None,
            "usb_powered": pick("USB powered"), "ac_powered": pick("AC powered"),
            # The fuel gauge's count, µAh: the battery's real charge, in steps of about 4 mAh on the S21 FE.
            "charge_uah": int(pick("Charge counter")) if pick("Charge counter").isdigit() else None,
        }
