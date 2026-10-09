#!/usr/bin/env python3
"""A jam host for the device checks: opens a jam on a relay as an account's device that plays a few songs, as
the core's host does (nori-core remote.rs, nori-remote jam.rs; the frames are nori-remote wire.rs), prints the
invite, lets its guests listen along (answering their time exchanges on this Mac's monotonic clock), and takes
guests' requests: each waits a few seconds, then goes into the queue as the guest's. Its place runs from 30 s
into the first song on through the queue. With NORI_JAM_ADMINS=1 every guest is an admin, whose play, pause,
seek and skips it carries out. The invite names the server as the emulator reaches it (10.0.2.2 for this
Mac). Prints one line per change it sees until killed, and ends the jam then.

  tools/jam-host.py <server> <user> <password> <song json>...
  tools/jam-host.py http://localhost:5274 admin admin '{"id":"6OQa...","title":"Long Track 04","duration":95}'
"""
import json
import os
import secrets
import signal
import sys
import time
import urllib.parse
import urllib.request

server, user, password, songs = sys.argv[1].rstrip("/"), sys.argv[2], sys.argv[3], [json.loads(s) for s in sys.argv[4:]]
dev, name = secrets.token_hex(8), "Mac Host"
ACCEPT_AFTER = 5
admins = os.environ.get("NORI_JAM_ADMINS") == "1"


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
print(f"room: {room}", flush=True)
print(f"invite: {link_server}/nori/jam#s=" + urllib.parse.quote(link_server, safe="") + "&k=" + urllib.parse.quote(opened["invite"], safe=""), flush=True)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))

queue = [entry(s, k) for k, s in enumerate(songs)]
members, pending, seq, rev = [], [], 0, 1
next_request = 1


def now_us():
    return time.monotonic_ns() // 1000


# The place: song `index` at `ms` at `at_us`, running on while playing.
place = {"index": 0, "ms": 30_000, "at_us": now_us(), "playing": True}


def place_now():
    """The place run on to now, into the next songs past each one's end."""
    p = dict(place)
    if p["playing"]:
        p["ms"] += (now_us() - p["at_us"]) // 1000
    p["at_us"] = now_us()
    while p["index"] + 1 < len(queue) and p["ms"] >= queue[p["index"]]["duration"] * 1000:
        p["ms"] -= queue[p["index"]]["duration"] * 1000
        p["index"] += 1
    # At the end of the queue the music stops.
    end = queue[p["index"]]["duration"] * 1000
    if p["playing"] and p["index"] + 1 == len(queue) and p["ms"] >= end:
        p["ms"], p["playing"] = end, False
    return p


def publish():
    global seq, place
    seq += 1
    place = place_now()
    role = "admin" if admins else "guest"
    jam = {"members": [{"id": dev, "name": name, "role": "host"}] + [{"id": m["id"], "name": m["name"], "role": role} for m in members],
           "pending": pending, "along": {"speed": 1.0, "pitch": 1.0}}
    state = {"seq": seq, "playing": place["playing"], "positionMs": place["ms"], "atUs": place["at_us"], "rate": 1.0, "index": place["index"],
             "rev": rev, "len": len(queue), "entries": queue, "jam": jam}
    call("noriRemote.send", {"room": room, "state": state}, dev=dev, name=name, kind="terminal")


def obey(op):
    """An admin's control, carried out as the core's host carries it out."""
    global place
    place = place_now()
    kind = op.get("op")
    if kind == "play":
        place["playing"] = True
    elif kind == "pause":
        place["playing"] = False
    elif kind == "seek":
        place["ms"] = op["ms"]
    elif kind in ("next", "previous"):
        place["index"] = max(0, min(len(queue) - 1, place["index"] + (1 if kind == "next" else -1)))
        place["ms"] = 0
    elif kind == "jump":
        place["index"], place["ms"] = op["index"], 0
    else:
        return False
    print(f"obeyed: {kind}", flush=True)
    return True


try:
    publish()
    since = None
    shown_index = place["index"]
    while True:
        p = {"dev": dev, "name": name, "kind": "terminal", "serve": "0"}
        if since is not None:
            p.update(since=str(since), hold="1")
        # Short holds while a request waits, so it is accepted on time; and near a song's end, so the next one
        # is said as it starts.
        to_end_s = (queue[place["index"]]["duration"] * 1000 - place_now()["ms"]) / 1000 if place["playing"] else 1e9
        quick = bool(pending) or since is None or to_end_s < 55
        answer = call("noriRemote.poll", **({**p, "hold": "0"} if quick else p))
        since = answer["seq"]
        now = place_now()
        changed = now["index"] != shown_index or now["playing"] != place["playing"]
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
            received = now_us()
            op, who = body["op"], next((m["name"] for m in members if m["id"] == e["from"]), "?")
            if op.get("op") == "clock":
                answer_to = {"room": room, "to": e["from"], "body": {"t": "clock", "t1": op["t1"], "t2": received, "t3": now_us()}}
                call("noriRemote.send", answer_to, dev=dev, name=name, kind="terminal")
                continue
            refusal = None
            if op.get("op") == "request":
                song = op["song"]
                pending.append({"request": next_request, "from": e["from"], "fromName": who, "song": entry(song, 0), "provider": song["id"].startswith("ext-"),
                                "at": time.time()})
                next_request += 1
                print(f"asked: {song.get('title', song['id'])} by {who}", flush=True)
                changed = True
            elif admins and obey(op):
                changed = True
            else:
                refusal = "notAllowed"
            call("noriRemote.send", {"room": room, "to": e["from"], "body": {"t": "ack", "id": body["id"], "refusal": refusal}}, dev=dev, name=name, kind="terminal")
        for w in [w for w in pending if time.time() - w["at"] >= ACCEPT_AFTER]:
            pending.remove(w)
            queue.append({**w["song"], "index": len(queue), "turn": len(queue), "by": w["fromName"]})
            rev += 1
            print(f"accepted: {w['song']['title']} for {w['fromName']}", flush=True)
            changed = True
        if changed:
            publish()
            shown_index = place["index"]
        if quick and since is not None:
            time.sleep(0.5)
finally:
    call("noriRemote.close", room=room)
    print("the jam is over", flush=True)
