# Shared by tools/smoke.sh, tools/audio-e2e.sh and tools/feature-e2e.sh: sourced, not run.
#
#   --only <section>[,<section>]   run only those sections (each script lists its own with --list)
#   --list                         print the sections and exit
#   NORI_E2E_SERVER=real|local     real (default): the server in ~/.music.pass. local: tools/dev-server.sh's
#                                  Navidrome with its generated music, reached through tools/lying-proxy.py,
#                                  which states transcoded songs longer than they are (see docs/testing.md).
#
# Nothing here sleeps for a fixed time where a condition can be awaited: `wait_for <field> <value>
# <timeout>` polls tools/app.sh state until the field reads that value.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; app="$here/app.sh"
export ANDROID_SERIAL=${ANDROID_SERIAL:-emulator-5554}
# Acquire before touching the device. --list is read-only.
if [[ " $* " != *" --list "* && ",${NORI_HELD_RESOURCES:-}," != *",device:$ANDROID_SERIAL,"* ]]; then
  exec python3 "$here/with-resource.py" "device:$ANDROID_SERIAL" bash "$0" "$@"
fi
pkg=${NORI_PKG:-dev.nori.music}
# Other nori builds on the same device (perf, an old release) play and log under the same tag: a second
# player takes the audio focus and its lines read as this build's. Only the build under test runs.
pass=0; fail=0; ONLY=""; LIST=0; t0=$(date +%s)

# ---- arguments -------------------------------------------------------------------------------------------
ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --only) ONLY="$2"; shift 2 ;;
    --only=*) ONLY="${1#--only=}"; shift ;;
    --list) LIST=1; shift ;;
    *) ARGS+=("$1"); shift ;;
  esac
done
set -- "${ARGS[@]+"${ARGS[@]}"}"

