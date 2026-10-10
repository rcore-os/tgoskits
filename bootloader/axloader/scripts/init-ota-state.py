#!/usr/bin/env python3
"""Initialize axloader records for a migration or a fresh A/B installation."""

import argparse
import hashlib
from pathlib import Path
import uuid


def encode(
    generation: int,
    a: bytes,
    b: bytes,
    update_id: bytes,
    pending: int,
) -> bytes:
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


def encode_stable(generation: int, digest: bytes) -> bytes:
    """Create a committed record with A active and no trial image."""
    return encode(generation, digest, digest, bytes(36), 255)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--stable", type=Path)
    group.add_argument(
        "--fresh-image",
        type=Path,
        help="new loader image used for both initial A/B slots",
    )
    parser.add_argument("--trial", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    if args.fresh_image is not None:
        if args.trial is not None:
            parser.error("--trial cannot be used with --fresh-image")
        image = args.fresh_image.read_bytes()
        if not image:
            parser.error("fresh loader image must not be empty")
        digest = hashlib.sha256(image).digest()
        (args.output / "STATE0.BIN").write_bytes(encode_stable(1, digest))
        (args.output / "STATE1.BIN").write_bytes(encode_stable(2, digest))
        print("fresh installation: A is stable and no trial is pending")
        return

    if args.trial is None:
        parser.error("--trial is required with --stable")
    if args.stable is None:
        parser.error("--stable is required without --fresh-image")
    a = hashlib.sha256(args.stable.read_bytes()).digest()
    b = hashlib.sha256(args.trial.read_bytes()).digest()
    if a == b:
        parser.error("new loader must differ from the legacy loader")
    update_id = str(uuid.uuid4()).encode("ascii")
    (args.output / "STATE0.BIN").write_bytes(encode(1, a, bytes(32), bytes(36), 255))
    (args.output / "STATE1.BIN").write_bytes(encode(2, a, b, update_id, 1))
    # Keep this prefix stable: ota_qemu.rs extracts the migration ID from it.
    print(f"first trial update_id={update_id.decode('ascii')}")


if __name__ == "__main__":
    main()
