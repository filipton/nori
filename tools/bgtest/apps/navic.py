"""Navic (ExoPlayer, Android's MediaCodec). Its switches don't tell uiautomator whether they are on:
their colour is read from the screen (ui.switch_row_on)."""
import time

import ui

from . import App


class Navic(App):
    pkg = "paige.navic"
    name = "navic"
    variants = {
        "default": {},
        # Audio offload is off by default and experimental; it applies after a restart of the app. Gapless
        # goes off with it: Navic says the two may conflict, and a phone that offloads without gapless
        # (the S21 FE: OFFLOAD_SUPPORTED, not GAPLESS) otherwise keeps playing on the CPU.
        # Skipping with offload on leaves Navic buffering for good (seen on the S21 FE: state 6 after
        # "next", the offload output closed), so this variant starts at the first song, no skips.
        "offload": {"offload": True, "gapless": False, "no_skips": True},
        # The built-in equaliser is Android's own (android.media.audiofx), run in audioserver.
        "eq": {"eq": True},
        # Settings > Now Playing: Background style Static instead of Dynamic (a blurred, moving cover: a
        # second layer redrawn at ~110 fps, RenderThread a whole core) and Slider style Flat instead of
        # Squiggly (a wave redrawn every frame). Only the player screen changes: for the player scenario.
        "static-ui": {"static_ui": True},
    }

    def login(self):
        self.step("open", self.launch, "Instance URL")
        self.step("server address", lambda: (ui.tap("Instance URL"), ui.text(self.url)))
        self.step("user", lambda: (ui.tap("Username"), ui.text(self.user)))
        self.step("password", lambda: (ui.tap("Password"), ui.text(self.password)))
        # The title and the button both say "Log in": the button is the second.
        self.step("log in", lambda: ui.tap("Log in", n=2), "Playlists", timeout=120)
        end = time.time() + 180
        while any("Syncing" in t for t in ui.texts()) and time.time() < end:
            time.sleep(3)

    def configure(self, variant):
        v = self.variants[variant]
        if not v:
            return
        if v.get("static_ui"):
            self.step("settings", lambda: ui.tap("Settings"), "Now Playing")
            self.step("now playing settings", lambda: ui.tap("Now Playing"), "Background style")
            # The row then reads "Static // Choose static if you have performance issues".
            self.step("background static", lambda: (ui.tap("Background style"), ui.tap("Static"), ui.tap("OK")), "Static //", exact=False)
            self.step("slider flat", lambda: (ui.tap("Slider style"), ui.tap("Flat"), ui.tap("OK")), "Flat")
            ui.back()
            ui.back()
            self.step("home", lambda: None, "Playlists")
            return
        self.step("settings", lambda: ui.tap("Settings"), "Playback")
        self.step("playback settings", lambda: ui.tap("Playback"), "Audio effects")
        self.step("audio effects", lambda: ui.tap("Audio effects"), "Audio offload")
        if "gapless" in v:
            self.step(f"gapless {'on' if v['gapless'] else 'off'}", lambda: set_row_switch("Gapless playback", v["gapless"]))
        if v.get("offload"):
            self.step("audio offload on", lambda: set_row_switch("Audio offload", True))
        if v.get("eq"):
            self.step("equaliser", lambda: ui.tap("Equaliser"), "Equaliser source")
            self.step("source: built-in", lambda: (ui.tap("Equaliser source"), ui.tap("Built-in"), ui.tap("OK")), "1500mB")
            self.step("bass up", raise_bass)
            ui.back()
        ui.back(); ui.back(); ui.back()
        if v.get("offload"):
            # "Requires application restart."
            self.step("restart the app", lambda: (ui.sh(f"am force-stop {self.pkg}"), self.launch()), "Playlists", timeout=60)

    def start_playlist(self):
        # Home lists "Recently played" and then "Playlists", each with its "See all".
        self.step("all playlists", lambda: (ui.tap("See all", n=2), ui.wait_scrolling(self.playlist, 60)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist), ui.tap(self.playlist)), "Shuffle")
        self.step(self.start_button.lower(), lambda: ui.tap(self.start_button))

    def download_playlist(self):
        self.step("all playlists", lambda: (ui.tap("See all", n=2), ui.wait_scrolling(self.playlist, 60)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist), ui.tap(self.playlist)), "More")
        self.step("download", lambda: (ui.tap("More"), ui.tap("Download")))
        ui.back()
        ui.back()


def row_of(label):
    nodes = ui.dump()
    a = ui.find(label, nodes=nodes)
    rows = [n for n in nodes if n.clickable and n.box[1] <= a.box[1] and n.box[3] >= a.box[3]]
    return min(rows, key=lambda n: n.box[3] - n.box[1])


def set_row_switch(label, on):
    """The switch at the right of `label`'s row, set to `on` and checked by its colour."""
    for _ in range(2):
        row = row_of(label)
        x, y = row.box[2] - 110, (row.box[1] + row.box[3]) // 2
        r, g, b = ui.pixel(x, y)
        is_on = max(r, g, b) - min(r, g, b) > 40  # a coloured (on) track or thumb; grey when off
        if is_on == on:
            return
        ui.tap_xy(*row.centre)
        time.sleep(1.2)
    raise RuntimeError(f"the switch of {label!r} did not turn {'on' if on else 'off'}")


def raise_bass():
    """The first two of the five bands up by about +900 mB (the slider's upper half)."""
    tops = sorted([n for n in ui.dump() if n.text == "1500mB"], key=lambda n: n.box[0])
    bottoms = sorted([n for n in ui.dump() if n.text == "-1500mB"], key=lambda n: n.box[0])
    for top, bottom in list(zip(tops, bottoms))[:2]:
        x = (top.box[0] + top.box[2]) // 2
        mid = (top.box[3] + bottom.box[1]) // 2
        ui.tap_xy(x, mid - (mid - top.box[3]) * 6 // 10)
        time.sleep(0.6)
