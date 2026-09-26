"""Musly (Flutter, ExoPlayer). Its login fields carry no labels: they are filled in order, moving from
one to the next with Tab (a tap does not move the focus in this app)."""
import ui

from . import App


class Musly(App):
    pkg = "com.devid.musly"
    name = "musly"
    # No equaliser and no offload in Musly: its only setting that changes the playback path is the
    # crossfade (Settings > Playback > Smart Crossfade, a slider from Off to 12 s).
    variants = {"default": {}, "crossfade": {"crossfade_s": 6}}
    # Its Flutter tree keeps reporting the home screen while the player is on top (its title scrolls
    # without end, so the tree is never refreshed): the player is told by the screen itself.
    player_marker = "Connect to a Device"

    def player_open_on_screen(self):
        """The tab bar's row: its coloured and grey icons on the home screen, plain dark background under
        the full-screen player (seen on the S21 FE: home up to 250, player at most 44 in every channel)."""
        w, h = ui.screen_size()
        ys = [int(h * f) for f in (0.88, 0.90, 0.92)]
        xs = [int(w * f) for f in (0.17, 0.33, 0.5, 0.67, 0.83)]
        return all(max(ui.pixel(x, y)) < 90 for y in ys for x in xs)

    def login(self):
        self.step("open", self.launch, ["Skip", "Subsonic"])
        if ui.has("Skip"):
            self.step("skip the intro", lambda: ui.tap("Skip"), "Subsonic")

        def fill():
            f = ui.fields()
            if len(f) < 3:
                raise RuntimeError(f"expected 3 login fields, found {len(f)}")
            ui.tap_xy(*f[0].centre)
            ui.clear_focused()
            ui.text(self.url, hide=False)
            ui.sh("input keyevent KEYCODE_TAB")
            ui.text(self.user, hide=False)
            ui.sh("input keyevent KEYCODE_TAB")
            ui.text(self.password, hide=False)
            got = [n.text for n in ui.fields()[:2]]
            if got != [self.url, self.user]:
                raise RuntimeError("the address and user did not land in their fields")

        self.step("server address, user, password", fill)
        self.step("log in", lambda: ui.sh("input keyevent KEYCODE_ENTER"), "I Understand & Continue", timeout=60)
        self.step("privacy notice", lambda: ui.tap("I Understand & Continue"), "Library\nTab 2 of 3", timeout=60)

    def configure(self, variant):
        v = self.variants[variant]
        if not v:
            return
        self.step("settings", lambda: ui.tap("Settings"), "Playback\nTab 1 of 6")
        if v.get("crossfade_s"):
            def slide():
                bar = next(n for n in ui.dump() if n.cls.endswith("SeekBar"))
                a, b, c, d = bar.box
                # The slider's 13 stops (Off, 1…12 s) span its width.
                ui.tap_xy(a + (c - a) * v["crossfade_s"] // 12, (b + d) // 2)
            self.step(f"crossfade {v['crossfade_s']} s", slide, f"{v['crossfade_s']} seconds crossfade", exact=False)
        self.step("back", ui.back, "Library\nTab 2 of 3")

    def download_playlist(self):
        self.step("library", lambda: (ui.tap("Library\nTab 2 of 3"), ui.wait_scrolling(self.playlist, 60, exact=False)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist, exact=False), ui.tap(self.playlist, exact=False)), "Download playlist")
        self.step("download", lambda: ui.tap("Download playlist"))
        ui.try_tap("Download", wait=3)  # a confirmation, when it asks
        ui.back()

    def start_playlist(self):
        self.step("library", lambda: (ui.tap("Library\nTab 2 of 3"), ui.wait_scrolling(self.playlist, 60, exact=False)))
        self.step(f"open {self.playlist}", lambda: (ui.scroll_to(self.playlist, exact=False), ui.tap(self.playlist, exact=False)), "Shuffle")
        self.step(self.start_button.lower(), lambda: ui.tap(self.start_button))
