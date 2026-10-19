#!/usr/bin/env python3
"""Build the pinned Zephyr guest and the resources of the `vcpu_runtime` case.

The script only prepares build inputs for the `axvm` `vcpu_runtime` ktest case.
It compiles the fixed Zephyr revision, derives the guest entry point and the
`virq_mailbox` address from the produced ELF, and writes the VM description
(`vm.toml`) together with the host-initramfs fixture
(`resources/tests/vcpu-runtime.toml`). It never starts the Axvisor host, never
runs QEMU and never judges test output: the ktest suite marker decides pass and
fail at runtime.

The pinned Zephyr checkout, the GNU Arm 10.3-2021.07 toolchain and the Python
virtualenv used by CMake live in one cache directory so that a machine or CI
job without local tools can reproduce the same guest build. `ZEPHYR_BASE`,
`CROSS_COMPILE` and `AXBUILD_TARGET_DIR` are honoured, and every path can be
overridden on the command line.
"""

import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import sys
from typing import Sequence

ZEPHYR_REVISION = "32229d0b6cc60a1906307311b8e79ec483ad1e88"
ZEPHYR_REPOSITORY = "https://github.com/zephyrproject-rtos/zephyr"
TOOLCHAIN_ARCHIVE_URL = (
    "https://developer.arm.com/-/media/Files/downloads/gnu-a/10.3-2021.07/binrel/"
    "gcc-arm-10.3-2021.07-x86_64-aarch64-none-elf.tar.xz"
)
TOOLCHAIN_ROOT_NAME = "gcc-arm-10.3-2021.07-x86_64-aarch64-none-elf"
# The old standalone workflow pinned these versions; keep the same set so the
# guest build stays byte-comparable with the previous runs.
PINNED_PYTHON_PACKAGES = (
    "cmake==4.4.3",
    "ninja==1.13.2",
    "pyelftools==0.33",
    "PyYAML==6.0.3",
    "packaging==26.3",
    "pykwalify==1.8.0",
)
BOARD = "qemu_cortex_a53/qemu_cortex_a53/smp"
MAILBOX_SYMBOL = "virq_mailbox"
MAILBOX_SIZE = 20
GUEST_RAM_START = 0x4000_0000
GUEST_RAM_SIZE = 0x0800_0000
DTB_LOAD_ADDR = 0x47E0_0000
VM_ID = 2
VM_CPU_NUM = 2
VM_NAME = "zephyr-vcpu-runtime"


def repo_root() -> Path:
    """Workspace root derived from this file, independent of the cwd."""
    return Path(__file__).resolve().parents[5]


def run(command: Sequence[str], *, env: dict[str, str] | None = None) -> None:
    """Run one preparation command and fail loudly with its own output."""
    result = subprocess.run(command, env=env, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"command failed with status {result.returncode}: {' '.join(command)}")


def target_dir() -> Path:
    """Axbuild target directory, `AXBUILD_TARGET_DIR` or `<repo>/target`."""
    configured = os.environ.get("AXBUILD_TARGET_DIR")
    if configured:
        return Path(configured).expanduser().resolve()
    return repo_root() / "target"


def default_output() -> Path:
    return target_dir() / "axbuild/axvm/vcpu-runtime"


def default_cache() -> Path:
    configured = os.environ.get("VIRQ_CACHE_DIR")
    if configured:
        return Path(configured).expanduser().resolve()
    return target_dir() / "axbuild/axvm/vcpu-runtime-cache"


def python_tools_dir(cache: Path) -> Path:
    """Return the `bin` directory of the pinned build virtualenv."""
    venv = cache / "venv"
    stamp = venv / "requirements.sha256"
    signature = hashlib.sha256("\n".join(PINNED_PYTHON_PACKAGES).encode()).hexdigest()
    if not stamp.is_file() or stamp.read_text().strip() != signature:
        venv.parent.mkdir(parents=True, exist_ok=True)
        print(f"[virq] creating build virtualenv {venv}")
        if not (venv / "bin/python3").is_file():
            run([sys.executable, "-m", "venv", str(venv)])
        run(
            [
                str(venv / "bin/pip"),
                "install",
                "--disable-pip-version-check",
                *PINNED_PYTHON_PACKAGES,
            ]
        )
        stamp.write_text(signature + "\n")
    return venv / "bin"


