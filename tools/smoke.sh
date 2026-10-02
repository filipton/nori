#!/usr/bin/env bash
# The smoke tier: the Android glue a change can break, in about two minutes on an installed debug build.
# What Rust owns (mixing, the queue, the planner, the settings' effects) is tested by `cargo test`; this
# checks that the app around it still launches, plays through a real AudioTrack, answers its controls and
# notification, and has not crashed. Exits non-zero on any failure. See docs/testing.md.
#   tools/smoke.sh [--only <section>,...] [--list]      NORI_E2E_SERVER=local for tools/dev-server.sh
source "$(dirname "$0")/e2e-lib.sh"
SECTIONS="launch play controls queue automix eq offload notification offline crashes"
list_sections
since=$(device_now)
whole_run_log
echo "== smoke against $([ "$NORI_E2E_SERVER" = local ] && echo "the local server" || echo "the real server")"
local_server_up

if want launch; then section launch
  adb shell am force-stop "$pkg" >/dev/null 2>&1
  check "the app launches and answers" app_up
  check "logged in to this run's server" test "$(fields loggedIn server | tr '\n' ' ')" = "True $APP_URL "
else
  app_up >/dev/null
fi
remember_settings offload autoMix eq
# Every section starts from the settings that let the plain path play: nothing mixing, nothing in the chain.
plain() { for s in "autoMix false" "crossfadeSec 0" "eq false" "offload false" "speed 1" "crossfadeKeepAlbums true"; do "$app" set $s >/dev/null; done; }
plain

if want play; then section play
  "$app" play "$SONG" >/dev/null
  check "a song plays through the AudioTrack" sounds 20
  check "and its bursts keep coming" bursts_continue
fi

if want controls; then section "controls: pause, play, seek, next"
  [ "$(field playing)" = True ] || { "$app" play "$SONG" >/dev/null; sounds 20; }
  "$app" do pause >/dev/null
  check "pause stops the track" silent 10
  "$app" do resume >/dev/null
  check "play starts it again" sounds 10
  "$app" do "seek 60000" >/dev/null
  check "a seek lands where asked" wait_for positionMs ">59000" 10
  check "and stays near it (${WAITED} ms)" test "${WAITED:-0}" -lt 70000
  was=$(field title)
  "$app" do next >/dev/null
  check "next moves to another song" wait_for title "!$was" 15
  check "and it sounds" sounds 15
fi

if want queue; then section "a queue edit"
  before=$(field queue)
  "$app" do "enqueue $OTHER" >/dev/null
  check "adding to the queue grows it (from $before)" wait_for queue ">${before:-0}" 10
fi

if want automix; then section "one AutoMix transition"
  album=$(mix_album)
  "$app" set crossfadeKeepAlbums false >/dev/null; "$app" set autoMix true >/dev/null
  "$app" play "album:$album" >/dev/null
  sounds 20
  # A cold start on the real server can take a while to know the length; the seek needs it.
  wait_for durationMs ">30000" 30 >/dev/null
  leaving=$(field title); dur=$(field durationMs)
  # 30 s before the end: a song with a long outro plans its mix up to about 20 s early, and a seek past
  # the planned start rightly goes straight on to the next song, with no mix to see.
  "$app" do "seek $((dur - 30000))" >/dev/null
  check "the mix is heard as the song ends" wait_for mixing True 45
  check "and the next song plays out of it" wait_for title "!$leaving" 30
  check "with the track still sounding" sounds 10
  plain
fi

if want eq; then section "the equalizer, on and off; the buffer deep out of sight, shallow in sight, in place"
  [ "$(field playing)" = True ] || { "$app" play "$SONG" >/dev/null; sounds 20; }
  watch_from_now
  "$app" set eq true >/dev/null
  check "the equalizer goes in" wait_for dspActive True 10
  check "made in place in the ring" waitfor_log "the change is heard after [0-9]+ ms" 10
  adb shell input keyevent KEYCODE_HOME
  check "out of sight, the deep buffer is back, in place" waitfor_log "deep again in place" 15
  check "still sounding out of sight" sounds 5
  "$app" launch >/dev/null
  check "in sight, the shallow buffer, in place" waitfor_log "shallow [0-9]+ ms.*topped up at" 15
  check "the track was not reopened or emptied for it" never "rust AudioTrack: .*(160|80) ms|emptied for the music"
  "$app" set eq false >/dev/null
  check "still sounding with the equalizer off" sounds 5
fi

