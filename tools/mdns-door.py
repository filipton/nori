#!/usr/bin/env python3
"""Resolve one CLI peer's announced LAN door, as the emulator cannot browse host multicast."""
import json
import re
import selectors
import shlex
import sqlite3
import subprocess
import sys
import time


def lines(command, deadline):
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    try:
        while time.monotonic() < deadline:
            if not selector.select(max(0, deadline - time.monotonic())):
                break
            line = process.stdout.readline()
            if not line:
                break
            yield line
    finally:
        selector.close()
        process.terminate()
        process.wait()


def resolve(database, timeout=15):
    with sqlite3.connect(database) as db:
        row = db.execute("SELECT value FROM app_kv WHERE key='remoteDeviceId'").fetchone()
    if not row:
        raise RuntimeError("the CLI peer has not created a remote device")
    device = row[0]
    deadline = time.monotonic() + timeout
    port = None
    for line in lines(["dns-sd", "-L", f"nori-{device}", "_nori._tcp", "local."], deadline):
        match = re.search(r"local\.:([0-9]+)", line)
        if match:
            port = int(match[1])
        txt = dict(word.split("=", 1) for word in shlex.split(line) if "=" in word)
        if port and txt.get("id") == device and txt.get("kind") == "terminal":
            return {"name": txt["name"], "door": f"10.0.2.2|{port}|" + ";".join(f"{k}={v}" for k, v in txt.items())}
    raise RuntimeError("the CLI peer did not announce its LAN door")


if __name__ == "__main__":
    try:
        print(json.dumps(resolve(sys.argv[1])))
    except (RuntimeError, sqlite3.Error) as error:
        sys.exit(str(error))