def ensure_python_runtime(cache: Path) -> Path:
    """Re-execute this script inside the cached build virtualenv.

    CMake, Ninja, PyYAML and pyelftools all come from the same pinned
    virtualenv, so a runner only needs a `python3` able to create virtualenvs
    instead of preinstalled distribution packages.
    """
    venv = cache / "venv"
    interpreter = python_tools_dir(cache) / "python3"
    if Path(sys.prefix).resolve() == venv.resolve():
        return interpreter
    print(f"[virq] re-executing under {interpreter}")
    os.execv(str(interpreter), [str(interpreter), str(Path(__file__).resolve()), *sys.argv[1:]])
    raise AssertionError("os.execv returned unexpectedly")


def git(source: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-C", str(source), *arguments],
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def clone_pinned(source: Path) -> None:
    """Clone exactly the pinned revision into the cache directory."""
    source.parent.mkdir(parents=True, exist_ok=True)
    print(f"[virq] cloning Zephyr {ZEPHYR_REVISION} into {source}")
    run(["git", "init", "--quiet", str(source)])
    run(["git", "-C", str(source), "remote", "add", "origin", ZEPHYR_REPOSITORY])
    try:
        run(
            [
                "git",
                "-C",
                str(source),
                "fetch",
                "--depth",
                "1",
                "origin",
                ZEPHYR_REVISION,
            ]
        )
    except RuntimeError:
        print("[virq] shallow fetch of the pinned revision failed; fetching all refs")
        run(["git", "-C", str(source), "fetch", "origin"])
    run(["git", "-C", str(source), "checkout", "--detach", ZEPHYR_REVISION])


def verify_zephyr_source(source: Path) -> None:
    """Require the pinned revision and a clean tracked tree before building."""
    if not (source / "CMakeLists.txt").is_file():
        raise RuntimeError(f"{source} is not a Zephyr checkout")
    revision = git(source, "rev-parse", "HEAD").strip()
    if revision != ZEPHYR_REVISION:
        raise RuntimeError(f"expected Zephyr {ZEPHYR_REVISION}, found {revision} in {source}")
    dirty = git(source, "status", "--porcelain").strip()
    if dirty:
        raise RuntimeError(
            f"Zephyr sources in {source} are modified:\n{dirty}\n"
            "use a clean checkout of the pinned revision"
        )


def ensure_zephyr_source(cache: Path, override: Path | None) -> Path:
    """Resolve the Zephyr sources from the CLI, `ZEPHYR_BASE` or the cache."""
    if override is not None:
        source = override
    else:
        configured = os.environ.get("ZEPHYR_BASE")
        source = Path(configured).expanduser().resolve() if configured else cache / "zephyr"
        if not (source / "CMakeLists.txt").is_file() and not configured:
            clone_pinned(source)
    verify_zephyr_source(source)
    return source


def install_toolchain(cache: Path) -> str:
    """Download and unpack the pinned GNU Arm bare-metal toolchain."""
    root = cache / "toolchain"
    prefix = root / TOOLCHAIN_ROOT_NAME / "bin/aarch64-none-elf-"
    archive = root / Path(TOOLCHAIN_ARCHIVE_URL).name
    if not Path(f"{prefix}gcc").is_file():
        root.mkdir(parents=True, exist_ok=True)
        if not archive.is_file():
            print(f"[virq] downloading {TOOLCHAIN_ARCHIVE_URL}")
            run(["curl", "-fL", "--retry", "3", "--output", str(archive), TOOLCHAIN_ARCHIVE_URL])
        expected = os.environ.get("VIRQ_TOOLCHAIN_SHA256")
        if expected:
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            if digest != expected.casefold():
                raise RuntimeError(
                    f"toolchain archive {archive} has sha256 {digest}, expected {expected}"
                )
        print(f"[virq] unpacking {archive}")
        run(["tar", "-xJf", str(archive), "-C", str(root)])
    if not Path(f"{prefix}gcc").is_file():
        raise RuntimeError(f"{prefix}gcc is missing after unpacking {archive}")
    return str(prefix)


def ensure_cross_compile(cache: Path, override: str | None) -> str:
    """Resolve an absolute CROSS_COMPILE prefix for the guest build."""
    prefix = override or os.environ.get("CROSS_COMPILE") or install_toolchain(cache)
    if not Path(prefix).is_absolute():
        raise RuntimeError(f"CROSS_COMPILE must be an absolute prefix, got {prefix}")
    if not Path(f"{prefix}gcc").is_file():
        raise RuntimeError(f"{prefix}gcc is missing; pass a full toolchain prefix")
    return prefix


def report_toolchain(prefix: str) -> None:
    version = subprocess.run(
        [f"{prefix}gcc", "--version"], check=False, capture_output=True, text=True
    ).stdout.splitlines()
    print(f"[virq] CROSS_COMPILE={prefix} {version[0] if version else 'unknown version'}")


def build_guest(
    source_dir: Path,
    build_dir: Path,
    zephyr_base: Path,
    cross_compile: str,
    tools_dir: Path,
    python: Path,
    jobs: str,
) -> Path:
    """Configure and build the Zephyr guest, returning the produced ELF."""
    env = dict(os.environ)
    env["ZEPHYR_BASE"] = str(zephyr_base)
    env["ZEPHYR_TOOLCHAIN_VARIANT"] = "cross-compile"
    # Keep the build from picking up unrelated west modules.
    env["WEST"] = "NOTFOUND"
    env["PATH"] = os.pathsep.join([str(tools_dir), env.get("PATH", "")])
    cmake = tools_dir / "cmake"
    build_dir.mkdir(parents=True, exist_ok=True)
    run(
        [
            str(cmake),
            "--fresh",
            "-GNinja",
            "-S",
            str(source_dir),
            "-B",
            str(build_dir),
            f"-DBOARD={BOARD}",
            "-DWEST=NOTFOUND",
            "-DCONFIG_MINIMAL_LIBC=y",
            f"-DCROSS_COMPILE={cross_compile}",
            f"-DPython3_EXECUTABLE={python}",
        ],
        env=env,
    )
    run([str(cmake), "--build", str(build_dir), "-j", jobs], env=env)
    elf = build_dir / "zephyr/zephyr.elf"
    if not elf.is_file():
        raise RuntimeError(f"the guest build did not produce {elf}")
    return elf


def inspect_guest(elf: Path) -> tuple[int, int]:
    """Read the entry point and mailbox address and enforce the RAM contract."""
    from elftools.elf.elffile import ELFFile

    with elf.open("rb") as stream:
        image = ELFFile(stream)
        entry = int(image.header.e_entry)
        symbols = image.get_section_by_name(".symtab")
        if symbols is None:
            raise RuntimeError(f"{elf} has no symbol table; cannot locate {MAILBOX_SYMBOL}")
        matches = symbols.get_symbol_by_name(MAILBOX_SYMBOL)
        if not matches:
            raise RuntimeError(f"{elf} does not define {MAILBOX_SYMBOL}")
        symbol = matches[0]
        mailbox = int(symbol["st_value"])
        size = int(symbol["st_size"])
    if size != MAILBOX_SIZE:
        raise RuntimeError(f"{MAILBOX_SYMBOL} is {size} bytes, expected {MAILBOX_SIZE}")
    end = GUEST_RAM_START + GUEST_RAM_SIZE
    if not GUEST_RAM_START <= mailbox or mailbox + MAILBOX_SIZE > end:
        raise RuntimeError(
            f"{MAILBOX_SYMBOL} at 0x{mailbox:x} does not fit guest RAM "
            f"[0x{GUEST_RAM_START:x}, 0x{end:x})"
        )
    if not GUEST_RAM_START <= entry < end:
        raise RuntimeError(f"entry point 0x{entry:x} is outside guest RAM [0x{GUEST_RAM_START:x}, 0x{end:x})")
    return entry, mailbox


def write_vm_config(output: Path, kernel: Path, entry: int) -> Path:
    """Write the VM description consumed by the host initramfs bundle."""
    # `zephyr.bin` is the flat image; keep the historical RAM, DTB and entry
    # layout unchanged. The path stays relative so the whole output directory
    # can be moved or archived as one resource bundle.
    relative_kernel = kernel.relative_to(output)
    config = output / "vm.toml"
    config.write_text(
        f"""[base]
id = {VM_ID}
name = "{VM_NAME}"
guest_type = "virtualized"
cpu_num = {VM_CPU_NUM}
phys_cpu_ids = [0, 1]
[kernel]
entry_point = 0x{entry:x}
kernel_path = "{relative_kernel.as_posix()}"
kernel_load_addr = 0x{GUEST_RAM_START:x}
dtb_load_addr = 0x{DTB_LOAD_ADDR:x}
memory_regions = [[0x{GUEST_RAM_START:x}, 0x{GUEST_RAM_SIZE:08x}, 0x7, 0]]
[devices]
passthrough = []
disabled = []
"""
    )
    return config


def write_test_resources(output: Path, entry: int, mailbox: int) -> Path:
    """Write the initramfs fixture read by the `vcpu_runtime` axtest target."""
    resources = output / "resources/tests"
    resources.mkdir(parents=True, exist_ok=True)
    fixture = resources / "vcpu-runtime.toml"
    fixture.write_text(
        f"""# Generated by virtualization/axvm/tests/guests/virq/prepare.py.
# `entry` and `mailbox` stay fixed for the pinned guest revision.
entry = 0x{entry:x}
mailbox = 0x{mailbox:x}
"""
    )
    return fixture


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zephyr-base", type=Path, help="pinned Zephyr checkout (or ZEPHYR_BASE)")
    parser.add_argument("--cross-compile", help="absolute toolchain prefix (or CROSS_COMPILE)")
    parser.add_argument("--output", type=Path, default=None, help="generated resource directory")
    parser.add_argument("--cache-dir", type=Path, default=None, help="Zephyr/toolchain/venv cache")
    parser.add_argument("--jobs", default=str(os.cpu_count() or 4), help="parallel guest build jobs")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    cache = (args.cache_dir or default_cache()).resolve()
    cache.mkdir(parents=True, exist_ok=True)
    python = ensure_python_runtime(cache)
    tools_dir = python_tools_dir(cache)
    for tool in ("cmake", "ninja"):
        if not (tools_dir / tool).is_file():
            raise RuntimeError(f"{tools_dir / tool} is missing from the prepared virtualenv")
    output = (args.output or default_output()).resolve()
    zephyr_base = ensure_zephyr_source(cache, args.zephyr_base.resolve() if args.zephyr_base else None)
    cross_compile = ensure_cross_compile(cache, args.cross_compile)
    report_toolchain(cross_compile)
    elf = build_guest(
        Path(__file__).resolve().parent,
        output / "guest",
        zephyr_base,
        cross_compile,
        tools_dir,
        python,
        args.jobs,
    )
    entry, mailbox = inspect_guest(elf)
    config = write_vm_config(output, elf.with_name("zephyr.bin"), entry)
    fixture = write_test_resources(output, entry, mailbox)
    print(f"[virq] zephyr={ZEPHYR_REVISION} entry=0x{entry:x} mailbox=0x{mailbox:x}")
    print(f"[virq] vm config {config}")
    print(f"[virq] test resources {fixture}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
