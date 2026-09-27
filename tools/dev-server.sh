#!/usr/bin/env bash
# Local Navidrome with generated, tagged test music. Emulator reaches it at http://10.0.2.2:4533
# First login (admin/admin) is created automatically through the API.
#
# One server for the checkout and every worktree beside it: the music lives in the main checkout's .dev/,
# the database in the volume nori-dev-data, and a Navidrome already answering on 4533 (whatever its
# container is called) is used as it is. Docker or Podman, whichever is installed.
#
# Besides the first generated albums it seeds what the e2e checks need (tools/e2e-lib.sh, local mode):
#   Nori E2E / Long Album       twelve songs of 75-130 s, every third one FLAC (the bridge, album pages)
#   Nori E2E Two / Far Side     three songs of 150-190 s off another album (crossfades are not gapless
#                               album joins), "Far Song One" the one transcoded to Opus in the checks
#   Nori Bench 1000 (playlist)  a thousand songs, for opening a long page (tools/open-bench.sh)
set -euo pipefail
common=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)
if [ -n "$common" ] && [ "$(basename "$common")" = .git ]; then top=$(dirname "$common"); else top="$(cd "$(dirname "$0")/.." && pwd)"; fi
root="${NORI_DEV_ROOT:-$top/.dev}"
music="$root/music"
mkdir -p "$music"
engine=$(command -v docker || command -v podman || true)
if [ -z "$(ls -A "$music")" ]; then
  n=0
  for artist in "Alpha Waves" "Beta Band" "Gamma Ray Trio"; do
    for album in "First Light" "Second Wind"; do
      d="$music/$artist/$album"; mkdir -p "$d"
      ffmpeg -loglevel error -f lavfi -i "color=c=0x$(printf '%02x%02x%02x' $((n*40%256)) $((n*90%256)) $((n*150%256))):s=600x600" -frames:v 1 "$d/cover.jpg"
      for t in 1 2 3 4; do
        n=$((n+1)); ext=mp3; [ $((n%3)) = 0 ] && ext=flac
        ffmpeg -loglevel error -f lavfi -i "sine=frequency=$((200+n*25)):duration=$((40+n*3))" \
          -metadata title="Track $t of $album" -metadata artist="$artist" -metadata album_artist="$artist" \
          -metadata album="$album" -metadata track="$t" -metadata date="$((2000+n))" -metadata genre="Test" \
          "$d/0$t - Track $t.$ext"
      done
    done
  done
fi

# Something like music for the e2e checks: a chord that moves, a beat, a little noise - so AutoMix has
# something to measure, and a transcode is not all silence.
song() { # song <file> <title> <artist> <album> <track> <seconds> <seed>
  local f="$1" s="$7"
  [ -s "$f" ] && return 0
  ffmpeg -loglevel error -f lavfi -i "sine=frequency=$((110+s*7)):duration=$6" -f lavfi -i "sine=frequency=$((330+s*11)):duration=$6" \
    -f lavfi -i "anoisesrc=d=$6:c=pink:a=0.05" \
    -filter_complex "[0][1][2]amix=inputs=3,volume=2,apulsator=hz=2:amount=0.6[a]" -map "[a]" -ac 2 -ar 44100 \
    -metadata title="$2" -metadata artist="$3" -metadata album_artist="$3" -metadata album="$4" \
    -metadata track="$5" -metadata date=2024 -metadata genre="Test" "$f"
}
seeded=0
d="$music/Nori E2E/Long Album"
if [ ! -f "$d/.done" ]; then
  mkdir -p "$d"
  ffmpeg -loglevel error -y -f lavfi -i "color=c=0x406080:s=600x600" -frames:v 1 "$d/cover.jpg"
  for t in $(seq 1 12); do
    ext=mp3; [ $((t%3)) = 0 ] && ext=flac
    song "$d/$(printf %02d "$t") - Long Track $(printf %02d "$t").$ext" "Long Track $(printf %02d "$t")" "Nori E2E" "Long Album" "$t" $((75 + t*5)) "$t"
  done
  touch "$d/.done"; seeded=1
fi
d="$music/Nori E2E Two/Far Side"
if [ ! -f "$d/.done" ]; then
  mkdir -p "$d"
  ffmpeg -loglevel error -y -f lavfi -i "color=c=0x806040:s=600x600" -frames:v 1 "$d/cover.jpg"
  song "$d/01 - Far Song One.mp3" "Far Song One" "Nori E2E Two" "Far Side" 1 190 21
  song "$d/02 - Far Song Two.mp3" "Far Song Two" "Nori E2E Two" "Far Side" 2 170 22
  song "$d/03 - Far Song Three.flac" "Far Song Three" "Nori E2E Two" "Far Side" 3 150 23
  touch "$d/.done"; seeded=1
fi

if curl -sf -m 2 localhost:4533/ping >/dev/null; then
  echo "navidrome: already running on 4533"
else
  [ -n "$engine" ] || { echo "neither docker nor podman is installed" >&2; exit 1; }
  "$engine" rm -f nori-navidrome >/dev/null 2>&1 || true
  # The database in a volume of its own, not a folder shared from the host: SQLite on Docker Desktop's file
  # sharing (a Mac) dies of a bus error mid-scan and leaves the database corrupt (tools/bgtest does the same).
  # The volume is written as the image's own user.
  "$engine" run -d --name nori-navidrome -p 4533:4533 \
    -e ND_SCANNER_SCHEDULE=@every\ 1m -e ND_LOGLEVEL=info \
    -v "$music:/music:ro" -v nori-dev-data:/data docker.io/deluan/navidrome:latest >/dev/null
  for _ in $(seq 30); do curl -sf localhost:4533/ping >/dev/null && break; sleep 1; done
  seeded=1
fi
curl -sf -X POST localhost:4533/auth/createAdmin -H 'content-type: application/json' \
  -d '{"username":"admin","password":"admin"}' >/dev/null || true
# New songs are scanned now rather than at the next minute's scan, and waited for.
a="u=admin&p=admin&v=1.16.1&c=dev&f=json"
if [ "$seeded" = 1 ] || ! curl -s "localhost:4533/rest/search3?$a&query=Long%20Track%2012&songCount=1" | grep -q '"song"'; then
  curl -s "localhost:4533/rest/startScan?$a" >/dev/null || true
  for _ in $(seq 120); do
    curl -s "localhost:4533/rest/search3?$a&query=Far%20Song%20Three&songCount=1" | grep -q '"song"' &&
      ! curl -s "localhost:4533/rest/getScanStatus?$a" | grep -q '"scanning":true' && break
    sleep 1
  done
fi
# A playlist of a thousand songs, for opening and scrolling a long page (tools/open-bench.sh): every song
# the server has, in turn, over again when there are fewer than a thousand. Made once.
big="Nori Bench 1000"
if ! curl -s "localhost:4533/rest/getPlaylists?$a" | grep -q "\"name\":\"$big\""; then
  ids=$(curl -s "localhost:4533/rest/search3?$a&query=&songCount=1000&artistCount=0&albumCount=0" |
    python3 -c 'import sys, json
songs = json.load(sys.stdin)["subsonic-response"].get("searchResult3", {}).get("song", [])
print("&".join("songId=" + songs[i % len(songs)]["id"] for i in range(1000)) if songs else "")')
  [ -n "$ids" ] && curl -s -X POST "localhost:4533/rest/createPlaylist" --data "$a&name=${big// /%20}&$ids" >/dev/null &&
    echo "navidrome: made the playlist \"$big\""
fi
echo "navidrome: http://localhost:4533  (admin/admin)"
