#!/usr/bin/env python3
"""A local Navidrome for the battery tests, with real songs: nothing is asked of the real server while
measuring, and every run plays the same files.

    tools/bgtest/server.py            download (once), build the formats, start, scan, make playlists
    tools/bgtest/server.py --stop

Songs come once from the server in ~/.music.pass (Subsonic `download`: the original file): the first
songs of --source-playlist plus every 48 kHz song of --extra-artist. From them it makes FLAC copies, so
the players have lossless files and files they must resample:
    bg-mp3        the MP3s as they are (320 kbps, 44.1 kHz)
    bg-flac-44    FLAC 16-bit 44.1 kHz
    bg-flac-48    FLAC 24-bit 48 kHz
    bg-flac-96    FLAC 24-bit 96 kHz
    bg-48k        the original 48 kHz MP3s
    bg-mixed      a bit of everything, in turn
    bg-quick      3 short fillers (for the harness's skips), then one-minute clips, a different song in
                  each format in turn (MP3, MP3 48k, FLAC 44k, FLAC 48k, FLAC 96k), twice: a 5-min
                  run hears every format, with a song change every minute
Each copy is its own album ("<album> [FLAC 48k]") so no two songs look the same to a player.

It runs as its own container (nori-bgtest-navidrome, port 4540, admin/admin), beside the e2e checks'
Navidrome on 4533, with its music and database in build/bgtest/server/. The phone reaches it at this
computer's address on the local network, which it prints (and writes to build/bgtest/server/url).
"""
import argparse
import hashlib
import json
import os
import re
import secrets
import socket
import subprocess
import sys
import time
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.path.join(ROOT, "build", "bgtest", "server")
ORIG = os.path.join(BASE, "originals")
MUSIC = os.path.join(BASE, "music")
DATA = os.path.join(BASE, "data")
NAME = "nori-bgtest-navidrome"
# Pinned: 0.64.2 crashes (a nil pointer in its playlist covers) right after the playlists are made.
IMAGE = "docker.io/deluan/navidrome:0.64.0"
PORT = 4540
LOCAL = ("admin", "admin")


def lan_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 9))  # UDP: nothing is sent, the route picks the address
        return s.getsockname()[0]
    finally:
        s.close()


def api(base, user, password, method, raw=False, post=None, **kw):
    salt = secrets.token_hex(6)
    q = dict(u=user, t=hashlib.md5((password + salt).encode()).hexdigest(), s=salt, v="1.16.1", c="bgtest", **kw)
    if not raw:
        q["f"] = "json"
    url = f"{base}/rest/{method}?{urllib.parse.urlencode(q, doseq=True)}"
    req = urllib.request.Request(url, headers={"User-Agent": "bgtest/1.0"}, data=post)
    with urllib.request.urlopen(req, timeout=300) as r:
        return r.read() if raw else json.load(r)["subsonic-response"]


def real_server():
    l = open(os.path.expanduser("~/.music.pass")).read().split("\n")
    return l[0].strip(), l[2].strip(), l[3].strip()


def safe(s):
    return re.sub(r'[\\/:*?"<>|]+', "_", s).strip()[:80]


def fetch(n_songs, playlist, extra_artist):
    """The originals, downloaded once (a song already there is not asked for again)."""
    base, user, pw = real_server()
    pls = api(base, user, pw, "getPlaylists")["playlists"]["playlist"]
    pl = next(p for p in pls if p["name"] == playlist)
    songs = api(base, user, pw, "getPlaylist", id=pl["id"])["playlist"]["entry"]
    picked = [s for s in songs if s.get("samplingRate") == 44100][:n_songs]
    extra = api(base, user, pw, "search3", query=extra_artist, songCount=50, albumCount=0, artistCount=0)["searchResult3"].get("song", [])
    picked += [s for s in extra + songs if s.get("samplingRate") == 48000 and s.get("suffix") != "Remote"]
    seen, out = set(), []
    os.makedirs(ORIG, exist_ok=True)
    for s in picked:
        if s["id"] in seen:
            continue
        seen.add(s["id"])
        f = os.path.join(ORIG, f"{safe(s.get('artist', '?'))} - {safe(s['title'])}.{s['suffix']}")
        if not os.path.exists(f):
            print(f"  downloading {os.path.basename(f)} ({s.get('size', 0) / 1e6:.1f} MB)", flush=True)
            data = api(base, user, pw, "download", raw=True, id=s["id"])
            open(f + ".part", "wb").write(data)
            os.rename(f + ".part", f)
        out.append((f, s))
    return out


def tags(s, album_suffix):
    t = {"title": s["title"], "artist": s.get("artist", ""), "album_artist": s.get("artist", ""),
         "album": f"{s.get('album', '')} [{album_suffix}]", "track": str(s.get("track", 1)), "date": str(s.get("year", ""))}
    out = []
    for k, v in t.items():
        out += ["-metadata", f"{k}={v}"]
    return out


