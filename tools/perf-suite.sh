#!/usr/bin/env bash
# The whole performance suite for nori on one device, hands-off:
#   tools/perf-suite.sh <adb-serial> <server-url> [user=admin] [password=admin]
# Needs the dev server (tools/dev-server.sh) with the "Bench / Long Play" album (10-minute MP3s, one FLAC,
# "Noise 1" with synced lyrics). The device must be unlocked. Media volume is set to 0 for the run and restored.
# Results are appended to docs/performance/raw/perf-results.md as one table per device.
set -uo pipefail
export ANDROID_SERIAL=$1; url=$2; user=${3:-admin}; pass=${4:-admin}
here="$(cd "$(dirname "$0")" && pwd)"; pkg=dev.nori.music; ui="$here/ui.sh"; out="$here/../docs/performance/raw/perf-results.md"
apk="$here/../app/build/outputs/apk/release/app-release.apk"
model=$(adb shell getprop ro.product.model | tr -d '\r'); rel=$(adb shell getprop ro.build.version.release | tr -d '\r'); abi=$(adb shell getprop ro.product.cpu.abi | tr -d '\r')
size=$(adb shell wm size | grep -oE '[0-9]+x[0-9]+' | tail -1); w=${size%x*}; h=${size#*x}
say() { echo "== $*" >&2; }
# No keyboard for the duration of the run, whatever happens: see the note in ui.sh. Restored on exit,
# including when the run is interrupted, so the device is left as it was found.
"$here/ui.sh" kb off
trap '"$here/ui.sh" kb on' EXIT INT TERM
# Screen on, keyguard gone, and proven so: every UI step after a screen-off measurement depends on it.
wake() {
  for _ in 1 2 3 4 5; do
    adb shell input keyevent 224 >/dev/null 2>&1
    adb shell wm dismiss-keyguard >/dev/null 2>&1
    sleep 1
    local awake locked
    awake=$(adb shell dumpsys power | grep -c "mWakefulness=Awake")
    locked=$(adb shell dumpsys window | grep -c "isKeyguardShowing=true")
    [ "$awake" -ge 1 ] && [ "$locked" -eq 0 ] && return 0
    adb shell input swipe $((w/2)) $((h*8/10)) $((w/2)) $((h*2/10)) 200 >/dev/null 2>&1
  done
  say "WARNING: could not wake/unlock the device"
}
field() { grep -E "^$1" | head -1 | sed -E "s/^$1: *//"; }
session() { adb shell dumpsys media_session | grep -A9 "package=$pkg" | grep -oE "\{state=[A-Z_0-9]+" | head -1 | tr -d '{' | sed -E 's/=3$/=PLAYING/; s/=2$/=PAUSED/'; }
# row of the label, tapped near the right edge: for switches that carry no label of their own
tapright() { local y; y=$(adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1; adb shell cat /sdcard/ui.xml | python3 -c "
import sys,re; m=re.search(r'(?:text|content-desc)=\"'+re.escape(sys.argv[1])+r'\"[^>]*?bounds=\"\[\d+,(\d+)\]\[\d+,(\d+)\]\"',sys.stdin.read()); print((int(m.group(1))+int(m.group(2)))//2 if m else '')" "$1"); [ -n "$y" ] && adb shell input tap $((w * ${2:-90} / 100)) "$y"; }
play() { # search for a song and tap it
  wake; adb shell am start -n $pkg/.app.MainActivity >/dev/null 2>&1; sleep 3
  "$ui" tapn Search 1 || true; sleep 2
  # Empty the field with the app's own Clear button: a burst of DEL leaves behind whatever it could not
  # reach, and the leftovers change what the search returns.
  "$ui" tap Clear 2>/dev/null; sleep 1
  # Tap the field by its placeholder, never by a fixed fraction of the screen: the search screen has a
  # title above the field and the offset moved when the UI was rebuilt.
  "$ui" tap "Songs, albums, artists" 2>/dev/null || adb shell input tap $((w / 2)) $((h * 145 / 1000))
  # Type only the first word: typing the whole title would make the search field itself the first node with that text.
  sleep 1; adb shell input text "$(echo "${1%% *}" | tr 'A-Z' 'a-z')"; sleep 3
  for _ in 1 2 3 4 5 6; do "$ui" has "$1" && break; sleep 2; done; "$ui" tap "$1"; sleep 8
  [ "$(session)" = "state=PLAYING" ] || say "WARNING: not playing after tapping $1 ($(session))"
}
bench() { "$here/bench.sh" $pkg "$1" "$2"; }
row() { printf '| %s | %s | %s | %s |\n' "$1" "$(echo "$2" | field cpu | sed -E 's/.*= //')" "$(echo "$2" | field quiet | sed -E 's/ seconds.*//')" "$(echo "$2" | field memory)" >> "$out"; }

say "$model, Android $rel, $abi, ${w}x$h"
vol=$(adb shell cmd media_session volume --stream 3 --get 2>/dev/null | grep -oE 'volume is [0-9]+' | grep -oE '[0-9]+' || true)
adb shell cmd media_session volume --stream 3 --set 0 >/dev/null 2>&1
trap '[ -n "${vol:-}" ] && adb shell cmd media_session volume --stream 3 --set "$vol" >/dev/null 2>&1; adb shell input keyevent 127; adb shell svc power stayon false' EXIT

# Always from source: a stale release APK silently makes the whole suite measure yesterday's build.
say "build release"; (cd "$here/.." && ./gradlew :app:assembleRelease -q) || say "WARNING: release build failed, installing whatever is on disk"
say "install"; adb install -r "$apk" >/dev/null || { adb uninstall $pkg >/dev/null; adb install "$apk" >/dev/null; }
adb shell pm grant $pkg android.permission.POST_NOTIFICATIONS 2>/dev/null
adb shell cmd package compile -m speed-profile -f $pkg >/dev/null 2>&1
wake; adb shell am force-stop $pkg; adb shell am start -W -n $pkg/.app.MainActivity >/dev/null; sleep 4
if "$ui" has "Server URL"; then
  say "login"; "$ui" tap "Server URL"; sleep 1; adb shell input text "$url"; adb shell input keyevent 61; adb shell input text "$user"; adb shell input keyevent 61; adb shell input text "$pass"
  adb shell input keyevent 4; sleep 1; "$ui" tap Connect; sleep 6
fi
"$ui" has "Shuffle everything" || { say "not logged in / home not showing; aborting"; "$ui" texts | head -12 >&2; exit 1; }

{ echo; echo "### $model · Android $rel · $abi · $(date +%F)"; echo; } >> "$out"

say "cold start x5"
starts=$(for _ in 1 2 3 4 5; do adb shell am force-stop $pkg; sleep 1; adb shell am start -W -n $pkg/.app.MainActivity | grep -oE 'TotalTime: [0-9]+' | grep -oE '[0-9]+'; done | sort -n | tr '\n' ' ')
echo "Cold start (ms, 5 runs sorted): $starts" >> "$out"; sleep 4

say "album grid scroll"; "$ui" tapn Library 1; sleep 3; "$here/scroll.sh" $pkg 6 >/dev/null
echo "Album grid fling, 300 albums: $("$here/scroll.sh" $pkg 10 | grep -E '50th|99th' | head -2 | sed -E 's/ percentile://' | tr '\n' ' ')" >> "$out"
{ echo; echo "| Scenario | CPU (% of one core) | Quiet seconds | Memory |"; echo "|---|---|---|---|"; } >> "$out"

say "idle, screen on"; "$ui" tapn Home 1; sleep 2; row "Idle on home, screen on (30 s)" "$(bench 30 on)"
say "MP3 screen off"; play "Noise 1"; adb shell input keyevent 3; r=$(bench 90 off); row "MP3 320, screen off (90 s)" "$r"
echo "$r" | sed -n '/busiest/,$p' | head -6 >&2
offload=$(adb shell dumpsys media.audio_flinger | grep -ciE "offload.*(active|tracks of which [1-9])" || true)
say "player visible"; wake; adb shell am start -n $pkg/.app.MainActivity >/dev/null 2>&1; sleep 3
"$ui" tap "Now playing bar"; sleep 3; "$ui" has Lyrics || { say "WARNING: player screen did not open"; "$ui" texts | tail -6 >&2; }; row "Player screen visible (45 s)" "$(bench 45 on)"
say "lyrics sweep"; "$ui" tap Lyrics; sleep 4; row "Lyrics, word sweep on (45 s)" "$(bench 45 on)"
adb shell input keyevent 4; sleep 1
say "FLAC screen off"; play "Noise flac"; adb shell input keyevent 3; row "FLAC, screen off (90 s)" "$(bench 90 off)"
say "DSP on"; adb logcat -c; wake; adb shell am start -n $pkg/.app.MainActivity >/dev/null 2>&1; sleep 2; "$ui" tapn Settings 1; sleep 2
for _ in 1 2 3 4 5 6 7 8; do "$ui" has "Equalizer and crossfeed" && break; adb shell input swipe $((w/2)) $((h*7/10)) $((w/2)) $((h*3/10)) 400; sleep 1; done
"$ui" tap "Equalizer and crossfeed"; sleep 2; tapright Equalizer 90; sleep 1; tapright 62 78; tapright 8k 35; sleep 1; adb shell input keyevent 4; sleep 1
adb logcat -d -s nori:I | grep -q "equalizer in chain" || say "WARNING: equalizer did not join the chain"
adb shell input keyevent 3; row "FLAC + equalizer, screen off (90 s)" "$(bench 90 off)"
wake; adb shell am start -n $pkg/.app.MainActivity >/dev/null 2>&1; sleep 2; "$ui" has Enabled || "$ui" tap "Equalizer and crossfeed"; sleep 2; tapright Equalizer 90; "$ui" tap Reset; adb shell input keyevent 4
say "paused"; adb shell input keyevent 127; sleep 3; adb shell input keyevent 3; row "Paused in background (30 s)" "$(bench 30 off)"
echo >> "$out"; echo "Audio offload threads active during MP3 playback: $offload" >> "$out"
say "done -> $out"
