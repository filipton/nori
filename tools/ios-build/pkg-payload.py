#!/usr/bin/env python3
# Writes the Payload (a gzipped cpio) of a macOS .pkg's component to stdout: pkg-payload.py <pkg> <component>.
# A .pkg is a xar archive; libarchive's xar reader mangles its names, so this reads the table of contents.
import shutil
import struct
import sys
import xml.etree.ElementTree as ET
import zlib

pkg, component = sys.argv[1], sys.argv[2]
with open(pkg, "rb") as f:
    magic, header_size, _, toc_size, _, _ = struct.unpack(">4sHHQQI", f.read(28))
    if magic != b"xar!":
        sys.exit(f"{pkg}: not a xar archive")
    f.seek(header_size)
    toc = ET.fromstring(zlib.decompress(f.read(toc_size)))
    heap = header_size + toc_size
    for entry in toc.iter("file"):
        if entry.findtext("name") != component:
            continue
        payload = next(e for e in entry.findall("file") if e.findtext("name") == "Payload")
        data = payload.find("data")
        f.seek(heap + int(data.findtext("offset")))
        remaining = int(data.findtext("length"))
        while remaining:
            chunk = f.read(min(remaining, 1 << 20))
            sys.stdout.buffer.write(chunk)
            remaining -= len(chunk)
        break
    else:
        sys.exit(f"{pkg}: no {component}")
