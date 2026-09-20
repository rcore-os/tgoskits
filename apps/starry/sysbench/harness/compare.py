#!/usr/bin/env python3
"""Compare complete runs of the same board bundle without inferring causes."""
import argparse
import math
from pathlib import Path
import re


def parse(text):
    headers = re.findall(r"^SYSBENCH_BEGIN (.+)$", text, re.M)
    if len(headers) != 1 or len(re.findall(r"^SYSBENCH_DONE\s*$", text, re.M)) != 1:
        raise ValueError("expected exactly one complete run")
    if re.search(r"^SYSBENCH_(?:BOARD_)?FAILED\b", text, re.M):
        raise ValueError("run contains a failure")
    config = dict(item.split("=", 1) for item in headers[0].split())
    if config.get("schema") != "1" or config.get("mode") != "board":
        raise ValueError("only schema 1 board measurements are comparable")
    hashes = re.findall(r"^ENV bundle_sha256=([0-9a-f]{64})$", text, re.M)
    affinity = re.findall(r"^ENV affinity=([0-9 ]+)$", text, re.M)
    if len(hashes) != 1 or len(affinity) != 1:
        raise ValueError("missing bundle hash or CPU affinity")
    cpus = affinity[0].split()
    if len(set(cpus)) != len(cpus) or len(cpus) != int(config["cpus"]):
        raise ValueError("invalid CPU affinity list")
    expected = {"threads", "mutex", "memory"}
    expected.update(f"cpu-{n}" for n in (1, 2, 4, 8) if n <= len(cpus))
    expected.update(f"{kind}-{cpu}" for cpu in cpus for kind in ("probe", "membw", "pinned"))
    cases = {}
    active = None
    for line in text.splitlines():
        if line.startswith("CASE_BEGIN "):
            name = line.removeprefix("CASE_BEGIN ")
            if active is not None or name in cases:
                raise ValueError("nested or repeated case")
            active = name
            cases[name] = []
        elif line.startswith("CASE_END "):
            if active is None or line.removeprefix("CASE_END ") != active:
                raise ValueError("unmatched case end")
            active = None
        elif active is not None:
            cases[active].append(line)
    if active is not None or set(cases) != expected:
        raise ValueError("incomplete or unexpected workload set")
    metrics = {}
    for name, lines in cases.items():
        body = "\n".join(lines)
        if name.startswith(("probe-", "membw-")):
            prefix = "CPUPROBE" if name.startswith("probe-") else "MEMBW"
            records = re.findall(rf"^{prefix} (.+)$", body, re.M)
            if len(records) != 1:
                raise ValueError(f"{name}: missing probe result")
            fields = dict(item.split("=", 1) for item in records[0].split())
            requested = fields["req" if prefix == "CPUPROBE" else "core"]
            if requested != name.split("-")[1] or fields["landed"] != requested:
                raise ValueError(f"{name}: affinity was not honored")
            units = ("ips",) if prefix == "CPUPROBE" else ("firsttouch_s", "memcpy_GBps", "read_GBps")
            for unit in units:
                metrics[name, unit] = float(fields[unit])
        elif name == "memory":
            matches = re.findall(r"\(([0-9.]+) MiB/sec\)", body)
            if len(matches) != 1:
                raise ValueError("missing memory bandwidth")
            metrics[name, "MiB_per_second"] = float(matches[0])
        else:
            events = re.findall(r"total number of events:\s*(\d+)", body)
            elapsed = re.findall(r"total time:\s*([0-9.]+)s", body)
            if len(events) != 1 or len(elapsed) != 1 or float(elapsed[0]) <= 0:
                raise ValueError(f"{name}: missing completed sysbench summary")
            metrics[name, "events_per_second"] = int(events[0]) / float(elapsed[0])
    if not all(math.isfinite(value) and value > 0 for value in metrics.values()):
        raise ValueError("measurement must be finite and positive")
    return (config, hashes[0], cpus), metrics


def compare(linux, starry):
    left_config, left = parse(linux)
    right_config, right = parse(starry)
    if left_config != right_config or left.keys() != right.keys():
        raise ValueError("bundle, parameters or CPU affinity differ")
    rows = ["case\tunit\tLinux\tStarryOS\tStarryOS/Linux"]
    for key in sorted(left):
        rows.append(f"{key[0]}\t{key[1]}\t{left[key]:.6g}\t{right[key]:.6g}\t{right[key] / left[key]:.4f}")
    return "\n".join(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("linux", type=Path)
    parser.add_argument("starry", type=Path)
    args = parser.parse_args()
    try:
        print(compare(args.linux.read_text(errors="replace"), args.starry.read_text(errors="replace")))
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"comparison rejected: {error}\n")


if __name__ == "__main__":
    main()
