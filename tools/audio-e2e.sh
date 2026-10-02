#!/usr/bin/env bash
# Audio, end to end, where only a device can say: the AudioTrack playing, paused and resumed from media
# keys, the background and a screen that is off; offload taken up and given back; the equalizer's in-place
# resize of the track; AutoMix measuring songs out of the media3 cache; a seek surviving a force stop; a
# transcode whose stated length the bytes never reach. How music is mixed, sped up, skipped, sought and
# planned is Rust's and tested there (cargo test; docs/testing.md lists what moved where).
#   tools/audio-e2e.sh [--only <section>,...] [--list] [song-ref] [other-song-ref]
#   NORI_E2E_SERVER=local for tools/dev-server.sh
source "$(dirname "$0")/e2e-lib.sh"
SECTIONS="play transport processing crossfade eq tuning automix restart transcode errors"
list_sections
song=${1:-$SONG}
other=${2:-$OTHER}
key() { adb shell input keyevent "$1"; }
whole_run_log
echo "== audio end to end against $([ "$NORI_E2E_SERVER" = local ] && echo "the local server" || echo "the real server")"
local_server_up
adb shell am force-stop "$pkg" >/dev/null 2>&1
app_up >/dev/null || { echo "the app did not come up"; exit 1; }
remember_settings offload autoMix eq
for s in "autoMix false" "crossfadeSec 0" "eq false" "speed 1" "pitch 1" "skipSilence false"; do "$app" set $s >/dev/null; done
playing_now() { [ "$(field playing)" = True ] && playing_audio || { "$app" play "$song" >/dev/null; sounds 20; }; }

if want play; then section play
  "$app" play "$song" >/dev/null
  check "plays a song" sounds 20
  check "and the bursts keep coming over a whole buffer" bursts_continue
fi

if want transport; then section "transport from outside the app"
  playing_now
  key KEYCODE_MEDIA_PAUSE; check "media key pause stops the track" silent 10
  key KEYCODE_MEDIA_PLAY; check "media key resume" sounds 10
  adb shell input keyevent KEYCODE_HOME
  key KEYCODE_MEDIA_PAUSE; silent 10
  # A real wait, the one thing measured here: the app in the background, paused long enough for the
  # system to treat it as idle (the service drops to the background, the track is stopped and kept).
  sleep 20
  key KEYCODE_MEDIA_PLAY
  check "resume after 20 s paused in the background" sounds 10
  check "and the bursts keep coming" bursts_continue
  adb shell input keyevent KEYCODE_SLEEP
  key KEYCODE_MEDIA_PAUSE; check "media key pause with the screen off" silent 10
  key KEYCODE_MEDIA_PLAY; check "and resume with the screen off" sounds 10
  check "the bursts keep coming with the screen off" bursts_continue
  adb shell input keyevent KEYCODE_WAKEUP; adb shell wm dismiss-keyguard >/dev/null 2>&1
  app_up >/dev/null   # back in the foreground: the settings below need the app's own hooks
fi

if want processing; then section "offload and the equalizer switched while it plays"
  # The limiter, mono and AutoMix switched under the music are the engine's (engine.rs
  # the_limiter_and_mono_..., automix_switched_on_while_playing_...); here only what reaches the platform.
  playing_now
  for setting in "eq true" "eq false" "offload false" "offload true"; do
    "$app" set $setting >/dev/null
    case "$setting" in
      "eq true") wait_for dspActive True 10 >/dev/null ;;
      "offload false") wait_for offloadWanted False 10 >/dev/null ;;
      "offload true") wait_for offloadWanted True 10 >/dev/null ;;
    esac
    check "still playing after $setting" sounds 10
  done
fi

if want crossfade; then section "a crossfade on the device"
  # Where the mix is planned, held and heard, and how the bar walks through it, is checked sample for
  # sample in crates/player/tests/pipeline/crossfade.rs and crates/engine/tests; here only that a real
  # track plays a mix through without stopping.
  "$app" set crossfadeSec 8 >/dev/null
  "$app" play "$song" >/dev/null; sounds 20
  "$app" do "playnext $other" >/dev/null
  wait_for durationMs ">30000" 10 >/dev/null
  leaving=$(field title); dur=$(field durationMs)
  # 30 s before the end: a song with a long outro plans its mix up to about 20 s early, and a seek past
  # the planned start rightly goes straight on to the next song, with no mix to see.
  "$app" do "seek $((dur - 30000))" >/dev/null
  check "the mix is heard as the song ends" wait_for mixing True 45
  check "the next song plays out of it" wait_for title "!$leaving" 30
  check "with the track sounding throughout" sounds 5
  "$app" set crossfadeSec 0 >/dev/null