def build(originals):
    """The copies in each format, made once."""
    groups = {}
    for f, s in originals:
        rate = s.get("samplingRate")
        variants = []
        if rate == 44100:
            variants = [("bg-mp3", "MP3", "mp3", []),
                        ("bg-flac-44", "FLAC 44k", "flac", ["-c:a", "flac", "-sample_fmt", "s16", "-ar", "44100"]),
                        ("bg-flac-48", "FLAC 48k", "flac", ["-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24", "-ar", "48000"]),
                        ("bg-flac-96", "FLAC 96k", "flac", ["-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24", "-ar", "96000"])]
        elif rate == 48000:
            variants = [("bg-48k", "MP3 48k", "mp3", [])]
        for group, suffix, ext, codec in variants:
            d = os.path.join(MUSIC, group)
            os.makedirs(d, exist_ok=True)
            out = os.path.join(d, os.path.splitext(os.path.basename(f))[0] + "." + ext)
            if not os.path.exists(out):
                cmd = ["ffmpeg", "-loglevel", "error", "-y", "-i", f, "-map", "0:a", "-map_metadata", "-1"] + tags(s, suffix)
                cmd += (["-c:a", "copy"] if ext == "mp3" and not codec else codec) + [out]
                subprocess.run(cmd, check=True)
            groups.setdefault(group, []).append(os.path.basename(out))
    return groups


QUICK = "bg-quick"
# Order of the formats in bg-quick, and where each comes from (a folder of build(), or the 48 kHz MP3s).
QUICK_FORMATS = [("bg-mp3", "MP3", "mp3"), ("bg-48k", "MP3 48k", "mp3"), ("bg-flac-44", "FLAC 44k", "flac"),
                 ("bg-flac-48", "FLAC 48k", "flac"), ("bg-flac-96", "FLAC 96k", "flac")]


def build_quick(clip_s=60, rounds=2, fillers=3):
    """bg-quick: `fillers` short MP3s (taken by the harness's skips), then `rounds` times one clip of
    `clip_s` seconds in each format, a different song for each, so a short run passes every format and
    changes song every clip. Clips are cut from a minute into the song, the formats as they are."""
    d = os.path.join(MUSIC, QUICK)
    os.makedirs(d, exist_ok=True)
    files = {g: sorted(os.listdir(os.path.join(MUSIC, g))) for g, _, _ in QUICK_FORMATS}
    order = []
    for i in range(fillers):
        src = os.path.join(MUSIC, "bg-mp3", files["bg-mp3"][-1 - i])
        order.append((src, f"00{i + 1} filler", "Filler", "mp3", ["-t", "20"], i + 1))
    n = fillers
    for r in range(rounds):
        for k, (g, label, ext) in enumerate(QUICK_FORMATS):
            pool = files[g]
            src = os.path.join(MUSIC, g, pool[(r * len(QUICK_FORMATS) + k) % len(pool)])
            n += 1
            order.append((src, f"{n:03d} {label}", label, ext, ["-ss", "60", "-t", str(clip_s)], n))
    names = []
    for src, name, label, ext, cut, track in order:
        title = os.path.splitext(os.path.basename(src))[0]
        out = os.path.join(d, f"{name} - {title}.{ext}")
        names.append(os.path.basename(out))
        if os.path.exists(out):
            continue
        # A FLAC cut from a FLAC keeps its source's odd last block size unless the frame size is given.
        codec = ["-c:a", "copy"] if ext == "mp3" else ["-c:a", "flac", "-frame_size", "4608"]
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", *cut[:2], "-i", src, *cut[2:], "-map", "0:a",
                        "-map_metadata", "-1", "-metadata", f"title={title} [{label}]", "-metadata", "artist=bgtest",
                        "-metadata", "album_artist=bgtest", "-metadata", f"album=bg-quick {track:03d}",
                        "-metadata", f"track={track}", *codec, out], check=True)
    return names


def docker(*a, check=True):
    exe = docker_exe()
    # Docker Desktop's helpers (docker-credential-desktop…) sit beside it, and are looked up on the PATH.
    env = dict(os.environ, PATH=os.path.dirname(exe) + os.pathsep + os.environ.get("PATH", ""))
    r = subprocess.run([exe, *a], capture_output=True, text=True, env=env)
    if check and r.returncode != 0:
        raise RuntimeError(f"docker {a[0]}: {r.stderr.strip()}")
    return r.stdout


def docker_exe():
    """Docker, also where a shell without the user's PATH (ssh, cron) would not look: Docker Desktop puts
    it in /usr/local/bin on a Mac."""
    import shutil
    for exe in (shutil.which("docker"), "/usr/local/bin/docker", "/opt/homebrew/bin/docker",
                "/Applications/Docker.app/Contents/Resources/bin/docker"):
        if exe and os.path.exists(exe):
            return exe
    sys.exit("docker is needed for the test server (Docker Desktop on a Mac): not found")


