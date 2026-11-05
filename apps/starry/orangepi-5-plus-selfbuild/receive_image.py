#!/usr/bin/env python3
"""Verify and restore an unmounted filesystem image using bounded HTTP parts."""

import hashlib
import json
import os
from pathlib import Path
import re
import sys
import time
import urllib.request

BLOCK = 1024 * 1024


def hash_range(stream, size):
    value = hashlib.sha256()
    while size:
        data = stream.read(min(BLOCK, size))
        if not data:
            return None
        value.update(data)
        size -= len(data)
    return value.hexdigest()


def receive(url, base):
    manifest = json.loads((base / "session/manifest.json").read_text())
    if manifest.get("format") != 1 or not manifest.get("parts"):
        raise ValueError("unsupported or empty image manifest")
    if not re.fullmatch(r"[0-9a-f]{64}", manifest["sha256"]):
        raise ValueError("invalid image checksum")
    total = 0
    for index, part in enumerate(manifest["parts"]):
        if (part["name"] != "image.part-{:03d}".format(index)
                or not isinstance(part["size"], int) or not 0 < part["size"] <= 60 * BLOCK
                or not re.fullmatch(r"[0-9a-f]{64}", part["sha256"])):
            raise ValueError("invalid image part")
        total += part["size"]
    if manifest["size"] != total:
        raise ValueError("image size does not match parts")
    image = base / "image.ext4"
    mode = "r+b" if image.exists() else "w+b"
    with image.open(mode, buffering=0) as output:
        offset = 0
        for part in manifest["parts"]:
            for attempt in range(1, 4):
                stage = "download"
                try:
                    output.seek(offset)
                    if attempt == 1 and hash_range(output, part["size"]) == part["sha256"]:
                        stage = "writeback"
                        os.fsync(output.fileno())
                        print("selfbuild_image_part=" + part["name"] + " verified=reused", flush=True)
                        break
                    value = hashlib.sha256()
                    output.seek(offset)
                    with urllib.request.urlopen(url + "/" + part["name"], timeout=180) as response:
                        remaining = part["size"]
                        while remaining:
                            data = response.read(min(BLOCK, remaining))
                            if not data:
                                raise ValueError("short image part: " + part["name"])
                            written = output.write(data)
                            if written != len(data):
                                raise OSError("short image write")
                            value.update(data)
                            remaining -= len(data)
                        if response.read(1):
                            raise ValueError("oversized image part: " + part["name"])
                    if value.hexdigest() != part["sha256"]:
                        raise ValueError("image part checksum mismatch: " + part["name"])
                    # A checksum alone is not durable. Retry the same range on
                    # writeback failure before accepting the following part.
                    stage = "writeback"
                    os.fsync(output.fileno())
                    print("selfbuild_image_part=" + part["name"] + " verified=downloaded", flush=True)
                    break
                except (OSError, ValueError) as error:
                    print("selfbuild_image_retry={} attempt={} stage={} error={}".format(
                        part["name"], attempt, stage, error), flush=True)
                    if attempt == 3:
                        raise
                    time.sleep(2)
            offset += part["size"]
        output.truncate(total)
        output.seek(0)
        if hash_range(output, total) != manifest["sha256"]:
            raise ValueError("complete image checksum mismatch")
        os.fsync(output.fileno())
    print("selfbuild_image=verified sha256=" + manifest["sha256"], flush=True)


if __name__ == "__main__":
    receive(sys.argv[1].rstrip("/"), Path(sys.argv[2]))
