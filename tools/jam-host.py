#!/usr/bin/env python3
"""A jam host for the device checks: opens a jam on a relay as an account's device that plays a few songs, as
the core's host does (nori-core remote.rs, nori-remote jam.rs; the frames are nori-remote wire.rs), prints the
invite, and takes guests' requests: each waits a few seconds, then goes into the queue as the guest's. The
invite names the server as the emulator reaches it (10.0.2.2 for this Mac). Prints one line per change it sees
until killed, and ends the jam then.

  tools/jam-host.py <server> <user> <password> <song json>...
  tools/jam-host.py http://localhost:5274 admin admin '{"id":"6OQa...","title":"Long Track 04","duration":95}'
"""
import json
import secrets
import signal
import sys
import time
import urllib.parse
import urllib.request

server, user, password, songs = sys.argv[1].rstrip("/"), sys.argv[2], sys.argv[3], [json.loads(s) for s in sys.argv[4:]]
dev, name = secrets.token_hex(8), "Mac Host"
ACCEPT_AFTER = 5


def call(endpoint, body=None, **params):
    q = urllib.parse.urlencode({"u": user, "p": password, "v": "1.16.1", "c": "nori", "f": "json", **params})
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(f"{server}/rest/{endpoint}?{q}", data=data, headers={"Content-Type": "application/json"} if data else {})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def entry(song, at, by=None):
    return {"index": at, "turn": at, "id": song["id"], "title": song.get("title", ""), "artist": song.get("artist", ""),
            "album": song.get("album", ""), "albumId": song.get("albumId"), "coverArt": song.get("coverArt"),
            "duration": song.get("duration", 0), "external": song["id"].startswith("ext-"), "by": by}


opened = call("noriRemote.open", dev=dev, name=name)
room = opened["room"]
link_server = server.replace("localhost", "10.0.2.2")
print("invite: nori://jam?s=" + urllib.parse.quote(link_server, safe="") + "&k=" + urllib.parse.quote(opened["invite"], safe=""), flush=True)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))

queue = [entry(s, k) for k, s in enumerate(songs)]
members, pending, seq, rev = [], [], 0, 1
next_request = 1


def publish():
    global seq
    seq += 1
    jam = {"members": [{"id": dev, "name": name, "role": "host"}] + [{"id": m["id"], "name": m["name"], "role": "guest"} for m in members],
           "pending": pending}
    state = {"seq": seq, "playing": True, "positionMs": 30_000, "index": 0, "rev": rev, "len": len(queue), "entries": queue, "jam": jam}
    call("noriRemote.send", {"room": room, "state": state}, dev=dev, name=name, kind="terminal")


try:
    publish()
    since = None
    while True:
        p = {"dev": dev, "name": name, "kind": "terminal", "serve": "0"}
        if since is not None:
            p.update(since=str(since), hold="1")
        # Short holds while a request waits, so it is accepted on time.
        answer = call("noriRemote.poll", **p) if not pending or since is None else call("noriRemote.poll", **{**p, "hold": "0"})
        since = answer["seq"]
        changed = False
        listed = [m for r in answer.get("rooms", []) if r["room"] == room for m in r["members"] if m["id"] != dev]
        if [m["id"] for m in listed] != [m["id"] for m in members]:
            for m in listed:
                if m["id"] not in [x["id"] for x in members]:
                    print(f"joined: {m['name']}", flush=True)
            for m in members:
                if m["id"] not in [x["id"] for x in listed]:
                    print(f"left: {m['name']}", flush=True)
            members, changed = listed, True
        for e in answer.get("events", []):
            body = e["body"]
            if e["room"] != room or body.get("t") != "command":
                continue
            op, who = body["op"], next((m["name"] for m in members if m["id"] == e["from"]), "?")
            if op.get("op") == "request":
                song = op["song"]
                pending.append({"request": next_request, "from": e["from"], "fromName": who, "song": entry(song, 0), "provider": song["id"].startswith("ext-"),
                                "at": time.time()})
                next_request += 1
                print(f"asked: {song.get('title', song['id'])} by {who}", flush=True)
                changed = True
            call("noriRemote.send", {"room": room, "to": e["from"], "body": {"t": "ack", "id": body["id"], "refusal": None}}, dev=dev, name=name, kind="terminal")
        for w in [w for w in pending if time.time() - w["at"] >= ACCEPT_AFTER]:
            pending.remove(w)
            queue.append({**w["song"], "index": len(queue), "turn": len(queue), "by": w["fromName"]})
            rev += 1
            print(f"accepted: {w['song']['title']} for {w['fromName']}", flush=True)
            changed = True
        if changed:
            publish()
        if pending:
            time.sleep(1)
finally:
    call("noriRemote.close", room=room)
    print("the jam is over", flush=True)
