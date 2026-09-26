"""Nori (this repo's player), perf build."""
import time

import ui

from . import App


class Nori(App):
    pkg = "dev.nori.music.perf"
    name = "nori"
    # Its defaults are what a fresh install has: load ahead 2 songs on Wi-Fi, 1 on mobile data.
    variants = {
        # Nothing changed: offloaded when the song's format allows (see SETTINGS.md).
        "plain": {},
        # The equalizer on (Bass boost): the CPU path, no offload.
        "eq": {"eq": True},
        "automix": {"automix": True},
        "automix-ml": {"automix": True, "ml": True},
        # Everything on at once, as it would be used: the equalizer with AutoMix, and with its beat model.
        "eq-automix": {"eq": True, "automix": True},
        "eq-automix-ml": {"eq": True, "automix": True, "ml": True},
    }

    def login(self):
        self.step("open", self.launch, "Server URL")
        self.step("server address", lambda: (ui.tap("Server URL"), ui.text(self.url)))
        self.step("user", lambda: (ui.tap("User"), ui.text(self.user)))
        self.step("password", lambda: (ui.tap("Password"), ui.text(self.password)))
        self.step("connect", lambda: ui.tap("Connect"), "Listen now", timeout=60)

    def configure(self, variant):
        v = self.variants[variant]
        if not v:
            return
        self.step("settings", lambda: ui.tap("Settings", n=1), "Playback")
        if v.get("eq"):
            self.step("sound settings", lambda: ui.tap("Sound"), "Equalizer and crossfeed")
            self.step("equalizer", lambda: ui.tap("Equalizer and crossfeed"), "Bass boost")
            self.step("equalizer on, bass boost", lambda: (ui.switch("Equalizer", True), ui.tap("Bass boost")), "+6.0")
            self.step("back to settings", lambda: (ui.back(), ui.back()), "Playback")
        if not (v.get("automix") or v.get("ml")):
            self.step("back to home", lambda: ui.tap("Home"), "Listen now")
            return
        self.step("playback settings", lambda: ui.tap("Playback"), "AutoMix")
        if v.get("automix"):
            self.step("AutoMix on", lambda: ui.switch("AutoMix", True), "Better beat detection")
        if v.get("ml"):
            self.step("better beat detection on", lambda: ui.switch("Better beat detection", True))
            # The model's weights (about 8 MB) come from its authors' server the first time.
            time.sleep(10)
        # One Back leaves Playback for Settings; Home is a tab (a second Back could leave the app).
        self.step("back to home", lambda: (ui.back(), ui.tap("Home")), "Listen now")

    def download_playlist(self):
        self.step("library", lambda: ui.tap("Library"), "Playlists")
        self.step("playlists", lambda: (ui.tap("Playlists"), ui.wait_scrolling(self.playlist, 60)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist), ui.tap(self.playlist)), "More")
        self.step("download", lambda: (ui.tap_xy(*[x for x in ui.dump() if x.label == "More"][0].centre), ui.tap("Download")))
        ui.back()
        ui.back()

    def start_playlist(self):
        self.step("library", lambda: ui.tap("Library"), "Playlists")
        self.step("playlists", lambda: (ui.tap("Playlists"), ui.wait_scrolling(self.playlist, 60)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist), ui.tap(self.playlist)), "Shuffle")
        self.step(self.start_button.lower(), lambda: ui.tap(self.start_button))
