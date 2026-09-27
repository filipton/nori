# bgtest: battery tests of Nori against other players, on a real phone

Each player (Nori, Musly, Navic, Symfonium) is wiped, logged in, set to a variant (EQ, AutoMix, offload,
crossfade…), started on the same playlist in the same order, and measured over the same stretch with the
screen off or its full-screen player on screen. Every step checks the screen it should reach (a screenshot
when it does not), and the phone is put back as it was whatever way the run ends.

```sh
tools/bgtest/run.py                      # in a terminal: one screen to pick the phone, runs, playlists…
tools/bgtest/run.py --list-devices
tools/bgtest/run.py --serial <serial> --wifi --server local --playlist bg-mp3,bg-quick \
    --runs nori:plain,nori:eq-automix,musly:default,navic:default,symfonium:default --minutes 20 --repeat 3
tools/bgtest/bgtest.py --list            # the apps and their variants
tools/bgtest/server.py                   # the local test server (below)
```

One run per phone at a time: a second one on the same serial refuses to start (a lock in `build/bgtest/`).
The media volume is set to the same step for every run (`--volume`, default 1) and put back afterwards,
like the brightness of the player scenario (`--brightness`, default 1).

A session that stopped (Ctrl+C, the computer asleep, the phone gone from adb) is continued with
`run.py --resume` (or `bgtest.py --resume [FOLDER]`): it shows what the newest session ran, what is left
and about when it would end, asks, then runs the rest with the session's own settings into the same
folder. Every session keeps its settings and plan in `session.json`; older ones are read from their log.

adb over Wi-Fi can drop (the phone's Wi-Fi dozing with the screen off, the router). Before every
run the Wi-Fi connection is made anew (`adb disconnect`/`connect`), and every adb call that
finds the phone gone reconnects (`adb disconnect`/`connect`) and waits up to 3 minutes for it; a run whose
batterystats still cannot be read is recorded as failed, and a phone that never comes back stops the
session cleanly (exit 3) for `--resume`, keeping the phone's record for the next run to put back (or
`bgtest.py --restore`).

With no terminal (an agent), `run.py` never asks: every choice is a flag, `ACTION:` lines are for the
person at the phone (unplug the cable…), and the last line is `RESULTS: <folder>`.

- `run.py`: the entry point. Picks the phone, moves a cabled one to adb over Wi-Fi and waits for the
  cable to be pulled (so it measures on battery), starts the local server, runs `bgtest.py`.
- `tui.py`: the picker `run.py` shows with no options.
- `bgtest.py`: the runs, the measuring (batterystats, the real current, AudioFlinger's view of the
  app's track, frames, network) and the tables.
- `apps/`: one module per player: log in, variants, start the playlist, download it, open the player.
- `phone.py`: what is changed on the phone, remembered first and put back.
- `ui.py`: adb and a UI driver that finds things by what the screen says.
- `server.py`: a local Navidrome (port 4540, admin/admin) with songs taken once from the server in
  `~/.music.pass`, in several formats: `bg-mp3`, `bg-flac-44`, `bg-flac-48`, `bg-flac-96`, `bg-48k`,
  `bg-mixed` and `bg-quick` (one-minute clips, every format twice, after three fillers for the skips).
- `SETTINGS.md`: where each player keeps each setting, and what was found testing them.

The APKs (`build/bgtest/apks/<package>.apk`, installed when a player is missing), the server's music and
database (`build/bgtest/server/`) and the results (`build/bgtest/results/<time>/`: `results.md`,
`runs.jsonl`, each run's batterystats, log and screenshots) stay out of git.

What the numbers are: batterystats' figures are estimates from CPU and wakelock time, without its fixed
audio-hardware and screen models (the same for every app, charged to an app only on some paths); the
real current (`mA`, `hours`) is measured only on battery. Musly and Navic decode in Android's mediacodec
service, which is added to their figure.
