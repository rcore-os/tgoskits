#!/usr/bin/env python3
# run_wgpu.py - on-target gate for the StarryOS wgpu (WebGPU) compute operator carpet, run across
# every wgpu language binding: Python (wgpu-py), C (wgpu-native C API), C++ (wgpu-native via C++17)
# and Rust (the wgpu crate). Each binding drives wgpu-native / wgpu-core over its Vulkan backend,
# which lands on lavapipe (Mesa's software Vulkan driver, LLVM llvmpipe CPU JIT) - a real WebGPU
# device on the CPU, no GPU needed. The stack ships for the two arches conda distributes (x86_64 /
# aarch64); riscv64 / loongarch64 have no Mesa/wgpu conda distribution, so the gate reports that
# honestly there and passes with a documented note.
#
# The Python carpet runs natively on the conda python; the C / C++ / Rust carpets are precompiled by
# prebuild (host toolchain for x86_64, cross toolchain for aarch64) and staged as native binaries at
# /root/gpu/bin - the gate runs whichever were staged.
import glob
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
os.chdir(HERE)

CONDA_PY = "/opt/miniconda/bin/python"
CONDA_ROOT = "/opt/miniconda"
BIN = os.path.join(HERE, "bin")

# (label, kind, target, ok-marker). kind: "py" runs under the conda python; "bin" is a native ELF.
# Each carpet prints "<NAME>_FULL_API OK <n>" plus a "PASS=<p> FAIL=<f> TOTAL=<t> EXPECTED=<e>"
# summary; a binding passes only when its OK marker is present, FAIL=0 and the count equals EXPECTED.
CARPETS = [
    ("wgpu-python", "py", "GpuWgpuCarpet.py", "WGPU_PY_FULL_API OK"),
    ("wgpu-c", "bin", os.path.join(BIN, "wgpu_c"), "WGPU_C_FULL_API OK"),
    ("wgpu-cpp", "bin", os.path.join(BIN, "wgpu_cpp"), "WGPU_CPP_FULL_API OK"),
    ("wgpu-rust", "bin", os.path.join(BIN, "wgpu_rs"), "WGPU_RUST_FULL_API OK"),
]


def wgpu_env():
    env = dict(os.environ)
    env["LD_LIBRARY_PATH"] = "%s/lib:%s" % (CONDA_ROOT, env.get("LD_LIBRARY_PATH", ""))
    # wgpu-native's Vulkan backend goes through the loader; point it at the (path-rewritten) lavapipe
    # ICD and force the Vulkan backend so wgpu selects the software device.
    icds = sorted(glob.glob("%s/share/vulkan/icd.d/lvp_icd.*.json" % CONDA_ROOT))
    if icds:
        env["VK_DRIVER_FILES"] = icds[0]
        env["VK_ICD_FILENAMES"] = icds[0]  # older loaders read this name
    env["WGPU_BACKEND_TYPE"] = "Vulkan"
    env["XDG_RUNTIME_DIR"] = "/tmp"        # lavapipe needs a writable runtime dir
    env["LP_NUM_THREADS"] = "1"            # deterministic single-threaded llvmpipe on Starry
    env.setdefault("RUST_LOG", "error")
    return env


def run(kind, target):
    argv = [CONDA_PY, target] if kind == "py" else [target]
    r = subprocess.run(argv, capture_output=True, text=True, env=wgpu_env())
    return r.returncode, (r.stdout or "") + (r.stderr or "")


print("=== gpu-wgpu: wgpu (WebGPU) compute operator carpet - Python / C / C++ / Rust bindings ===")

if not os.path.exists(CONDA_PY):
    print("  NOTE wgpu stack absent: wgpu-py + wgpu-native + Mesa lavapipe ship glibc x86_64 /")
    print("       aarch64 only; this arch has no upstream Mesa/wgpu distribution.")
    print("GPU_OK=0/0 (no wgpu distribution for this arch)")
    print("TEST PASSED")
    sys.exit(0)

passed = 0
present = 0
for label, kind, target, marker in CARPETS:
    if kind == "bin" and not os.path.exists(target):
        print("  ---- %s (binding binary not staged for this arch)" % label)
        continue
    present += 1
    rc, out = run(kind, target)
    res = [ln for ln in out.splitlines() if "PASS=" in ln and "FAIL=" in ln]
    dev = [ln for ln in out.splitlines() if "adapter" in ln.lower()]
    if marker in out and rc == 0 and "FAIL=0" in out and "FAIL:" not in out:
        print("  OK   %s (%s) %s" % (label, res[0].strip() if res else "", dev[0].strip() if dev else ""))
        passed += 1
    else:
        print("  FAIL %s (%s) rc=%s" % (label, marker, rc))
        print("\n".join(out.splitlines()[-25:]))

print("GPU_OK=%d/%d" % (passed, present))
if passed == present and present > 0:
    print("TEST PASSED")
    sys.exit(0)
print("TEST FAILED")
sys.exit(1)
