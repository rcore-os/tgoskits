#!/usr/bin/env python3
"""Initialize both axloader records for a one-time legacy-to-A/B migration."""

import argparse
import hashlib
from pathlib import Path
import uuid


def encode(generation: int, a: bytes, b: bytes, update_id: bytes, pending: int) -> bytes:
    record = bytearray(256)
    record[:8] = b"AXOTA001"
    record[8:16] = generation.to_bytes(8, "little")
    record[16] = 0  # stable A
    record[17] = pending
    record[18] = 0  # not attempted
    record[19] = 0  # direct uploader owns first confirmation
    record[20:52] = a
    record[52:84] = b
    record[84:120] = update_id
    record[224:] = hashlib.sha256(record[:224]).digest()
    return bytes(record)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stable", type=Path, required=True)
    parser.add_argument("--trial", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    a = hashlib.sha256(args.stable.read_bytes()).digest()
    b = hashlib.sha256(args.trial.read_bytes()).digest()
    if a == b:
        parser.error("new loader must differ from the legacy loader")
    update_id = str(uuid.uuid4()).encode("ascii")
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "STATE0.BIN").write_bytes(encode(1, a, bytes(32), bytes(36), 255))
    (args.output / "STATE1.BIN").write_bytes(encode(2, a, b, update_id, 1))
    print(f"first trial update_id={update_id.decode('ascii')}")


if __name__ == "__main__":
    main()