# `want <section>`: is this section to run? Every section of a script is named in its SECTIONS, and
# `--list` prints them. Sections marked opt-in (OPT_IN) run only when named in --only.
want() {
  if [ "$LIST" = 1 ]; then return 1; fi
  if [ -z "$ONLY" ]; then [[ " ${OPT_IN:-} " != *" $1 "* ]]; return; fi
  [[ ",$ONLY," == *",$1,"* ]]
}
list_sections() {
  if [ "$LIST" = 1 ]; then echo "sections: ${SECTIONS:-}"; [ -n "${OPT_IN:-}" ] && echo "opt-in (only with --only): $OPT_IN"; exit 0; fi
  if [ -n "$ONLY" ]; then
    local s; for s in ${ONLY//,/ }; do
      [[ " ${SECTIONS:-} ${OPT_IN:-} " == *" $s "* ]] || { echo "no section '$s' here; sections: ${SECTIONS:-} ${OPT_IN:-}" >&2; exit 2; }
    done
  fi
  for other in dev.nori.music dev.nori.music.perf dev.nori.music.old; do
    [ "$other" != "$pkg" ] && adb shell am force-stop "$other" >/dev/null 2>&1
  done
}

# ---- the server ------------------------------------------------------------------------------------------
# URL/USER/PASS: what this machine asks the server's API with (curl). APP_URL: what the app logs in to.
NORI_E2E_SERVER=${NORI_E2E_SERVER:-real}
if [ "$NORI_E2E_SERVER" = local ]; then
  proxy_port=${NORI_E2E_PROXY_PORT:-4534}
  if [[ "$ANDROID_SERIAL" == emulator-* && -z "${NORI_E2E_PROXY_PORT:-}" ]]; then
    proxy_port=$((4534 + ${ANDROID_SERIAL#emulator-} - 5554))
  fi
  URL="http://localhost:$proxy_port"; APP_URL="http://10.0.2.2:$proxy_port"
  USER="nori-e2e-$ANDROID_SERIAL"; PASS=nori-e2e
elif [ "$LIST" = 1 ]; then
  URL=""; USER=""; PASS=""; APP_URL=""
else
  URL=$(sed -n 1p ~/.music.pass); USER=$(sed -n 3p ~/.music.pass); PASS=$(sed -n 4p ~/.music.pass); APP_URL=$URL
fi
# The songs the checks play. On the real server, well-known songs a search finds; on the local one, the
# songs tools/dev-server.sh seeds. SONG and OTHER are off different albums (an album's own songs join
# gaplessly, so a pair of them proves nothing about crossfades); MIX_ALBUM has songs enough to mix.
if [ "$NORI_E2E_SERVER" = local ]; then
  SONG="search:Long Track 07"; OTHER="search:Far Song Two"; PLAIN="search:Long Track 04"
  LYRICS_SONG="search:Long Track 05"; MIX_ALBUM=""; TRANSCODED="search:Far Song One"
else
  SONG="search:paranoid android"; OTHER="search:nothing else matters"; PLAIN="search:creep"
  LYRICS_SONG="search:creep"; MIX_ALBUM=6Lt5zppPoP7FGBYqInxzZB; TRANSCODED=""
fi
# Subsonic wants token auth: t=md5(password+salt).
api() { local m="$1"; shift; local s=nori$RANDOM; local t
  t=$(printf '%s%s' "$PASS" "$s" | md5sum | cut -d' ' -f1)
  curl -s "$URL/rest/$m?u=$USER&t=$t&s=$s&v=1.16.1&c=nori&f=json$*"
}
# The first song a search finds, as the id: never a provider's (streaming one makes octo-fiesta fetch it).
song_id() { api search3 "&songCount=10&artistCount=0&albumCount=0&query=$(python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1]))' "$1")" | python3 -c "
import sys,json
for s in json.load(sys.stdin)['subsonic-response'].get('searchResult3',{}).get('song',[]):
    if not s['id'].startswith('ext-') and s.get('suffix')!='Remote': print(s['id']); break"; }

# The album to mix across: the local server's seeded one, found by name.
mix_album() {
  if [ -n "$MIX_ALBUM" ]; then echo "$MIX_ALBUM"; return; fi
  api search3 "&albumCount=1&songCount=0&artistCount=0&query=Long%20Album" | python3 -c "
import sys,json
print(json.load(sys.stdin)['subsonic-response']['searchResult3']['album'][0]['id'])" 2>/dev/null
}
# A song of the server's own, long enough to still be playing a minute on: a random one on the real
# server (a thirteen-second interlude would end before the check looks), a seeded one locally.
long_song() {
  # Not one of the Long Album's: the bridge needs that album with nothing of it downloaded.
  if [ "$NORI_E2E_SERVER" = local ]; then song_id "Far Song Three"; return; fi
  api getRandomSongs "&size=30" | python3 -c "
import sys,json
songs=[s for s in json.load(sys.stdin)['subsonic-response']['randomSongs']['song'] if not s['id'].startswith('ext-') and s.get('suffix')!='Remote']
s=next((s for s in songs if s.get('duration',0) >= 90), songs[0])
print(s['id'])"
}

# The local server: Navidrome (tools/dev-server.sh, which also seeds the songs these checks need) and the
# proxy in front of it. Both are left running for the next run.
local_server_up() {
  [ "$NORI_E2E_SERVER" = local ] || return 0
  python3 "$here/with-resource.py" local-server "$here/dev-server.sh" >/dev/null || { echo "tools/dev-server.sh failed" >&2; exit 1; }
  python3 "$here/with-resource.py" local-server python3 "$here/dev-user.py" "$USER" || return 1
  if ! curl -sf -m 2 "$URL/ping" >/dev/null; then
    nohup python3 "$here/lying-proxy.py" "$proxy_port" http://localhost:4533 >/dev/null 2>&1 &
    wait_until 10 curl -sf -m 1 "$URL/ping" >/dev/null || return 1
  fi
}

# ---- the app ---------------------------------------------------------------------------------------------
state() { "$app" state; }
# `field <name>`: one field of one state reading. `fields <a> <b>...`: several of the same reading, one per line.
field() { state | python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))" 2>/dev/null; }
fields() { state | python3 -c "
import sys,json;d=json.load(sys.stdin)
for k in sys.argv[1:]: print(d.get(k,''))" "$@" 2>/dev/null; }

# `wait_for <field> <value> <timeout s>`: until the field reads the value (a leading ! waits for it to read
# anything else, and >N / <N for a number past N). Fails after the timeout, saying what it last read.
WAITED=""
wait_for() {
  local f="$1" want="$2" limit="$3" v="" end=$(( $(date +%s) + $3 ))
  while :; do
    v=$(field "$f")
    case "$want" in
      !*) [ -n "$v" ] && [ "$v" != "${want#!}" ] && break ;;
      \>*) [ -n "$v" ] && python3 -c "import sys;sys.exit(0 if float('$v')>float('${want#>}') else 1)" 2>/dev/null && break ;;
      \<*) [ -n "$v" ] && python3 -c "import sys;sys.exit(0 if float('$v')<float('${want#<}') else 1)" 2>/dev/null && break ;;
      *) [ "$v" = "$want" ] && break ;;
    esac
    [ "$(date +%s)" -ge "$end" ] && { WAITED="$v"; echo "     (waited ${limit}s for $f $want, last read '$v')" >&2; return 1; }
    sleep 0.3
  done
  WAITED="$v"; return 0
}
# `wait_until <timeout s> <command...>`: until the command succeeds.
wait_until() {
  local end=$(( $(date +%s) + $1 )); shift
  until "$@"; do [ "$(date +%s)" -ge "$end" ] && return 1; sleep 0.3; done
}

check() { # check <name> <command...>
  local name="$1"; shift
  if "$@"; then echo "  PASS  $name"; pass=$((pass+1)); else
    echo "  FAIL  $name"; fail=$((fail+1))
    local artifacts="$here/../build/e2e/$ANDROID_SERIAL/failures/$t0"
    mkdir -p "$artifacts"
    "$here/ui-dump.sh" > "$artifacts/$fail.xml" 2>/dev/null
    adb exec-out screencap -p > "$artifacts/$fail.png" 2>/dev/null
    return 1
  fi
}
section() { echo "-- $1 ($(( $(date +%s) - t0 )) s)"; }
finish() {
  echo "== $pass passed, $fail failed in $(( $(date +%s) - t0 )) s"
  [ "$fail" -eq 0 ]
}

# Is audio actually flowing? What the system says about this app's own AudioTrack: instant and
# background-safe. The media session's position is not updated periodically (it would cost wakeups), and
# the bytes this app hands to the track arrive in ten-second bursts, so neither can say it quickly.
# Only this app's tracks: the list also holds other apps' and dead processes' tracks.
track_state() {
  local pid; pid=$(adb shell pidof "$pkg" | tr -d '\r')
  [ -n "$pid" ] || return 0
  local states; states=$(adb shell dumpsys audio | grep -oE "type:android.media.AudioTrack u/pid:[0-9]+/$pid state:[a-z]+" |
    grep -oE "state:[a-z]+" | sed 's/state://')
  # Several of ours can be listed (a track rebuilt at a format change); playing means one is started.
  if echo "$states" | grep -qx started; then echo started; else echo "$states" | head -1; fi
}
playing_audio() { [ "$(track_state)" = "started" ]; }
stopped() { [ "$(track_state)" != "started" ]; }
# `sounds [timeout]`: the track is started within the timeout (default 10 s).
sounds() { wait_until "${1:-10}" playing_audio; }
silent() { wait_until "${1:-10}" stopped; }
# The bursts keep coming: the bytes written grow past what they were, within one buffer cycle (the track
# holds ten seconds, so a top-up is due within about that).
bursts_continue() {
  playing_audio || return 1
  local a; a=$(field sinkBytes)
  wait_for sinkBytes ">${a:-0}" 15 >/dev/null
}

# The app in the foreground, answering, logged in to this run's server.
fixture_logged_in=0
app_up() {
  "$app" wake >/dev/null
  "$app" launch >/dev/null
  wait_until 20 bash -c "'$app' state 2>/dev/null | grep -q '\"route\"'" || return 1
  if [ "$(field server)" != "$APP_URL" ] || { [ "$NORI_E2E_SERVER" = local ] && [ "$fixture_logged_in" = 0 ]; }; then
    echo "     logging in to $([ "$NORI_E2E_SERVER" = local ] && echo "$APP_URL" || echo "the real server")"
    "$app" login "$APP_URL|$USER|$PASS" >/dev/null
    wait_for server "$APP_URL" 30 >/dev/null || return 1
    fixture_logged_in=1
  fi
  wait_for loggedIn True 30 >/dev/null
}

# Logcat captured from now on, for lines that come and go faster than app.sh (which clears the log to
# read its own answers) can see them.
watching=$(mktemp); watcher=""
watch_from_now() {
  [ -n "$watcher" ] && kill "$watcher" 2>/dev/null
  adb logcat -c
  adb logcat -v time -s nori:I > "$watching" 2>/dev/null &
  watcher=$!
}
# And one capture for the whole run, for the player's errors: `adb logcat -d` sees only what came since
# the last app.sh call, which cleared the log.
runlog=$(mktemp); runwatcher=""
whole_run_log() { adb logcat -c; adb logcat -v brief -s nori:* > "$runlog" 2>/dev/null & runwatcher=$!; }
run_errors() { grep -cE "rust player error: " "$runlog"; }
cleanup_e2e() {
  restore_settings
  [ "${network_off:-0}" = 0 ] || online
  if declare -F cleanup_feature >/dev/null; then cleanup_feature; fi
  for pid in "$watcher" "$runwatcher"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; fi
  done
  rm -f "$watching" "$runlog"
}
trap cleanup_e2e EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

# The settings a script changes, read at its start and put back at its end (the ones state reports).
SAVED=()
remember_settings() { local v; v=$(fields "$@"); local i=0; for k in "$@"; do i=$((i+1)); SAVED+=("$k $(echo "$v" | sed -n "${i}p" | tr 'TF' 'tf')"); done; }
restore_settings() { local s; for s in "${SAVED[@]+"${SAVED[@]}"}"; do "$app" set $s >/dev/null; done; SAVED=(); }
logged() { grep -qE "$1" "$watching"; }
never() { ! logged "$1"; }
waitfor_log() { wait_until "$2" logged "$1"; }

# The network back on, and the server reachable from the phone again: the emulator's Wi-Fi takes anywhere
# from two seconds to twenty to come back, and a check made before it has is testing the Wi-Fi, not the app.
network_off=0
offline() { network_off=1; adb shell svc wifi disable; adb shell svc data disable; }
online() {
  adb shell svc wifi enable; adb shell svc data enable
  local host; host=$(printf '%s' "$APP_URL" | sed -E 's#https?://##; s#[/:].*##')
  wait_until 30 adb shell "ping -c 1 -W 1 $host" >/dev/null 2>&1 || return 1
  network_off=0
}

# Crashes and ANRs of this app since `since` (device time, "YYYY-MM-DD HH:MM:SS"), from the dropbox.
device_now() { adb shell date '+%Y-%m-%d\ %H:%M:%S' | tr -d '\r'; }
dropbox_since() { # dropbox_since <tag> <since>: how many entries of this app
  adb shell dumpsys dropbox --print "$1" 2>/dev/null | tr -d '\r' | python3 -c "
import sys,re
since=sys.argv[1]; pkg=sys.argv[2]; n=0; cur=None
for line in sys.stdin:
    m=re.match(r'^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d) ', line)
    if m: cur=m.group(1); continue
    if cur and cur>=since and ('Process: '+pkg) in line: n+=1; cur=None
print(n)" "$2" "$pkg"
}
