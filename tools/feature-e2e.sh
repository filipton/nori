#!/usr/bin/env bash
# The rest of the app where only a device can say: the screens' buttons and what they start, the
# notification's commands, downloads through media3 and its notifications, playing offline and the
# offline bridge as the network really goes, the mock DAC's track, and a device's sound on connect. The
# decisions behind them (stars, playlists, scrobbling, lyrics choice, mixes, what plays when the queue
# runs out, the DAC's modes, AutoEQ) are the core's and tested by cargo test against fake servers;
# docs/site/developers/testing.md lists what moved where. Checks that the server has to agree with ask the server's API.
#   tools/feature-e2e.sh [--only <section>,...] [--list]     NORI_E2E_SERVER=local for tools/dev-server.sh
#   (the real server's credentials come from ~/.music.pass: url, blank, user, password)
source "$(dirname "$0")/e2e-lib.sh"
SECTIONS="lyrics motion notification album-page bridge download-notification downloads foryou dac device-sound remote jam"
OPT_IN="lyrics-services"
list_sections
json() { python3 -c "import sys,json;d=json.load(sys.stdin)['subsonic-response'];print(eval('d$1',{'d':d}))" 2>/dev/null; }
# What the screen itself reports, read out of the accessibility tree rather than guessed at from a
# screenshot: a label to assert on, and a node to press where the UI says the button is.
ui() { "$here/ui-dump.sh"; }
pill() { ui | grep -oE 'text="(Play|Pause)"' | head -1 | cut -d'"' -f2; }
pill_is() { [ "$(pill)" = "$1" ]; }
tapnode_now() { # $1 = text|content-desc, $2 = that value, $3 = which of several (the first by default)
  local c; c=$(ui | python3 -c "
import sys,re
found=[]
for m in re.finditer(r'<node[^>]*>', sys.stdin.read()):
    a=re.search('$1=\"([^\"]*)\"', m.group(0))
    if a and a.group(1)=='$2':
        b=[int(v) for v in re.findall(r'\d+', re.search(r'bounds=\"([^\"]*)\"', m.group(0)).group(1))]
        found.append('%d %d' % ((b[0]+b[2])//2, (b[1]+b[3])//2))
if len(found) >= ${3:-1}: print(found[${3:-1}-1])")
  [ -n "$c" ] || return 1
  adb shell input tap $c
}
tapnode() { wait_until 10 tapnode_now "$@"; }
on_screen() { ui | grep -q -- "$1"; }
off_screen() { local tree; tree=$(ui) || return 1; ! printf '%s' "$tree" | grep -q -- "$1"; }
# Chrome may keep the landing page until its explicit app link is tapped.
open_invite() {
  local link="$1"
  [ -n "$link" ] || return 1
  if [ "${2:-}" = app ]; then
    link="nori://jam?${link#*#}"
    adb shell am start -a android.intent.action.VIEW -d "'$link'" >/dev/null 2>&1 || return 1
  else
    adb shell am start -a android.intent.action.VIEW -p com.android.chrome --ez create_new_tab true -d "'$link'" >/dev/null 2>&1 || return 1
  fi
  invite_opened() {
    local tree; tree=$(ui)
    if [[ "$tree" == *"package=\"$pkg\""* ]]; then return 0; fi
    for button in "Use without an account" "No thanks" "Got it" "Open in nori"; do
      if [[ "$tree" == *"text=\"$button\""* ]]; then tapnode text "$button"; break; fi
    done
    return 1
  }
  wait_until 15 invite_opened
}
starred_on_server() { api getStarred2 | python3 -c "
import sys,json
d=json.load(sys.stdin)['subsonic-response'].get('starred2',{})
print(any(s['id']=='$1' for s in d.get('song',[])))"; }

cleanup_feature() {
  [ "${feature_finished:-0}" = 0 ] || return 0
  if [ -n "${peer:-}" ]; then
    "$app" remote pick here >/dev/null
    "$app" set remoteControl false >/dev/null
    tmux kill-session -t "$peer" 2>/dev/null
  fi
  for guest_pid in "${gus:-}" "${dee:-}" "${host:-}"; do
    [ -z "$guest_pid" ] || kill "$guest_pid" 2>/dev/null
  done
  if [ -n "${app_jam:-}" ]; then
    "$app" set jam false >/dev/null
    "$app" login "$APP_URL|$USER|$PASS" >/dev/null
  fi
}

whole_run_log
echo "== features end to end against $([ "$NORI_E2E_SERVER" = local ] && echo "the local server" || echo "the real server")"
local_server_up
adb shell am force-stop "$pkg" >/dev/null 2>&1
app_up >/dev/null || { echo "the app did not come up"; exit 1; }
remember_settings offload autoMix eq
id=$(long_song)

if want lyrics; then section lyrics
  # Which answer wins, word timing, the lookups switch and every service's format are the lyrics crate's
  # (crates/lyrics tests, over recorded answers). Here: that an answer comes back through the platform.
  if [ "$NORI_E2E_SERVER" = local ]; then
    echo "  NOTE  the local server's generated songs have no lyrics anywhere; run against the real server"
  else
    "$app" set thirdPartyLookups true >/dev/null
    "$app" play "$LYRICS_SONG" >/dev/null; sounds 20
    "$app" do lyrics >/dev/null
    check "lyrics arrive for a well-known song" wait_for lyricLines ">0" 30
    echo "     $(field lyricLines) lines from $(field lyricsSource)"
  fi
fi

if want lyrics-services; then section "each lyrics service (a report, not a check)"
  # Each service asked on its own for the same song: whether the services still answer is theirs to
  # say, not the app's. A service's answer and a failure are kept in the response cache, so a second run
  # reads what the first found: clear the app's data to ask them all again.
  "$app" set lyricsOnline true >/dev/null
  "$app" play "$LYRICS_SONG" >/dev/null; sounds 20
  answered=0
  services="binilyrics better_lyrics paxsenix lyrics_plus portato paxsenix_musixmatch simpmusic unison netease kugou lrclib paxsenix_spotify youtube_captions megalobiz youtube_music genius"
  for s in $services; do
    "$app" set lyricsSources "$s" >/dev/null
    "$app" do lyrics >/dev/null; wait_for lyricsSource "!SERVER" 12 2>/dev/null
    src=$(field lyricsSource)
    echo "     $s: $(field lyricLines) lines from $src (synced=$(field lyricsSynced), wordTimed=$(field lyricsWordTimed))"
    [ "$src" != "SERVER" ] && answered=$((answered+1))
  done
  echo "     $answered of $(echo $services | wc -w) services answered"
  "$app" set lyricsSources default >/dev/null
fi

if want motion; then section "moving covers"
  # Off, which is the default, opening the player builds nothing: no video player and no lookup.
  "$app" set motionArtwork false >/dev/null; "$app" open player >/dev/null
  wait_for route player 5 >/dev/null
  check "switched off, the open player makes no video player" test "$(field motionPlayers)" = 0
  # On, over any network. Whether an album has one is Apple's to say, and a test server's generated
  # music has none, so a video found is reported here rather than required.
  "$app" set motionArtwork true >/dev/null; "$app" set motionArtworkWifiOnly false >/dev/null
  wait_for motionPlayers ">0" 8 2>/dev/null
  echo "     video for this album: '$(field motionVideo)', players: $(field motionPlayers)"
  "$app" open home >/dev/null
  check "put away, the video player is let go" wait_for motionPlayers 0 5
  "$app" set motionArtwork false >/dev/null; "$app" set motionArtworkWifiOnly true >/dev/null
  "$app" set thirdPartyLookups true >/dev/null
fi

if want notification; then section "the notification's heart and shuffle"
  # "notification <x>" sends the same session command the notification's button sends; "notification"
  # in the state is what the session last published to its controllers (the notification is one of them).
  # That the star reaches the server as Subsonic asks is client.rs a_heart_a_new_playlist_...
  "$app" play "song:$id" >/dev/null; sounds 20
  buttons=$(field notification); echo "     buttons: $buttons"
  check "the notification has a heart and a shuffle button" bash -c '[[ "'"$buttons"'" == *heart* && "'"$buttons"'" == *shuffle* ]]'
  was=$(field starred); flip=$([ "$was" = True ] && echo False || echo True)
  "$app" do "notification favourite" >/dev/null
  check "the notification's heart stars the song in the app ($was -> $flip)" wait_for starred "$flip" 10
  buttons=$(field notification)
  check "the notification's heart redraws ($buttons)" bash -c '[[ "'"$flip"'" == True && "'"$buttons"'" == *heart_filled* ]] || [[ "'"$flip"'" == False && "'"$buttons"'" != *heart_filled* ]]'
  "$app" do "notification favourite" >/dev/null; wait_for starred "$was" 10 >/dev/null   # put it back
  shuffle=$(field notification | grep -o 'shuffle_o[nf]*')
  "$app" do "notification shuffle" >/dev/null
  flipped=$([ "$shuffle" = shuffle_on ] && echo shuffle_off || echo shuffle_on)
  shuffled() { [ "$(field notification | grep -o 'shuffle_o[nf]*')" = "$1" ]; }
  check "the notification's shuffle toggles ($shuffle -> $flipped)" wait_until 5 shuffled "$flipped"
  "$app" do "notification shuffle" >/dev/null; wait_until 5 shuffled "$shuffle"   # put it back
  # The song's cover as the notification, the lock screen and the headphones open it (CarArtProvider):
  # drawn once, then read from the disk, with nothing in the app woken to draw it again.
  art="content://$pkg.carart/c/$(field cover)?s=800"
  adb shell "content read --uri '$art'" >/dev/null 2>&1
  drawn=$(field coversDrawn)
  kept_bytes=$(adb shell "content read --uri '$art' | wc -c" | tr -d '\r ')
  drawn_still() { [ "${kept_bytes:-0}" -gt 1000 ] && [ "$(field coversDrawn)" = "$drawn" ]; }
  check "the cover opened again is read from the disk, not drawn again ($kept_bytes bytes)" drawn_still
fi

if want album-page; then section "the album page answers for its own queue"
  # The hero's pills answer for the queue the page started: Play becomes Pause while that queue sounds,
  # and a second press on Shuffle switches shuffle off where it stands. What each press means is
  # pages.rs the_big_buttons_answer_for_the_pages_own_queue; here the taps on the real screen. Picked: an
  # album whose songs all run past a minute, all the server's own, and not the one playing now.
  "$app" wake >/dev/null
  aid=""; playing_title=$(field title)
  if [ "$NORI_E2E_SERVER" = local ]; then
    aid=$(mix_album)
  else
    for a in $(api getAlbumList2 "&type=recent&size=25" | python3 -c "
import sys,json
for x in json.load(sys.stdin)['subsonic-response']['albumList2'].get('album',[]):
    if not x['id'].startswith('ext-') and x.get('songCount',0) >= 2: print(x['id'])"); do
      d=$(api getAlbum "&id=$a" | python3 -c "
import sys,json
s=json.load(sys.stdin)['subsonic-response']['album']['song']
t='''$playing_title'''
print(0 if any(x.get('title')==t or x['id'].startswith('ext-') or x.get('suffix')=='Remote' for x in s) else min(x.get('duration',0) for x in s))" 2>/dev/null)
      [ "${d:-0}" -ge 60 ] && { aid=$a; break; }
    done
  fi
  if [ -z "$aid" ]; then
    echo "     (no album of songs a minute or more long; nothing to tap at)"
  else
    "$app" do pause >/dev/null
    "$app" open "album/$aid" >/dev/null
    check "the pill reads Play before the album is played" wait_until 8 pill_is Play
    "$app" play "album:$aid" >/dev/null; sounds 20
    check "the pill reads Pause while this album is what sounds" wait_until 8 pill_is Pause
    was=$(field shuffle)
    tapnode content-desc Shuffle
    check "shuffle starts this page's queue and lights ($was -> True)" wait_for shuffle True 8
    check "and the pill stays Pause over the queue it started" wait_until 5 pill_is Pause
    tapnode content-desc Shuffle
    check "a second press turns shuffle off on this queue" wait_for shuffle False 8
    tapnode text Pause
    check "the pill pauses playback" wait_for playing False 8
    check "and reads Play again" wait_until 5 pill_is Play
    pos=$(field positionMs)
    tapnode text Play
    check "Play picks the queue up where it stopped (from $pos ms)" wait_for positionMs ">$((pos + 500))" 10
  fi
fi

if want bridge; then section "the offline bridge"
  # A long library album, started while online and paused at once. Offline, a skip past what was fetched
  # ahead cannot play; with the bridge on, downloads play instead, and the album comes back with the
  # network. Which song is parked and where the album comes back is the core's (bridge.rs, and
  # a_bridge_is_started_and_undone_over_the_core_queue); here the network really going and coming.
  # Something has to be downloaded to stand in: the smoke's download, or this one.
  if [ "$(field downloaded)" = 0 ]; then
    "$app" do "download song:$id" >/dev/null; wait_for downloaded ">0" 60 >/dev/null
  fi
  "$app" set bridgeOffline true >/dev/null
  # Nothing of the album may already be on the phone: a song in the stream cache or downloaded plays
  # offline, rightly, and then there is nothing to bridge. What the phone holds is read from its database.
  "$app" set clearStreamCache true >/dev/null
  dl=$(mktemp -d)
  for f in nori.db nori.db-wal nori.db-shm; do adb exec-out run-as "$pkg" cat files/$f > "$dl/$f" 2>/dev/null; done
  python3 -c "
import sqlite3
c=sqlite3.connect('$dl/nori.db')
print('\n'.join(r[0] for r in c.execute('select id from downloads')))" > "$dl/held" 2>/dev/null
  if [ "$NORI_E2E_SERVER" = local ]; then candidates=$(mix_album); else
    candidates=$(api getAlbumList2 "&type=random&size=100" | python3 -c "
import sys,json
for a in json.load(sys.stdin)['subsonic-response']['albumList2'].get('album',[]):
    if not a['id'].startswith('ext-') and a.get('songCount',0) >= 10: print(a['id'])"); fi
  bid=$(for a in $candidates; do api getAlbum "&id=$a" | python3 -c "
import sys,json
held=set(open('$dl/held').read().split())
s=json.load(sys.stdin)['subsonic-response']['album']['song']
own=all(not x['id'].startswith('ext-') and x.get('suffix')!='Remote' for x in s)
print('$a' if own and len(s)>=10 and not any(x['id'] in held for x in s) else '')"; done | grep . | head -1)
  rm -rf "$dl"
  if [ -n "$bid" ]; then
    "$app" play "album:$bid" >/dev/null; wait_for playing True 20 >/dev/null; "$app" do pause >/dev/null
    offline
    for _ in 1 2 3 4 5 6; do "$app" do next >/dev/null; done
    check "downloads stand in while the server is out of reach" wait_for bridging True 30
    check "and they play" sounds 20
    online
    check "the album comes back with the network" wait_for bridging False 30
    "$app" do pause >/dev/null
  else
    echo "     (no library album of ten songs with nothing of it downloaded)"
  fi
  "$app" set bridgeOffline false >/dev/null
fi

if want download-notification; then section "the download queue from its notification"
  # The notification's tap is this intent; the app is already running, so it arrives as a new intent.
  "$app" open home >/dev/null
  adb shell am start -a dev.nori.music.OPEN_DOWNLOADS -n "$pkg/dev.nori.music.app.MainActivity" >/dev/null 2>&1
  check "tapping the download notification opens the queue" wait_for route downloads 8
fi

if want downloads; then section "downloads run side by side through media3 and survive a force stop"
  # How the batch is counted, its speed and time left are transfers.rs; here media3 running them.
  # A library album of at least six songs with nothing of it downloaded yet (every run downloads one).
  held=$(adb shell "run-as $pkg sqlite3 files/nori.db \"select distinct json_extract(json,'\$.albumId') from items where kind=2 and id in (select id from downloads)\"" 2>/dev/null | tr -d '\r')
  if [ "$NORI_E2E_SERVER" = local ]; then
    # The generated albums: a few hundred to take from before any is used twice. Never the Long Album,
    # which the bridge needs with nothing of it downloaded.
    aids=$(api getAlbumList2 "&type=alphabeticalByName&size=500" | python3 -c "
import sys,json
for a in json.load(sys.stdin)['subsonic-response']['albumList2'].get('album',[]):
    if a.get('songCount',0) >= 3: print(a['id'])" | grep -vxF -e "${held:-none}" -e "$(mix_album)" | head -12 | tr '\n' ' ')
  else
    aids=$(api getAlbumList2 "&type=random&size=100" | python3 -c "
import sys,json
for a in json.load(sys.stdin)['subsonic-response']['albumList2'].get('album',[]):
    if not a['id'].startswith('ext-') and a.get('songCount',0) >= 6: print(a['id'])" | grep -vxF -e "${held:-none}" | while read -r a; do
      api getAlbum "&id=$a" | python3 -c "
import sys,json
s=json.load(sys.stdin)['subsonic-response']['album']['song']
print('$a' if all(not x['id'].startswith('ext-') and x.get('suffix')!='Remote' for x in s) else '')"; done | grep . | head -12 | tr '\n' ' ')
  fi
  if [ -n "$aids" ]; then
    before=$(field downloaded)
    # An album already downloaded has nothing left to fetch, so ask each candidate until one has work.
    active=0
    for a in $aids; do
      "$app" do "download album:$a" >/dev/null
      wait_for dlActive ">1" 5 2>/dev/null && { active=$WAITED; break; }
    done
    check "several songs download at once ($active)" test "${active:-0}" -ge 2
    # Under its own id: it used to share the playback notification's (1001) and replace it. 2001 while
    # songs are coming, 2002 once they are done (the local server can finish before this looks).
    notifs=$(adb shell dumpsys notification --noredact 2>/dev/null | grep -o "$pkg|[0-9]*" | sort -u | tr '\n' ' ')
    check "the download notification posts under its own id ($notifs)" bash -c "[[ '$notifs' == *'|2001'* || '$notifs' == *'|2002'* ]]"
    adb shell am force-stop "$pkg" >/dev/null 2>&1; app_up >/dev/null
    resumed() { local v; v=$(fields downloading dlActive | tr '\n' ' '); set -- $v; [ "${1:-1}" = 0 ] || [ "${2:-0}" -gt 0 ]; }
    check "after a force stop the queue picks up again" wait_until 15 resumed
    check "the interrupted album finishes (was $before downloaded)" wait_for downloading 0 180
  else
    echo "     no library-only album with work left found; skipped"
  fi
fi

if want foryou; then section "for you: favourites and mixes open as pages"
  # What a mix page shows, read from the screen: "<count>|<title>@<x>,<y>|..." for the song count in its
  # caption and every fully visible row title (rows sit below the Play pill and above the mini player).
  # Which songs a mix holds, and that it stays the same when opened again, is mixes.rs.
  page() {
    ui | python3 -c "
import sys,re
nodes=[(t,d,*map(int,b)) for t,d,b in ((m.group(1),m.group(2),re.findall(r'\d+',m.group(3))) for m in re.finditer(r'text=\"([^\"]*)\"[^>]*content-desc=\"([^\"]*)\"[^>]*bounds=\"([^\"]*)\"',sys.stdin.read()))]
count=next((int(m.group(1)) for t,*_ in nodes for m in [re.match(r'(\d+) songs? ',t)] if m),0)
bar=next((y1 for t,d,x1,y1,x2,y2 in nodes if d=='Now playing bar'),10**6)
play=next((y2 for t,d,x1,y1,x2,y2 in nodes if t=='Play'),0)
rows=[n for n in nodes if n[0] and n[3]>=play and n[5]<=bar and 150<n[2]<260]
titles=[n for i,n in enumerate(rows) if i%2==0]
print('|'.join([str(count)]+['%s@%d,%d'%(t,(x1+x2)//2,(y1+y2)//2) for t,d,x1,y1,x2,y2 in titles]))"
  }
  count_is() { [ "$(page | cut -d'|' -f1)" = "$1" ]; }
  starred_count() { api getStarred2 | python3 -c "
import sys,json
d=json.load(sys.stdin)['subsonic-response'].get('starred2',{})
print(sum(1 for s in d.get('song',[]) if not s.get('isExternal') and not s['id'].startswith(('ext-','pl-'))))"; }
  "$app" open mix/favourites >/dev/null
  check "the favourites tile opens its page" wait_for route "mix/{id}" 8
  server=$(starred_count)
  check "it lists the songs the server has starred (server $server)" wait_until 8 count_is "$server"
  "$app" do "star song:$id" >/dev/null
  check "starring a song adds it while the page is open" wait_until 10 count_is "$((server + 1))"
  "$app" do "star song:$id" >/dev/null   # put it back
  check "and unstarring takes it away again" wait_until 10 count_is "$server"
  if [ "$NORI_E2E_SERVER" = local ]; then
    # A fresh fixture profile needs album rows in its local index for Discover.
    "$app" open "album/$(mix_album)" >/dev/null
    check "the fixture album loads" wait_until 15 on_screen 'text="Long Track 01"'
  fi
  "$app" open mix/discover >/dev/null
  wait_until 8 bash -c "'$app' state | grep -q 'mix/{id}'"
  first=""
  page_ready() { first=$(page) || return 1; [ -n "$(echo "$first" | cut -d'|' -f4)" ]; }
  wait_until 15 page_ready
  # What you see is what plays: a tap on the third row starts the whole mix at that row.
  n=$(echo "$first" | cut -d'|' -f1); third=$(echo "$first" | cut -d'|' -f4)
  if [ -n "$third" ]; then
    xy=${third##*@}; adb shell input tap "${xy%,*}" "${xy#*,}"
    check "tapping a row plays that song (${third%@*})" wait_for title "${third%@*}" 10
    check "with the rest of the mix around it ($(field index) of $(field queue), page $n)" \
      test "$(field index)" = "2" -a "$(field queue)" = "$n"
    "$app" do pause >/dev/null
  else
    check "the mix has songs to play" false
  fi
fi

if want dac; then section "a USB DAC, faked"
  # A DAC cannot be plugged into an emulator, so the app is pointed at a mock one (ActionsViewModel,
  # "dac"). Which mode a DAC gets and why one cannot be fed is dac.rs; here what the platform does: offload
  # stands down when the device appears, and the track is opened in the DAC's own format.
  "$app" set autoMix false >/dev/null; "$app" set crossfadeSec 0 >/dev/null
  "$app" set crossfeedDb 0 >/dev/null; "$app" set offload true >/dev/null; "$app" set eq false >/dev/null
  "$app" do "dac off" >/dev/null
  "$app" play "$PLAIN" >/dev/null; sounds 20
  check "offload is asked for on the phone's own output" wait_for offloadWanted True 10
  "$app" do "dac Mock DAC@44100/16,96000/24" >/dev/null
  check "offload stands down when a USB device appears" wait_for offloadWanted False 10
  check "the DAC is seen" wait_for dac "Mock DAC" 5
  check "audio keeps flowing to the DAC" bursts_continue
  "$app" set bitPerfect true >/dev/null; "$app" play "$PLAIN" >/dev/null; sounds 20
  check "bit-perfect engages on a mode the sink can write" wait_for bitPerfect True 10
  check "and says what the track was opened with" test -n "$(field dacTrack)"
  "$app" do "dac off" >/dev/null; "$app" set bitPerfect false >/dev/null
fi

if want device-sound; then section "a sound per output device"
  # The service switches the sound when the output changes, with no screen involved. A fake DAC stands in
  # for the device; "eq" in the state is the equalizer switch, which each device's sound sets. What a
  # device gets (flat, a profile, AutoEQ offered, applied or undone) is profiles.rs and device.rs; here the
  # platform's output events reaching it, both ways.
  dev="USB: Nori Check DAC"
  clean() {
    "$app" do "dac off" >/dev/null; "$app" set autoEqAuto false >/dev/null
    "$app" set deleteProfile "nori check" >/dev/null; "$app" set forgetDevice "$dev" >/dev/null
  }
  clean   # a run that stopped half-way must not decide this one
  "$app" set eq false >/dev/null
  "$app" play "$PLAIN" >/dev/null; sounds 20
  "$app" set eq true >/dev/null; "$app" set saveProfile "nori check" >/dev/null; "$app" set eq false >/dev/null
  "$app" set deviceSound "$dev=profile:nori check" >/dev/null
  "$app" do "dac Nori Check DAC@44100/16" >/dev/null
  on_dev() { [ "$(fields output eq | tr '\n' '/')" = "$dev/True/" ]; }
  check "a device with a profile gets it on connect" wait_until 10 on_dev
  "$app" do "dac off" >/dev/null
  check "and the sound from before comes back without it" wait_for eq False 10
  clean   # nothing of the check stays in the device list or the profiles
fi

remote_checks() {
  section "playing on another device: the terminal client on this Mac"
  # Who plays, the queue, the place, the volume and a transfer's order are the core's (crates/core
  # tests/remote.rs). Here: the media session handed to the other device (a remote volume the keys move),
  # this phone's own output let go while it plays there, and the music coming back.
  if [ "$NORI_E2E_SERVER" != local ]; then
    echo "  NOTE  needs the local server: the terminal client signs in to it as the other device"
  else
    peer="nori-e2e-peer-$ANDROID_SERIAL"; data="$here/../build/e2e/$ANDROID_SERIAL/peer"; cli="$here/../target/debug/nori-cli"
    python3 "$here/with-resource.py" build cargo build -j4 -p nori-cli >/dev/null || return 1
    tmux kill-session -t "$peer" 2>/dev/null; mkdir -p "$data"
    # Seed the peer's setting before startup; no screen navigation or stale selection.
    python3 - "$data/nori.db" <<'PY'
import sqlite3, sys
with sqlite3.connect(sys.argv[1]) as db:
    db.execute("CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID")
    db.execute("INSERT OR REPLACE INTO settings VALUES('remoteControl', '{\"b\":true}')")
PY
    tmux new-session -d -s "$peer" -x 160 -y 45 "$cli --data $data --url http://localhost:4533 --user $USER --password $PASS --no-images --no-mpris"
    screen() { tmux capture-pane -p -t "$peer"; }
    keys() { tmux send-keys -t "$peer" -- "$@"; }
    check "the CLI peer starts" wait_until 15 bash -c "tmux capture-pane -p -t $peer | grep -q 'localhost:4533'" || return 1
    for _ in $(seq 16); do keys -; done   # quiet on the Mac's speakers
    # Its door as mDNS has it; the emulator hears no multicast from here, so the app is handed it.
    resolved=$(python3 "$here/mdns-door.py" "$data/nori.db") || { check "the CLI peer announces its door" false; return 1; }
    name=$(printf '%s' "$resolved" | python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])')
    door=$(printf '%s' "$resolved" | python3 -c 'import json,sys; print(json.load(sys.stdin)["door"])')
    echo "     the Mac's door: $door"
    "$app" set remoteControl true >/dev/null
    "$app" play "$PLAIN" >/dev/null; sounds 20
    "$app" open player >/dev/null; "$app" remote watch on >/dev/null
    "$app" remote found "$door" >/dev/null
    check "the Mac is listed" wait_until 15 bash -c "'$app' remote devices | grep -Fq '$name'" || return 1
    "$app" remote watch off >/dev/null
    "$app" remote pick "$name" >/dev/null
    check "the player shows the Mac playing" wait_for playingOn "$name" 15 || return 1
    remote_session() { adb shell dumpsys media_session | grep -q "volumeType=REMOTE"; }
    check "the media session is the Mac's (a remote volume)" wait_until 10 remote_session
    check "this phone's output is let go" silent 10
    peer_volume() { screen | grep -oE '[0-9]+%' | tail -1 | tr -d '%'; }
    before=$(peer_volume)
    adb shell input keyevent KEYCODE_VOLUME_UP; adb shell input keyevent KEYCODE_VOLUME_UP
    louder() { [ "$(peer_volume)" -gt "$before" ]; }
    check "the volume keys turn the Mac up ($before%)" wait_until 10 louder
    "$app" remote pick here >/dev/null
    check "\"This phone\" brings it back" wait_for playingOn "" 15
    check "and it sounds here" sounds 15
    check "the Mac paused" wait_until 10 bash -c "tmux capture-pane -p -t $peer | grep -q '▶'"
    "$app" set remoteControl false >/dev/null
    tmux kill-session -t "$peer"
  fi
}
if want remote; then
  failures_before=$fail
  remote_checks || { [ "$fail" -gt "$failures_before" ] || check "remote setup succeeds" false; }
  "$app" remote pick here >/dev/null
  "$app" set remoteControl false >/dev/null
  [ -z "${peer:-}" ] || tmux kill-session -t "$peer" 2>/dev/null
fi

jam_checks() {
  section "a jam hosted here: started from the devices sheet, guests ask, the host decides by tapping"
  # The jam's roles, requests, who added what, and that a provider's song is not looked up before it is
  # accepted are the core's (crates/core tests/remote.rs). Here: the jam in the player, its queue and the
  # devices sheet, the invite's link, requests arriving live and decided by tapping, and the accepted song
  # playing, against octo-fiesta's real relay (NORI_E2E_JAM, the local one on 5274) with two guests on this
  # Mac (tools/jam-guest.py).
  jam_server=${NORI_E2E_JAM:-http://localhost:5274}
  if [ "$NORI_E2E_SERVER" != local ] || ! curl -sf "$jam_server/rest/noriRemote.poll?u=admin&p=admin&v=1.16.1&c=e2e&f=json&dev=e2e-probe" | grep -q seq; then
    if [ "$NORI_E2E_SERVER" = local ]; then
      check "octo-fiesta relay is available at $jam_server" false
      return 1
    fi
    echo "  NOTE  needs the local server, and octo-fiesta with the relay in front of it at $jam_server"
  else
    check "the relay serves its invite page" bash -c "curl -fsS '$jam_server/nori/jam' | grep -q 'Open in nori'" || return 1
    app_jam=$(echo "$jam_server" | sed 's#localhost#10.0.2.2#')
    start_jam() {
      tapnode text "Start a Jam" || return 1
      wait_until 10 off_screen 'content-desc="Close sheet"'
    }
    lib_song=$(song_id "Far Song Two"); first=$(song_id "Long Track 04")
    # A provider's song for Dee to ask for: only its id and words travel, nothing streams it.
    provider='{"id":"ext-e2e-refused","title":"Refused provider request","artist":"Nori E2E","duration":170,"isExternal":true}'
    "$app" login "$app_jam|admin|admin" >/dev/null; wait_for server "$app_jam" 30 >/dev/null
    "$app" set jam true >/dev/null
    "$app" play "song:$first" >/dev/null; sounds 20
    "$app" open devices >/dev/null
    check "the devices sheet offers to start a jam" wait_until 10 on_screen 'text="Start a Jam"' || return 1
    check "starting the jam closes the devices sheet" start_jam || return 1
    check "the player opens on the queue" wait_until 10 on_screen 'text="Long Track 04"' || return 1
    "$app" open devices >/dev/null
    check "the devices sheet shows the hosted jam" wait_until 10 on_screen 'text="Your Jam"' || return 1
    check "nobody listens yet" on_screen 'text="Jam · no one yet"'
    check "the devices sheet offers the invite" wait_until 10 on_screen 'text="Invite"'
    tapnode text Invite
    check "Invite shows the code and the link" wait_until 10 on_screen 'text="Copy link"'
    link=$(ui | grep -oE 'text="https?://[^"]*/nori/jam#[^"]*"' | head -1 | cut -d'"' -f2 | sed 's/&amp;/\&/g')
    echo "     the invite: $link"
    adb shell input keyevent KEYCODE_BACK
    adb shell input keyevent KEYCODE_BACK
    if on_screen 'content-desc="Close sheet"'; then tapnode content-desc "Close sheet"; fi
    # Its own invite opened here is refused: this phone stays the host, on its own profile.
    check "the invite reaches nori" open_invite "$link" app || return 1
    check "its own invite is refused" wait_until 10 on_screen 'text="That’s your own Jam"' || return 1
    check "and it still hosts" bash -c "'$app' remote view | grep -q 'hosting=true'"
    check "on its own profile" wait_for server "$app_jam" 5
    "$app" open queue >/dev/null
    guests="$here/../build/e2e/$ANDROID_SERIAL/jam"; mkdir -p "$guests"
    python3 "$here/jam-guest.py" "$link" Gus "{\"id\":\"$lib_song\",\"title\":\"Far Song Two\",\"artist\":\"Nori E2E Two\",\"duration\":170}" > "$guests/gus.log" 2>&1 & gus=$!
    check "Gus's request comes in by itself" wait_until 20 on_screen 'text="Asked by Gus"' || return 1
    python3 "$here/jam-guest.py" "$link" Dee "$provider" > "$guests/dee.log" 2>&1 & dee=$!
    check "and Dee's under it" wait_until 20 on_screen 'text="Asked by Dee"' || return 1
    check "the provider's song says accepting downloads it" on_screen 'text="Downloaded to your server if accepted"'
    "$app" open devices >/dev/null
    check "the devices sheet counts both" on_screen 'text="Jam · 2 listening"'
    adb shell input keyevent KEYCODE_BACK
    check "Dee's Refuse button can be tapped" tapnode content-desc Refuse 2 || return 1
    check "Refuse takes Dee's request away" wait_until 10 off_screen 'text="Asked by Dee"'
    tapnode content-desc Accept
    queued() { [[ "$(field upNext)" == *"$lib_song"* ]]; }
    check "Accept queues Gus's song" wait_until 15 queued
    check "nothing of the provider's was queued" bash -c "! '$app' state | grep -q 'ext-'"
    check "its row says Gus added it" wait_until 10 on_screen 'content-desc="Added by Gus"'
    check "Gus sees it in the host's queue, by him" wait_until 15 grep -q "Far Song Two by Gus" "$guests/gus.log"
    tapnode text "Far Song Two"
    check "and it plays" wait_for title "Far Song Two" 15
    "$app" open devices >/dev/null
    tapnode text End
    adb shell input keyevent KEYCODE_BACK
    check "End Jam ends it for the guests" wait_until 15 grep -q "the jam is over" "$guests/gus.log"
    check "and here" wait_until 10 bash -c "'$app' remote view | grep -q 'no jam'"
    kill $gus $dee 2>/dev/null
    # Its own invite to the jam it ended changes nothing, and a new one starts at once.
    check "the ended invite reaches nori" open_invite "$link" app || return 1
    check "its ended jam's invite says so" wait_until 10 on_screen 'text="This Jam has ended"'
    check "on its own profile, in no jam" bash -c "'$app' remote view | grep -q 'no jam'"
    check "no jam under the song" off_screen 'text="Jam · '
    "$app" open devices >/dev/null
    check "the devices sheet offers a jam again" wait_until 10 on_screen 'text="Start a Jam"'
    check "restarting the jam closes the devices sheet" start_jam || return 1
    "$app" open devices >/dev/null
    check "and it starts at once" wait_until 5 on_screen 'text="Your Jam"'
    tapnode text End
    adb shell input keyevent KEYCODE_BACK
    check "and ends" wait_until 10 bash -c "'$app' remote view | grep -q 'no jam'"
    "$app" set jam false >/dev/null
    "$app" login "$APP_URL|$USER|$PASS" >/dev/null; wait_for server "$APP_URL" 30 >/dev/null

    # This phone a guest of a jam on another server (the relay's), hosted on this Mac (tools/jam-host.py),
    # while its own profile is the home server's.
    echo "  -- a guest of a jam on another server"
    relay_song() { curl -s "$jam_server/rest/search3?u=admin&p=admin&v=1.16.1&c=e2e&f=json&songCount=1&albumCount=0&artistCount=0&query=$1" | python3 -c "
import sys,json
s=json.load(sys.stdin)['subsonic-response']['searchResult3']['song'][0]
print(json.dumps({k:s.get(k) for k in ['id','title','artist','album','albumId','coverArt','duration']}))"; }
    rm -f "$guests/host.log"
    python3 "$here/jam-host.py" "$jam_server" admin admin "$(relay_song 'Long%20Track%2004')" "$(relay_song 'Long%20Track%2005')" > "$guests/host.log" 2>&1 & host=$!
    wait_until 15 grep -qs invite: "$guests/host.log"
    link=$(grep invite: "$guests/host.log" | cut -d' ' -f2)
    # The invite is the relay's page: the browser opens it, and the page hands the invite to the app.
    check "the guest invite reaches nori" open_invite "$link" || return 1
    check "the invite opens the player on the host's jam" wait_until 20 on_screen 'text="Mac Host’s Jam"' || return 1
    check "playing what the host plays" wait_for title "Long Track 04" 15
    check "said under the song" on_screen 'text="Jam · Mac Host · 1 listening"'
    no_controls() { off_screen 'content-desc="Next"' && off_screen 'content-desc="Shuffle"' && off_screen 'content-desc="Remove"'; }
    check "with no controls of its own" no_controls
    "$app" open library >/dev/null
    guest_library() { on_screen 'text="Albums"' && on_screen 'text="Artists"' && on_screen 'text="Genres"' && off_screen 'text="Playlists"'; }
    check "its Library is the host's, without the account's playlists" wait_until 10 guest_library
    far=$(relay_song 'Far%20Song%20Two' | python3 -c 'import sys,json; print(json.load(sys.stdin)["albumId"])')
    "$app" open "album/$far" >/dev/null
    wait_until 15 on_screen 'text="Far Song Two"'
    tapnode text "Far Song Two"
    check "a tap asks the host, and the row says so" wait_until 10 on_screen 'text="Asked"'
    check "the host takes it" wait_until 20 grep -q "accepted: Far Song Two" "$guests/host.log"
    check "and the row lets go" wait_until 10 off_screen 'text="Asked"'
    "$app" open queue >/dev/null
    check "the host's queue says it was asked for here" wait_until 10 on_screen 'content-desc="Added by '
    tapnode text "Listen here"
    check "Listen here plays the host's music on this phone" sounds 30
    # Listening along is playback as any other: the service in the foreground, its notification the
    # jam's song with whose jam it is, on through the background and the screen off (the deep buffer's
    # bursts keep coming).
    jam_notified() { adb shell dumpsys notification --noredact | grep -A40 'pkg=dev.nori.music' | grep -q 'android.subText=String (Jam · Mac Host)'; }
    check "the notification says whose jam it is" wait_until 10 jam_notified
    in_foreground() { adb shell dumpsys activity services dev.nori.music | grep -q 'isForeground=true'; }
    adb shell input keyevent 3
    adb shell input keyevent 26
    check "in the background, screen off, the service stays in the foreground" in_foreground
    check "and the music goes on" bursts_continue
    check "and on" bursts_continue
    "$app" wake >/dev/null
    "$app" launch >/dev/null
    check "back in the app, the jam plays here" wait_for playing True 10
    # A plain guest's pause holds its own listening (the host plays on); play joins the jam where it is then.
    "$app" do pause >/dev/null
    check "a guest's pause is its own" silent 10
    check "the player says the jam plays on" wait_until 10 on_screen 'text="Paused here · Jam still playing"'
    "$app" do resume >/dev/null
    check "play joins the jam again" sounds 15
    check "the strip says the jam again" wait_until 10 off_screen 'text="Paused here · Jam still playing"'
    tapnode text Leave
    check "leaving returns to the home server" wait_for server "$APP_URL" 20
    check "and its music stops at once" silent 5
    check "the host saw it leave" wait_until 15 grep -q "left:" "$guests/host.log"
    kill $host 2>/dev/null

    # An admin's controls are the host's: its pause pauses the jam (and so here), its play and skip too.
    echo "  -- an admin of a jam on another server"
    rm -f "$guests/admin-host.log"
    NORI_JAM_ADMINS=1 python3 "$here/jam-host.py" "$jam_server" admin admin "$(relay_song 'Long%20Track%2004')" "$(relay_song 'Long%20Track%2005')" > "$guests/admin-host.log" 2>&1 & host=$!
    wait_until 15 grep -qs invite: "$guests/admin-host.log"
    link=$(grep invite: "$guests/admin-host.log" | cut -d' ' -f2)
    check "the admin invite reaches nori" open_invite "$link" || return 1
    check "the invite opens the player on the host's jam" wait_until 20 on_screen 'text="Listen here"'
    tapnode text "Listen here"
    check "Listen here plays the host's music" sounds 30
    check "an admin skips" wait_until 10 on_screen 'content-desc="Next"'
    "$app" do pause >/dev/null
    check "an admin's pause is the host's" wait_until 15 grep -q "obeyed: pause" "$guests/admin-host.log"
    check "and pauses here with it" silent 15
    "$app" do resume >/dev/null
    check "its play is the host's" wait_until 15 grep -q "obeyed: play" "$guests/admin-host.log"
    check "and plays here with it" sounds 15
    "$app" do next >/dev/null
    check "its skip is the host's" wait_until 15 grep -q "obeyed: next" "$guests/admin-host.log"
    check "and here it plays the host's next song" wait_for title "Long Track 05" 20
    tapnode text Leave
    check "leaving returns to the home server" wait_for server "$APP_URL" 20
    kill $host 2>/dev/null

    "$app" play "$LYRICS_SONG" >/dev/null
    check "a song played after leaving plays here" sounds 20
    check "it is the one picked" wait_for title "${LYRICS_SONG#search:}" 10
    "$app" do pause >/dev/null
    kill $host 2>/dev/null

    # A guest whose jam ends goes home by itself: at once when the host ends it, and on opening the app
    # when the relay dropped it meanwhile (its key no longer signs in).
    host_jam() { # host_jam <log>: a jam hosted on this Mac, joined from this phone
      rm -f "$1"
      python3 "$here/jam-host.py" "$jam_server" admin admin "$(relay_song 'Long%20Track%2004')" > "$1" 2>&1 & host=$!
      wait_until 15 grep -qs invite: "$1"
      check "the invite reaches nori" open_invite "$(grep invite: "$1" | cut -d' ' -f2)" || return 1
      check "a guest again" wait_until 20 bash -c "'$app' remote view | grep -q 'Mac Host:HOST'"
    }
    host_jam "$guests/host-ends.log" || return 1
    kill $host
    check "the host ending the jam takes the guest home" wait_for server "$APP_URL" 15
    check "and says who ended it" wait_until 5 on_screen 'text="Mac Host ended the Jam"'
    host_jam "$guests/host-gone.log" || return 1
    kill -9 $host; wait $host 2>/dev/null
    adb shell am force-stop "$pkg"
    curl -sf "$jam_server/rest/noriRemote.close?u=admin&p=admin&v=1.16.1&c=e2e&f=json&room=$(grep room: "$guests/host-gone.log" | cut -d' ' -f2)" >/dev/null
    "$app" wake >/dev/null
    "$app" launch >/dev/null
    wait_until 20 bash -c "'$app' state 2>/dev/null | grep -q '\"route\"'" || return 1
    check "a guest profile whose jam the relay dropped opens the user's own" wait_for server "$APP_URL" 20
  fi
}
if want jam; then
  failures_before=$fail
  jam_checks || { [ "$fail" -gt "$failures_before" ] || check "Jam setup succeeds" false; }
  for guest_pid in "${gus:-}" "${dee:-}" "${host:-}"; do
    [ -z "$guest_pid" ] || kill "$guest_pid" 2>/dev/null
  done
  "$app" set jam false >/dev/null
  "$app" login "$APP_URL|$USER|$PASS" >/dev/null
fi

restore_settings
feature_finished=1
finish