if want offload; then section "offload, on and off"
  "$app" play "$PLAIN" >/dev/null; sounds 20
  "$app" set offload true >/dev/null
  check "offload is asked for on the phone's own output" wait_for offloadWanted True 10
  if wait_for offloaded True 15 2>/dev/null; then
    check "a song is offloaded to the chip" true
    check "and sounds" sounds 10
  else
    # An emulator's audio HAL may take no compressed stream at all; the rule is then the only thing to check.
    echo "  NOTE  nothing was offloaded: this device's output takes no compressed stream"
  fi
  "$app" set offload false >/dev/null
  check "offload off brings it back to the CPU" wait_for offloaded False 10
  check "still sounding" sounds 10
fi

if want notification; then section "the notification's play and pause"
  [ "$(field playing)" = True ] || { "$app" play "$SONG" >/dev/null; sounds 20; }
  check "the playback notification is posted" bash -c "adb shell dumpsys notification --noredact 2>/dev/null | grep -q '$pkg|1001'"
  tap_media() { # the media controls' button by its description, in the pulled-down shade
    local c; c=$(adb shell uiautomator dump /sdcard/nori-shade.xml >/dev/null 2>&1; adb shell cat /sdcard/nori-shade.xml | python3 -c "
import sys,re
for m in re.finditer(r'<node[^>]*>', sys.stdin.read()):
    n=m.group(0)
    if 'content-desc=\"$1\"' in n and 'systemui' in n:
        b=[int(v) for v in re.findall(r'\d+', re.search(r'bounds=\"([^\"]*)\"', n).group(1))]
        print((b[0]+b[2])//2, (b[1]+b[3])//2); break")
    [ -n "$c" ] && adb shell input tap $c
  }
  adb shell cmd statusbar expand-notifications >/dev/null 2>&1
  if wait_until 5 tap_media Pause; then
    check "its pause button pauses" wait_for playing False 10
    wait_until 5 tap_media Play
    check "its play button plays" wait_for playing True 10
  else
    echo "  NOTE  no media controls found in the shade; the session's own pause and play instead"
    adb shell cmd media_session dispatch pause >/dev/null 2>&1
    check "the session's pause pauses" wait_for playing False 10
    adb shell cmd media_session dispatch play >/dev/null 2>&1
    check "the session's play plays" wait_for playing True 10
  fi
  adb shell cmd statusbar collapse >/dev/null 2>&1
  check "and the track sounds again" sounds 10
fi

if want offline; then section "a download plays offline"
  id=$(long_song)
  # This download, not whatever the count held before: right after a server switch it can still be the
  # other server's, and a song downloaded by an earlier run is already there.
  before=$(field downloaded)
  held=$(adb exec-out run-as "$pkg" sqlite3 files/nori.db "select count(*) from downloads where id='$id' and done=1" 2>/dev/null | tr -dc 0-9)
  [ "${held:-0}" -gt 0 ] && before=$((before - 1))
  "$app" do "download song:$id" >/dev/null
  dl_done() { local v; v=$(fields downloading downloaded | tr '\n' ' '); set -- $v; [ "${1:-1}" = 0 ] && [ "${2:-0}" -gt "${before:-0}" ]; }
  # A whole song over the internet from the real server can take longer than from the local one.
  check "the download finishes" wait_until "$([ "$NORI_E2E_SERVER" = local ] && echo 60 || echo 180)" dl_done
  offline
  adb shell am force-stop "$pkg" >/dev/null 2>&1; "$app" launch >/dev/null
  # From the device's own list: looking the song up by id would need the network and prove nothing. This
  # song, not the newest in the list (a song downloaded by an earlier run was queued before the others),
  # once the restarted app has read its downloads.
  check "the download is still there after the restart" wait_for downloaded ">0" 20
  "$app" play "downloaded:$id" >/dev/null
  check "a downloaded song plays with the network off" sounds 20
  online
fi

if want crashes; then section "crashes and ANRs"
  "$app" do pause >/dev/null 2>&1
  crashes=$(dropbox_since data_app_crash "$since"); anrs=$(dropbox_since data_app_anr "$since")
  check "no crash of $pkg since $since ($crashes)" test "${crashes:-0}" = 0
  check "no ANR of $pkg since $since ($anrs)" test "${anrs:-0}" = 0
  errs=$(run_errors)
  check "no playback errors in the whole run's log ($errs)" test "$errs" -eq 0
  # Which ones: the run's log is gone once the script ends.
  [ "$errs" -eq 0 ] || grep -E "rust player error: " "$runlog" | head -5 | sed 's/^/        /'
fi
restore_settings
finish
