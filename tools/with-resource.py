#!/usr/bin/env python3
"""Queue expensive host work or exclusive device checks across agent worktrees."""
import fcntl
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def main():
    if len(sys.argv) < 3:
        sys.exit("usage: with-resource.py build|device:SERIAL[,RESOURCE...] COMMAND [ARG...]")
    resources = sorted(set(sys.argv[1].split(",")))
    inherited = set(filter(None, os.environ.get("NORI_HELD_RESOURCES", "").split(",")))
    directory = Path(tempfile.gettempdir()) / f"nori-resources-{os.getuid()}"
    directory.mkdir(mode=0o700, exist_ok=True)
    locks = []
    if resources == ["device"]:
        devices = os.environ.get("NORI_E2E_DEVICES", os.environ.get("ANDROID_SERIAL", "emulator-5554")).split(",")
        if not devices or any(not device.strip() for device in devices):
            sys.exit("NORI_E2E_DEVICES must contain device serials")
        devices = [device.strip() for device in devices]
        waiting = False
        selected = "device:" + os.environ.get("ANDROID_SERIAL", "")
        if selected in inherited:
            resources = [selected]
        while resources == ["device"] and not locks:
            for device in devices:
                resource = f"device:{device}"
                lock = (directory / hashlib.sha256(resource.encode()).hexdigest()).open("a+")
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    lock.close()
                    continue
                locks.append(lock)
                resources = [resource]
                os.environ["ANDROID_SERIAL"] = device
                break
            if not locks:
                if not waiting:
                    print("waiting for a nori emulator", file=sys.stderr, flush=True)
                    waiting = True
                time.sleep(0.2)
    for resource in resources:
        if locks and resource.startswith("device:") and sys.argv[1] == "device":
            continue
        if resource in inherited:
            continue
        path = directory / hashlib.sha256(resource.encode()).hexdigest()
        lock = path.open("a+")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(f"waiting for nori resource: {resource}", file=sys.stderr, flush=True)
            fcntl.flock(lock, fcntl.LOCK_EX)
        locks.append(lock)
    env = dict(os.environ, NORI_HELD_RESOURCES=",".join(sorted(inherited | set(resources))))
    child = subprocess.Popen(sys.argv[2:], env=env, start_new_session=True)

    def forward(signum, _frame):
        try:
            os.killpg(child.pid, signum)
        except ProcessLookupError:
            pass

    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, forward)
    code = child.wait()
    for lock in locks:
        lock.close()
    return code if code >= 0 else 128 - code


if __name__ == "__main__":
    sys.exit(main())