fi

if want eq; then section "the equalizer leaves without stopping the track"
  playing_now
  "$app" set eq true >/dev/null; wait_for dspActive True 10 >/dev/null
  "$app" set eq false >/dev/null
  check "still playing after the EQ leaves" sounds 5
fi

if want tuning; then section "the buffer is shallow in sight and deep out of sight, in place"
  # The app in sight trades the deep buffer for a sound change heard at once, and gives it back as it
  # leaves (the service's inSight, from the activity's controller). The AudioTrack is resized in place by
  # crates/android track.rs, which only a device has; what it plays is tested there on a simulated track.
  playing_now
  "$app" set eq true >/dev/null
  watch_from_now
  adb shell input keyevent KEYCODE_HOME
  check "out of sight, the deep buffer is back, in place" waitfor_log "deep again in place" 15
  check "still playing out of sight" sounds 5
  "$app" launch >/dev/null
  check "in sight, the shallow buffer, in place" waitfor_log "shallow [0-9]+ ms.*topped up at" 15
  "$app" set eq false >/dev/null
  check "a change in sight is made in place" waitfor_log "the change is heard after [0-9]+ ms" 10
  check "the track was never reopened or emptied for it" never "rust AudioTrack: .*(160|80) ms|emptied for the music"
  check "still playing" sounds 5
fi

if want automix; then section "AutoMix measures the songs coming up on the device"
  # The measuring reads songs out of the media3 cache through the Java side (crates/android measure.rs);
  # what is measured and planned from it is tested in crates/engine/tests/core.rs and the automix tests.
  "$app" play "album:$(mix_album)" >/dev/null; sounds 20
  watch_from_now
  "$app" set autoMix true >/dev/null
  check "measuring starts when AutoMix is switched on" waitfor_log "measuring ahead:" 20
  # A song is measured as it is fetched ahead, or from bytes already on the device; the log says when
  # neither has happened yet.
  check "the songs coming up are measured" waitfor_log "analysed [^ ]+ (ahead|as it came): [0-9]|not on the device yet" 120
  "$app" set autoMix false >/dev/null
fi

if want restart; then section "seek after a restart"
  # Pause, kill, reopen, seek while paused, play: the seek has to win over the restored position. The
  # queue is restored from the core's saved copy by a new service; the seek logic itself is
  # controls.rs a_seek_while_paused_sticks_and_play_resumes_from_it.
  "$app" play "$song" >/dev/null; sounds 20
  "$app" do "seek 10000" >/dev/null; wait_for positionMs ">9000" 10 >/dev/null
  "$app" do pause >/dev/null; wait_for playing False 10 >/dev/null
  adb shell am force-stop "$pkg" >/dev/null 2>&1
  app_up >/dev/null
  # Cold boot: wait for the queue to be back before touching it.
  wait_for title "!" 60 >/dev/null
  "$app" do "seek 30000" >/dev/null
  wait_for positionMs ">27000" 10 >/dev/null; b=$WAITED
  check "a seek while paused after a restart sticks ($b)" test "${b:-0}" -ge 27000 -a "${b:-0}" -le 33000
  "$app" do resume >/dev/null
  check "play resumes from the seek" wait_for positionMs ">30500" 10
  check "and sounds" sounds 10
fi

if want transcode; then section "a transcode stated longer than it is"
  if [ -z "$TRANSCODED" ]; then
    echo "  NOTE  only against the local server (NORI_E2E_SERVER=local), whose proxy overstates transcodes"
  else
    # tools/lying-proxy.py claims a quarter more bytes than the Opus transcode has and answers 416 past
    # the real end: the song has to play to its real end and the queue go on, with no error.
    "$app" set wifiQuality opus:128 >/dev/null
    "$app" set clearStreamCache true >/dev/null
    "$app" play "$TRANSCODED" >/dev/null
    check "the transcoded song plays" sounds 20
    "$app" do "enqueue $other" >/dev/null
    wait_for durationMs ">30000" 10 >/dev/null
    leaving=$(field title); dur=$(field durationMs)
    "$app" do "seek $((dur - 8000))" >/dev/null
    check "it plays past its real end into the next song" wait_for title "!$leaving" 30
    check "which sounds" sounds 10
    check "with no error" test -z "$(field error)"
    "$app" set wifiQuality raw >/dev/null
  fi
fi

if want errors; then section errors
  # The player's error events (RustPlayer.kt), over the whole run.
  errs=$(run_errors)
  check "no playback errors in the log ($errs)" test "$errs" -eq 0
fi
restore_settings
finish
