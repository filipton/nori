"""Symfonium (from its official F-Droid repo, repo.symfonium.app). Paid app with a trial."""
import time

import ui

from . import App


class Symfonium(App):
    pkg = "app.symfonik.music.player"
    name = "symfonium"
    variants = {
        "default": {},
        # Settings > Playback > Transitions: crossfade is the fade curves (Smooth gives 6.0 s)…
        "crossfade": {"crossfade": True},
        # …and Smart fades works the crossfade out from the waveform ("may increase battery usage").
        "smartfades": {"crossfade": True, "smart": True},
        # Output settings > Telefon > Equalizer: the graphic equalizer, 31.5 and 63 Hz raised.
        "eq": {"eq": True},
        # Everything at once: smart crossfades and the equalizer.
        "smartfades-eq": {"crossfade": True, "smart": True, "eq": True},
    }

    def login(self):
        self.step("open", self.launch, "On a network server or cloud provider")
        self.step("network server", lambda: ui.tap("On a network server or cloud provider"), "(Open) Subsonic")
        self.step("Subsonic", lambda: ui.tap("(Open) Subsonic"), "Server URL")
        self.step("server address", lambda: (ui.tap("Server URL"), ui.text(self.url)))
        self.step("user", lambda: (ui.tap("Login"), ui.text(self.user)))
        self.step("password", lambda: (ui.tap("Password"), ui.text(self.password)))
        self.step("add the server", lambda: ui.tap("Add"), "Library", timeout=180)
        ui.try_tap("Dismiss")

    def configure(self, variant):
        v = self.variants[variant]
        if not v:
            return
        self.step("settings", lambda: ui.tap("Settings"), "Playback")
        self.step("playback settings", lambda: (top(), ui.tap("Playback")), "Transitions")
        if v.get("crossfade"):
            self.step("transitions", lambda: ui.tap("Transitions"), "Crossfade")
            if not ui.has("Fade out curve"):
                ui.tap("Crossfade")  # the section opens and closes on its title
            self.step("fade out curve smooth", lambda: (ui.tap("Fade out curve"), ui.tap("Smooth")), "Fade out duration")
            self.step("fade in curve smooth", lambda: (ui.tap("Fade in curve"), ui.tap("Smooth")), "Fade in duration")
            if v.get("smart"):
                self.step("smart fades on", lambda: ui.switch("Smart fades", True))
            if v.get("eq"):
                # Transitions and Output settings are both entries of the Playback page.
                self.step("back to playback settings", ui.back, "Output settings")
        if v.get("eq"):
            self.step("output settings", lambda: (top(), ui.tap("Output settings")), "Telefon")
            self.step("this phone", lambda: ui.tap("Telefon"), "Equalizer")
            self.step("equalizer", lambda: ui.tap("Equalizer"), "Graphic equalizer")
            self.step("graphic equalizer on", lambda: ui.switch("Graphic equalizer", True))
            self.step("bass up", raise_bass, "+8", exact=False)
        # Out of the settings screens one Back at a time, looking after each, until the main screen with
        # its tabs shows (how many Backs that takes depends on how the pages were opened).
        self.step("back to the main screen", back_to_tabs)
        self.step("home", lambda: ui.tap("Home"), "Library")

    def download_playlist(self):
        self.step("library", lambda: ui.tap("Library"), "Playlists")
        self.step("playlists", lambda: (ui.tap("Playlists"), ui.wait_scrolling(self.playlist, 180)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist), ui.tap(self.playlist)), "More actions")
        # "Sync" keeps the playlist on the device (Symfonium's offline files).
        self.step("sync for offline", lambda: (ui.tap("More actions"), ui.tap("Sync")))
        ui.try_tap("OK", wait=3)
        ui.back()
        ui.back()

    def start_playlist(self):
        self.step("library", lambda: ui.tap("Library"), "Playlists")
        # The first sync may still be bringing the playlists in.
        self.step("playlists", lambda: (ui.tap("Playlists"), ui.wait_scrolling(self.playlist, 180)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist), ui.tap(self.playlist)), "Shuffle")
        self.step(self.start_button.lower(), lambda: ui.tap(self.start_button))
        time.sleep(2)


def top():
    w, h = ui.screen_size()
    for _ in range(5):
        ui.sh(f"input swipe {w // 2} {int(h * 0.3)} {w // 2} {int(h * 0.9)} 200")


def raise_bass():
    ui.swipe_up(0.4)
    nodes = ui.dump()
    for label in ("31.5", "63"):
        n = next(x for x in nodes if x.text == label)
        bar = next(x for x in nodes if x.cls.endswith("SeekBar") and abs(x.box[1] - n.box[1]) < 60)
        ui.tap_xy(bar.box[0] + (bar.box[2] - bar.box[0]) * 3 // 4, (bar.box[1] + bar.box[3]) // 2)
        time.sleep(0.8)


def back_to_tabs():
    for _ in range(6):
        nodes = ui.dump()
        if ui.find("Home", nodes=nodes) and ui.find("Library", nodes=nodes) and ui.find("Search", nodes=nodes):
            return
        ui.back()
    raise RuntimeError("the main screen's tabs did not come back")
