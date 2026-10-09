#!/usr/bin/env python3
"""A jam guest for the device checks: joins the jam an invite link names and asks its host for songs, as
the core's guest does (nori-core remote.rs; the relay's frames are nori-remote wire.rs). The emulator's
address in the link (10.0.2.2) is read as this Mac's. Prints one line per change it sees until killed.

  tools/jam-guest.py <link> <name> [<song json>...]
  tools/jam-guest.py 'http://10.0.2.2:5274/nori/jam#s=...&k=...' Gus '{"id":"6OQa...","title":"Far Song Two","duration":170}'
"""
import json
import secrets
import sys
import urllib.parse
import urllib.request

link, name, songs = sys.argv[1], sys.argv[2], [json.loads(s) for s in sys.argv[3:]]
parts = urllib.parse.urlsplit(link)
# The invite page's link carries the invite in its fragment, the app's (nori://jam) in its query.
query = urllib.parse.parse_qs(parts.query if parts.scheme == "nori" else parts.fragment)
server = query["s"][0].replace("10.0.2.2", "localhost").rstrip("/")
dev = secrets.token_hex(8)


def call(endpoint, key, body=None, **params):
    q = urllib.parse.urlencode({"apiKey": "nori-jam-" + key, "v": "1.16.1", "c": "nori", "f": "json", **params})
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(f"{server}/rest/{endpoint}?{q}", data=data, headers={"Content-Type": "application/json"} if data else {})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


key = call("noriRemote.join", query["k"][0], name=name)["key"]
print(f"{name} joined", flush=True)


def poll(since=None):
    p = {"dev": dev, "name": name, "kind": "guest", "serve": "0"}
    if since is not None:
        p.update(since=str(since), hold="1")
    return call("noriRemote.poll", key, **p)


def host(answer):
    """The room and the member whose state carries the jam: the host."""
    for room in answer.get("rooms", []):
        for m in room.get("members", []):
            if (m.get("state") or {}).get("jam") is not None:
                return room["room"], m
    return None, None


answer = poll()
room, h = host(answer)
while h is None:
    answer = poll(answer["seq"])
    room, h = host(answer)
for n, song in enumerate(songs, 1):
    call("noriRemote.send", key, {"room": room, "to": h["id"], "body": {"t": "command", "id": n, "op": {"op": "request", "song": song}}},
         dev=dev, name=name, kind="guest")
    print(f"{name} asked for {song.get('title', song['id'])}", flush=True)
said = None
while True:
    answer = poll(answer["seq"])
    room, h = host(answer)
    if h is None:
        print(f"{name}: the jam is over", flush=True)
        break
    state = h["state"]
    now = " | ".join(f"{e.get('title')}{' by ' + e['by'] if e.get('by') else ''}" for e in state.get("entries", []))
    line = f"{name} sees: {now}; waiting {len(state['jam'].get('pending', []))}"
    if line != said:
        print(line, flush=True)
        said = line
