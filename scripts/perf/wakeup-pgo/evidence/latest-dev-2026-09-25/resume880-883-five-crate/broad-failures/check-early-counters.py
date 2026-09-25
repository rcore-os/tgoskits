#!/usr/bin/env python3
"""Reject profile counter writes in the pre-MMU boot functions of an ELF."""

import json
import re
import subprocess
import sys
from pathlib import Path

ELF = Path(sys.argv[1])
LAYOUT = json.loads(Path(sys.argv[2]).read_text())
EXPECT_POSITIVE = len(sys.argv) == 4 and sys.argv[3] == "--expect-positive"
START = int(LAYOUT["counter_address"], 16)
END = int(LAYOUT["counter_end"], 16)
HEADER = re.compile(r"^[0-9a-f]+ <(.+)>:$")
INSTRUCTION = re.compile(r"^\s*([0-9a-f]+):\s+[0-9a-f]+\s+([a-z0-9.]+)\s*(.*)$")
ADRP = re.compile(r"x(\d+),\s*(?:0x)?([0-9a-f]+)")
ADD = re.compile(r"x(\d+),\s*x(\d+),\s*#(?:0x)?([0-9a-f]+)")
LOAD = re.compile(r"\[x(\d+)\]")


def inspect():
    sites = []
    function = ""
    pages = {}
    addresses = {}
    index = 0
    process = subprocess.Popen(
        ["aarch64-linux-gnu-objdump", "-d", "-C", str(ELF)],
        stdout=subprocess.PIPE,
        text=True,
    )
    assert process.stdout is not None
    for line in process.stdout:
        header = HEADER.match(line.strip())
        if header:
            function = header.group(1)
            pages.clear()
            addresses.clear()
            index = 0
            continue
        if not ("someboot::" in function or "ax_cpu::boot::" in function):
            continue
        match = INSTRUCTION.match(line)
        if not match:
            continue
        pc, opcode, operands = match.groups()
        index += 1
        if opcode == "adrp":
            match = ADRP.search(operands)
            if match:
                pages[int(match.group(1))] = (int(match.group(2), 16), index)
        elif opcode == "add":
            match = ADD.search(operands)
            if match:
                dest, source = int(match.group(1)), int(match.group(2))
                page = pages.get(source)
                if page and index - page[1] <= 3:
                    address = page[0] + int(match.group(3), 16)
                    if START <= address < END:
                        addresses[dest] = (address, index)
        elif opcode in ("ldxr", "ldaxr"):
            match = LOAD.search(operands)
            if match:
                address = addresses.get(int(match.group(1)))
                if address and index - address[1] <= 4:
                    sites.append({"function": function, "pc": f"0x{pc}", "counter": hex(address[0])})
    if process.wait() != 0:
        raise SystemExit("objdump failed")
    return sites


sites = inspect()
print(json.dumps({"elf": str(ELF), "counter_sites": len(sites), "first": sites[:20]}, indent=2))
if bool(sites) != EXPECT_POSITIVE:
    raise SystemExit("unexpected early profile counter sites")
