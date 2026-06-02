#!/usr/bin/env python3
"""Perceptual PPM diff with a tolerance band.

Counts pixels whose RGB channels differ by more than `--delta` from the
golden. If that count exceeds `--max-changed-pct` of the total, exits
non-zero. On failure, emits a per-channel histogram + bounding box of
the changed region to help pinpoint what moved.

Tolerance defaults are chosen to absorb the jitter we actually see:
anti-aliasing on text (usually <2% of pixels), clock-time redraws
(a few hundred pixels per minute boundary), compositor cursor-hide
fade, and TCG's slightly non-deterministic frame boundaries. If a
test frames things in ways that need tighter or looser bounds, pass
custom thresholds on the invocation.

Input files are P6 (binary) PPM written by rfb_capture.py. PNG isn't
directly supported to keep this script dependency-free (core Python
only); the visual-test CI job converts goldens back-and-forth via
ImageMagick in the orchestrator.
"""
import argparse
import struct
import sys


def read_ppm(path: str):
    with open(path, "rb") as f:
        magic = f.readline().strip()
        if magic != b"P6":
            raise RuntimeError(f"{path}: expected P6 PPM, got {magic!r}")
        # Skip comment lines, tolerate the `width height` + `maxval` pair
        # with or without interleaved whitespace/comments.
        while True:
            line = f.readline()
            if not line.startswith(b"#"):
                break
        w, h = map(int, line.split())
        maxval = int(f.readline().strip())
        if maxval != 255:
            raise RuntimeError(f"{path}: only maxval=255 supported, got {maxval}")
        data = f.read(w * h * 3)
        if len(data) != w * h * 3:
            raise RuntimeError(
                f"{path}: truncated pixel data ({len(data)} < {w*h*3})"
            )
    return w, h, data


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("golden")
    p.add_argument("actual")
    p.add_argument(
        "--delta", type=int, default=8,
        help="per-channel allowed drift before a pixel is 'changed'",
    )
    p.add_argument(
        "--max-changed-pct", type=float, default=1.5,
        help="threshold — if more than this %% of pixels differ beyond "
             "`--delta`, the test fails",
    )
    args = p.parse_args()

    gw, gh, gdata = read_ppm(args.golden)
    aw, ah, adata = read_ppm(args.actual)
    if (gw, gh) != (aw, ah):
        print(
            f"FAIL geometry mismatch: golden {gw}x{gh} vs actual {aw}x{ah}",
            file=sys.stderr,
        )
        return 1

    total = gw * gh
    changed = 0
    # Bounding box of changed region (for debugging regressions).
    min_x, min_y, max_x, max_y = gw, gh, -1, -1
    # Channel-mean delta on changed pixels — distinguishes "color shift"
    # from "everything moved a few pixels".
    sum_dr = sum_dg = sum_db = 0
    for i in range(total):
        off = i * 3
        gr, gg, gb = gdata[off], gdata[off + 1], gdata[off + 2]
        ar, ag, ab = adata[off], adata[off + 1], adata[off + 2]
        dr, dg, db = abs(gr - ar), abs(gg - ag), abs(gb - ab)
        if dr > args.delta or dg > args.delta or db > args.delta:
            changed += 1
            y, x = divmod(i, gw)
            if x < min_x: min_x = x
            if y < min_y: min_y = y
            if x > max_x: max_x = x
            if y > max_y: max_y = y
            sum_dr += dr
            sum_dg += dg
            sum_db += db

    pct = 100.0 * changed / total
    if changed > 0:
        avg_dr = sum_dr / changed
        avg_dg = sum_dg / changed
        avg_db = sum_db / changed
        bbox = f"[{min_x},{min_y})→[{max_x+1},{max_y+1})"
    else:
        avg_dr = avg_dg = avg_db = 0
        bbox = "(none)"

    status = "PASS" if pct <= args.max_changed_pct else "FAIL"
    print(
        f"{status} changed={changed}/{total} ({pct:.3f}%) "
        f"threshold={args.max_changed_pct}% bbox={bbox} "
        f"avg_delta=(R:{avg_dr:.1f},G:{avg_dg:.1f},B:{avg_db:.1f})"
    )
    return 0 if status == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
