#!/usr/bin/env python3
"""Drive a test-control desktop build through its private Unix socket."""
import argparse
import json
import os
import socket

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--socket", default=os.environ.get("NORI_DESKTOP_SOCKET"))
parser.add_argument("command", choices=["state", "open", "do", "panel"])
parser.add_argument("arg", nargs="?")
args = parser.parse_args()
if not args.socket:
    parser.error("set NORI_DESKTOP_SOCKET or pass --socket")
if (args.command == "state") != (args.arg is None):
    parser.error("state has no argument; open, do and panel require one")
request = {"command": args.command}
if args.arg is not None:
    request["arg"] = args.arg
try:
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(5)
        client.connect(args.socket)
        client.sendall((json.dumps(request) + "\n").encode())
        with client.makefile() as reply:
            state = json.loads(reply.readline())
    print(json.dumps(state))
    if "error" in state:
        raise SystemExit(1)
except (OSError, ValueError) as error:
    parser.exit(1, f"desktop control: {error}\n")
