#!/usr/bin/env python3
"""Build the pinned E1 guest and run Axvisor through the project task entry."""

import argparse
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time

from elftools.elf.elffile import ELFFile

ZEPHYR_REVISION = "32229d0b6cc60a1906307311b8e79ec483ad1e88"
ROOT = Path(__file__).resolve().parents[3]
SUCCESS = (
    "VIRQ_INJECT_COMPLETE vm=2 vcpu=1 vector=48 samples=300 errors=0",
    "E1_COUNTERS idle_vcpu_returns=0 acknowledgements=300",
    "SOFTWARE VIRQ COMPLETE streams=1 samples_each=300 total=300",
    "VM[2] state changed to Stopped",
)
FAILURE = re.compile(r"VIRQ_TEST_FAILED|SOFTWARE VIRQ FAIL|panicked at|Kernel panic|Unhandled exception")


def checked(*command, **kwargs):
    subprocess.run(command, check=True, **kwargs)


def prepare(args):
    revision = subprocess.check_output(
        ["git", "-C", str(args.zephyr_base), "rev-parse", "HEAD"], text=True
    ).strip()
    if revision != ZEPHYR_REVISION:
        raise RuntimeError(f"expected Zephyr {ZEPHYR_REVISION}, found {revision}")
    checked("git", "-C", str(args.zephyr_base), "diff", "--quiet", "HEAD", "--")
    build = args.output / "guest"
    env = dict(os.environ, ZEPHYR_BASE=str(args.zephyr_base), ZEPHYR_TOOLCHAIN_VARIANT="cross-compile")
    checked(
        "cmake", "--fresh", "-GNinja", "-S", str(Path(__file__).parent), "-B", str(build),
        "-DBOARD=qemu_cortex_a53/qemu_cortex_a53/smp", "-DWEST=NOTFOUND",
        "-DCONFIG_MINIMAL_LIBC=y", f"-DCROSS_COMPILE={args.cross_compile}",
        f"-DPython3_EXECUTABLE={sys.executable}", env=env,
    )
    checked("cmake", "--build", str(build), "-j", "8", env=env)
    with (build / "zephyr/zephyr.elf").open("rb") as stream:
        elf = ELFFile(stream)
        entry = elf.header.e_entry
        symbol = elf.get_section_by_name(".symtab").get_symbol_by_name("virq_mailbox")[0]
        mailbox = symbol["st_value"]
        if symbol["st_size"] != 20 or not 0x40000000 <= mailbox < 0x48000000 - 20:
            raise RuntimeError("mailbox does not fit the guest RAM contract")
    vm = args.output / "vm.toml"
    vm.write_text(f'''[base]
id = 2
name = "zephyr-virq-suspend"
guest_type = "virtualized"
cpu_num = 2
phys_cpu_ids = [0, 1]
[kernel]
entry_point = {entry}
image_location = "memory"
kernel_path = {json.dumps(str(build / "zephyr/zephyr.bin"))}
kernel_load_addr = 0x40000000
dtb_load_addr = 0x47e00000
memory_regions = [[0x40000000, 0x08000000, 0x7, 0]]
[devices]
passthrough = []
disabled = []
''')
    config = args.output / "build.toml"
    config.write_text(f'''features = ["test-virq-delivery"]
log = "Info"
target = "aarch64-unknown-none-softfloat"
vm_configs = [{json.dumps(str(vm))}]
[env]
AXVISOR_VIRQ_MAILBOX = "{mailbox:x}"
''')
    return config


def run(config, output):
    command = [
        "cargo", "xtask", "axvisor", "qemu", "--config", str(config), "--smp", "2",
        "--qemu-config", str(Path(__file__).with_name("qemu-aarch64.toml")),
    ]
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT))
    with (output / "qemu.log").open("wb") as log:
        log.write(f"host={head} dirty={dirty} zephyr={ZEPHYR_REVISION}\n".encode())
        process = subprocess.Popen(
            command, cwd=ROOT, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, start_new_session=True,
        )
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 900
        transcript = ""
        try:
            while time.monotonic() < deadline:
                for key, _ in selector.select(timeout=1):
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    if not chunk:
                        raise RuntimeError(f"Axvisor exited before success: {process.wait()}")
                    log.write(chunk)
                    log.flush()
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
                    transcript = (transcript + chunk.decode(errors="replace"))[-2_000_000:]
                    complete_lines = transcript.rsplit("\n", 1)[0]
                    if FAILURE.search(complete_lines):
                        raise RuntimeError("E1 reported a runtime failure")
                    if all(marker in complete_lines for marker in SUCCESS):
                        log.write(b"AXVISOR_VIRQ_SUSPEND_PASSED\n")
                        print("AXVISOR_VIRQ_SUSPEND_PASSED", flush=True)
                        return
            raise RuntimeError("E1 build/run exceeded the 900-second bound")
        finally:
            selector.close()
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zephyr-base", type=Path, required=True)
    parser.add_argument("--cross-compile", required=True, help="absolute cross-compiler prefix")
    parser.add_argument("--output", type=Path, default=ROOT / "tmp/axbuild/virq-suspend")
    parser.add_argument("--prepare-only", action="store_true")
    args = parser.parse_args()
    args.zephyr_base = args.zephyr_base.resolve()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    config = prepare(args)
    if not args.prepare_only:
        run(config, args.output)


if __name__ == "__main__":
    main()