def start():
    os.makedirs(DATA, exist_ok=True)
    if docker("ps", "-q", "-f", f"name=^{NAME}$").strip():
        print(f"  {NAME} already running")
    else:
        docker("rm", "-f", NAME, check=False)
        docker("run", "-d", "--name", NAME, "--user", f"{os.getuid()}:{os.getgid()}", "-p", f"{PORT}:4533",
               "-e", "ND_SCANNER_SCHEDULE=0", "-e", "ND_LOGLEVEL=warn", "-e", "ND_ENABLETRANSCODINGCONFIG=false",
               "-v", f"{MUSIC}:/music:ro", "-v", f"{DATA}:/data", IMAGE)
    base = f"http://localhost:{PORT}"
    for _ in range(60):
        try:
            urllib.request.urlopen(f"{base}/ping", timeout=2)
            break
        except Exception:
            time.sleep(1)
    try:
        req = urllib.request.Request(f"{base}/auth/createAdmin", data=json.dumps({"username": LOCAL[0], "password": LOCAL[1]}).encode(),
                                     headers={"content-type": "application/json"})
        urllib.request.urlopen(req, timeout=10)
    except Exception:
        pass  # made before
    return base


def scan(base, expected):
    api(base, *LOCAL, "startScan", fullScan="true")
    for _ in range(300):
        st = api(base, *LOCAL, "getScanStatus")["scanStatus"]
        if not st.get("scanning") and st.get("count", 0) >= expected:
            return st["count"]
        time.sleep(1)
    raise RuntimeError(f"scan did not finish: {st}")


# Each folder's copies carry their format in the album tag ("<album> [FLAC 48k]"): that is how a
# song is told apart on the server, which reports paths made from tags, not the files' folders.
SUFFIX = {"bg-mp3": "MP3", "bg-flac-44": "FLAC 44k", "bg-flac-48": "FLAC 48k", "bg-flac-96": "FLAC 96k", "bg-48k": "MP3 48k"}


def playlists(base, groups):
    songs = api(base, *LOCAL, "search3", query="", songCount=1000, albumCount=0, artistCount=0)["searchResult3"].get("song", [])
    songs.sort(key=lambda s: (s.get("artist", ""), s.get("title", "")))
    existing = {p["name"]: p["id"] for p in api(base, *LOCAL, "getPlaylists")["playlists"].get("playlist", [])}
    lists = {g: [s["id"] for s in songs if s.get("album", "").endswith(f"[{SUFFIX[g]}]")] for g in groups}
    # Mixed: one of each format in turn.
    mixed, i = [], 0
    while any(i < len(v) for v in lists.values()):
        for g in sorted(lists):
            if i < len(lists[g]):
                mixed.append(lists[g][i])
        i += 1
    lists["bg-mixed"] = mixed
    # bg-quick in its own order: its album tags number the clips.
    quick = sorted((x for x in songs if x.get("album", "").startswith("bg-quick ")), key=lambda x: x["album"])
    if quick:
        lists[QUICK] = [x["id"] for x in quick]
    for name, ids in lists.items():
        if name in existing:
            api(base, *LOCAL, "deletePlaylist", id=existing[name])
        api(base, *LOCAL, "createPlaylist", name=name, songId=ids)
        print(f"  playlist {name}: {len(ids)} songs")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--songs", type=int, default=20, help="44.1 kHz songs taken from the source playlist")
    p.add_argument("--source-playlist", default="RockMix")
    p.add_argument("--extra-artist", default="Maphra")
    p.add_argument("--stop", action="store_true")
    a = p.parse_args()
    if a.stop:
        docker("rm", "-f", NAME, check=False)
        print("test server stopped")
        return
    if os.path.exists(os.path.expanduser("~/.music.pass")):
        print("songs:")
        originals = fetch(a.songs, a.source_playlist, a.extra_artist)
        print("formats:")
        groups = build(originals)
        quick = build_quick()
    elif all(os.path.isdir(os.path.join(MUSIC, g)) for g in list(SUFFIX) + [QUICK]):
        # No real server here (no ~/.music.pass): the songs copied from a computer that built them
        # (rsync its build/bgtest/server/music/ here) are served as they are.
        print("songs: the ones already here (no ~/.music.pass to fetch more)")
        groups = {g: sorted(os.listdir(os.path.join(MUSIC, g))) for g in SUFFIX}
        quick = sorted(os.listdir(os.path.join(MUSIC, QUICK)))
    else:
        sys.exit(f"no songs in {MUSIC} and no ~/.music.pass to fetch them: copy build/bgtest/server/music/ from a "
                 "computer that has them")
    print(f"  {sum(len(v) for v in groups.values()) + len(quick)} files in {len(groups) + 1} folders ({QUICK}: {len(quick)} clips)")
    print("server:")
    base = start()
    n = scan(base, sum(len(v) for v in groups.values()) + len(quick))
    print(f"  scanned {n} songs")
    playlists(base, groups)
    url = f"http://{lan_ip()}:{PORT}"
    open(os.path.join(BASE, "url"), "w").write(url + "\n")
    print(f"ready: {url}  ({LOCAL[0]}/{LOCAL[1]})")


if __name__ == "__main__":
    sys.exit(main())
