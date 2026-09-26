"""One module per player. Each app says how to log in and start the test playlist shuffled; the base
class does the rest (clean install state, permissions, checks that playback really runs).

Every step is `step(what, action, expect)`: do something, then wait until the screen shows what should
come next. A step that does not arrive at its screen fails the run with a screenshot, rather than the
harness tapping on into the wrong screen.
"""
import re
import time

import ui


class StepFailed(Exception):
    pass


class App:
    pkg = ""
    name = ""
    variants = {"default": {}}

    def __init__(self, creds, playlist, log, shots):
        self.url, self.user, self.password = creds
        self.playlist = playlist
        self.log = log
        self.shots = shots  # where failure screenshots go

    # ---- steps ---------------------------------------------------------------------------------------
    def step(self, what, action=None, expect=None, timeout=30.0, exact=True):
        self.log(f"    {self.name}: {what}")
        try:
            if action:
                action()
            if expect is None:
                return None
            if isinstance(expect, (list, tuple)):
                return ui.wait_any(list(expect), timeout, exact)
            return ui.wait_for(expect, timeout, exact)
        except Exception as e:
            path = f"{self.shots}/{self.name}-{re.sub(r'[^a-z0-9]+', '-', what.lower())[:40]}.png"
            try:
                ui.screenshot(path)
            except Exception:
                path = "(no screenshot)"
            raise StepFailed(ui.redact(f"{self.name}: step '{what}' failed: {e} — screenshot {path}"))

    # ---- lifecycle ----------------------------------------------------------------------------------
    def clean(self):
        """Data and cache wiped, as after a fresh install; runtime permissions given back."""
        ui.sh(f"am force-stop {self.pkg}")
        out = ui.sh(f"pm clear {self.pkg}")
        if "Success" not in out:
            raise StepFailed(f"{self.name}: pm clear said {out.strip()!r}")
        ui.sh(f"pm grant {self.pkg} android.permission.POST_NOTIFICATIONS")
        ui.sh(f"cmd locale set-app-locales {self.pkg} --locales en-US")

    def launch(self):
        ui.sh(f"monkey -p {self.pkg} -c android.intent.category.LAUNCHER 1")
        time.sleep(3)

    def login(self):
        raise NotImplementedError

    def configure(self, variant):
        """Settings for a variant (only Nori has some)."""

    # A label only the app's full-screen player shows, when the general check (no tab bar, or song times
    # or a seek bar on screen) does not work for it.
    player_marker = None

    # "Play" (the list in order: every app plays the same songs in the same order) or "Shuffle".
    start_button = "Play"

    def start_playlist(self):
        """Opens the test playlist and presses start_button on it."""
        raise NotImplementedError

    def open_player(self):
        """The full-screen player, from the mini player: the song playing now is found in the lower part
        of the screen (the mini player) and tapped; the player is open when that song's title is shown
        and the app's tab bar is gone. An app with its own way overrides this."""
        title = self.session()[1].split(",")[0].strip()
        if not title:
            raise StepFailed(f"{self.name}: no song to open the player on")
        h = ui.screen_size()[1]

        def tap_mini():
            nodes = ui.dump()
            mini = [n for n in nodes if title.lower() in n.label.lower() and n.box[1] > h * 0.6
                    and n.box[3] < h - ui.NAV_BAR]
            if not mini:
                raise RuntimeError(f"the mini player with {title!r} is not on screen")
            ui.tap_xy(*mini[-1].centre)
            time.sleep(1.5)

        def is_open():
            if self.player_open_on_screen() is True:
                return
            nodes = ui.dump()
            if self.player_marker and any(n.label == self.player_marker for n in nodes):
                return
            if not any(title.lower() in n.label.lower() for n in nodes):
                raise RuntimeError(f"{title!r} is not on screen")
            # Open when the tab bar is gone, or when the song's times or a seek bar show (a mini player shows
            # neither); some apps (Flutter) keep a covered tab bar in the tree under the player.
            tabs = [n for n in nodes if n.label.split("\n")[0] in ("Home", "Library", "Search") and n.box[1] > h * 0.8]
            timed = any(re.search(r"(^|\s|-)\d{1,2}:\d{2}(\s|$|/)", n.label) for n in nodes) or \
                any(n.cls.endswith("SeekBar") or "Slider" in n.cls for n in nodes)
            if len(tabs) >= 2 and not timed:
                raise RuntimeError("the tab bar is still there and no song times or seek bar: the player did not open")

        def is_open_soon():
            # The player slides up with an animation: looked at again for a few seconds.
            end = time.time() + 10
            while True:
                try:
                    return is_open()
                except RuntimeError:
                    if time.time() > end:
                        raise
                    time.sleep(1)

        self.step("open the full-screen player", tap_mini)
        self.step("the player is open", is_open_soon)
        try:
            ui.screenshot(f"{self.shots}/{self.name}-player.png")
        except Exception:
            pass

    def player_open_on_screen(self):
        """True when the pixels show the full-screen player, None when the app has no such check (the
        accessibility tree is used then)."""
        return None

    def stop(self):
        ui.sh("cmd media_session dispatch pause")
        ui.sh(f"am force-stop {self.pkg}")

    # ---- what the platform says about playback ------------------------------------------------------
    def session(self):
        """(state, title) of this app's media session: state 3 is playing."""
        out = ui.sh("dumpsys media_session")
        blocks = out.split("package=")
        for b in blocks[1:]:
            if b.startswith(self.pkg + "\n") or b.split("\n", 1)[0].strip() == self.pkg:
                m = re.search(r"state=PlaybackState \{state=\w*\((\d+)\)", b)
                t = re.search(r"description=([^\n]*)", b)
                return (int(m.group(1)) if m else -1, t.group(1).strip() if t else "")
        return (-1, "")

    def wait_playing(self, timeout=60.0):
        end = time.time() + timeout
        while time.time() < end:
            state, title = self.session()
            if state == 3:
                return title
            time.sleep(2)
        raise StepFailed(f"{self.name}: not playing after {timeout:.0f} s (session state {self.session()})")

    def skip(self, times):
        """Presses next `times` times, each time checking the song really changed and plays again."""
        title = self.wait_playing()
        for i in range(times):
            ui.sh("cmd media_session dispatch next")
            end = time.time() + 30
            while time.time() < end:
                time.sleep(2)
                state, now = self.session()
                if now != title and state == 3:
                    break
            else:
                raise StepFailed(f"{self.name}: skip {i + 1} did not move to another playing song")
            title = now
            time.sleep(4)
        return title


def all_apps():
    from . import musly, navic, nori, symfonium
    return {"nori": nori.Nori, "musly": musly.Musly, "navic": navic.Navic, "symfonium": symfonium.Symfonium}
