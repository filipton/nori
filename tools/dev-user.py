#!/usr/bin/env python3
"""Create one isolated account in the local generated-music fixture."""
import json
import re
import sys
import urllib.request

name = sys.argv[1]
if not re.fullmatch(r"nori-e2e-[a-zA-Z0-9-]+", name):
    raise SystemExit("expected a nori-e2e device account")
token = ""


def call(path, body=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["x-nd-authorization"] = "Bearer " + token
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request("http://localhost:4533" + path, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


token = call("/auth/login", {"username": "admin", "password": "admin"})["token"]
if not any(user["userName"] == name for user in call("/api/user")):
    call("/api/user", {"userName": name, "name": name, "password": "nori-e2e",
                       "isAdmin": False, "libraryIds": [item["id"] for item in call("/api/library")]})
