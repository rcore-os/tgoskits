# Reproduction guide

This guide reproduces the completed QEMU Linux/Zephyr captures and the physical
Orange Pi 5 Plus StarryOS/Zephyr flow, and identifies the video and upstream PR
work separately. Run all commands from the repository root unless a step says
otherwise.

## 1. Pin and record the source

The retained evidence and current QEMU captures were produced on branch
`feat/rt-axvisor-partition-virtio-net` while `HEAD` was the base commit below
plus uncommitted competition changes:

```text
263f89d8f3d0481d2712224a7b517a73b1165fb3
```

That base hash alone does **not** contain the historical QEMU implementation.
The physical-board support is represented by the current worktree and its
separate metadata below; do not relabel the older retained QEMU archives as
though they came from that later source state. For every new run, capture:

```sh
git rev-parse HEAD
git status --short
git submodule status --recursive
```

Do not compare measurements from different source states without recording the
difference.

The retained physical StarryOS captures used branch
`feat/rt-axvisor-partition-virtio-net` at base commit
`f808646899f51fde9addfbe60976f6667c760beb` plus the uncommitted implementation
described by
[`results/orangepi-starry-reference/metadata.json`](results/orangepi-starry-reference/metadata.json).
That base commit alone also does not contain the physical implementation.

The five retained AxVisor RT captures and the three final IVC captures used the
same implementation source state. The harness recorded:

```text
source snapshot     8594ab76e903dd179db5f1aa91546c03a7d759454d300b2ac6c665933ab0216a
tracked binary diff 13e7a0689bd1e2606c76b8373b2701cf1bbc6f2bed4201e60269e1915d9cc7f5
untracked manifest  4f8072e04fb5619c5157bb7b58682474a55c5c1b5b2a48567fd09330f78c0eaf (89 files)
```

This is an honest dirty-worktree attestation, not a replacement for a future
committed revision. Generated build trees, `tmp/`, and retained result files
are pruned from the source manifest.

The later formal physical campaigns use clean commits instead of that
historical dirty attestation. In particular, RKNN NPU v8 is pinned to
`c3f01dc34b83695eddf8da83cf4ed71622f64f7c`, and ONNX Runtime CPU v4 is
pinned to `0110647de52f5e2ad6b550cb594780d7506ffecf`. Reproducing either result
requires a new clean worktree and a new result directory; do not run a formal
campaign from the Windows working tree merely because its files have been
synchronized into WSL2.

## 2. Environment

The successful AxVisor checks used WSL Ubuntu with:

| Dependency | Recorded or required value |
| --- | --- |
| Rust | repository-pinned `nightly-2026-07-15`; observed `rustc 1.99.0-nightly (da80ed070 2026-07-14)` |
| QEMU | `qemu-system-aarch64 10.0.3` |
| device-tree compiler | `dtc 1.7.2` |
| libclang used by Rust bindings | `/usr/lib/llvm-14/lib` |
| Linux image | repository image manager's AArch64 Alpine image, observed release `v0.0.10` |
| Zephyr source | upstream tag `v4.3.0`, commit `3568e1b6d5cdd51a6b964a2a1d6d29200fea2056` |
| Zephyr guest compiler | `aarch64-linux-gnu-gcc 11.4.0`, selected through the recorded `cross-compile` prefix shim |
| Physical board | Xunlong Orange Pi 5 Plus, RK3588, 16 GiB DRAM |
| Board automation host | WSL2 Ubuntu with a CH340 serial adapter at 1,500,000 baud and SSH access to the board Linux image |

The model pipeline additionally uses two isolated, hash-locked environments:
CPython 3.10.12 for ONNX/RKNN conversion and CPython 3.12.11 with ONNX Runtime
1.25.0 for ORT export/host verification. Follow
[`ivc/model/README.md`](ivc/model/README.md) for the exact hash-locked
`uv pip sync --require-hashes` commands, deterministic rebuild, 10,000-vector,
vendor-source, and license gates. Do not install the RKNN and ORT lock files
into one environment.

Typical Ubuntu packages are:

```sh
sudo apt-get update
sudo apt-get install -y \
  build-essential clang libclang-14-dev llvm-14-dev \
  qemu-system-arm device-tree-compiler e2fsprogs cpio fakeroot rsync \
  git cmake ninja-build python3 python3-pip python3-venv
```

Python 3.11 and newer provide `tomllib` in the standard library. On Ubuntu
22.04 and other hosts using Python 3.10 or older, install the pinned backport
used by the configuration contract tests:

```sh
python3 -m pip install --user -r competition/requirements-host.txt
```

Rebuilding the AxVisor RT probe needs a static AArch64 musl compiler named
`aarch64-linux-musl-gcc`. For exact campaign reproduction, use the retained
718,296-byte static AArch64 ELF at
[`results/axvisor-rt-reference/axvisor-rt-probe`](results/axvisor-rt-reference/axvisor-rt-probe)
with `run.sh --probe PATH`. Its SHA-256 is
`8b3f6e7471dc9ecf60d5b64ab5f3c3a4657af8743fde1aa6b1772358c62806da`.
The compiler version that produced this already-built artifact was not
separately captured, so no compiler-version provenance is claimed for it. If
the probe is rebuilt, record the compiler version and resulting hash, and do
not silently mix different probe binaries within a comparison pair.

The repository's [`rust-toolchain.toml`](../rust-toolchain.toml) supplies the
Rust components and bare-metal targets. Add the static Linux target used for
the guest controller:

```sh
rustup +nightly-2026-07-15 target add aarch64-unknown-linux-musl
rustc +nightly-2026-07-15 --version --verbose
qemu-system-aarch64 --version
dtc --version
```

The validated Zephyr source resolves to the exact upstream `v4.3.0` tag. The
retained Windows-hosted checkout uses `core.autocrlf=true`; Git's post-run
status is clean after applying that canonical line-ending filter. The
provenance records the tag object, peeled commit, Git-index hash, and clean
status after both native QEMU runs. The guest was built with Ubuntu's AArch64
GCC 11.4.0 and binutils 2.38 through the repo-local prefix shim described in
[`ivc/zephyr/README.md`](ivc/zephyr/README.md). An official Zephyr SDK remains
the preferred reproduction route; do not mix artifacts from the two toolchain
routes without recording new hashes.

## 3. Host protocol and policy gates

These commands were exercised during implementation:

```sh
cargo +nightly-2026-07-15 test -p ivcproto
cargo +nightly-2026-07-15 check -p ivcproto --no-default-features --lib
cargo +nightly-2026-07-15 clippy -p ivcproto --all-targets -- -D warnings

cargo +nightly-2026-07-15 test -p axvmconfig
cargo +nightly-2026-07-15 test -p axvm-net
cargo +nightly-2026-07-15 test -p axdevice
```

Validate all five full-profile guest descriptions independently with the
current CLI:

```sh
cargo +nightly-2026-07-15 run -p axvmconfig -- \
  check --config-path competition/ivc/config/linux-smp2.toml

cargo +nightly-2026-07-15 run -p axvmconfig -- \
  check --config-path competition/ivc/config/linux-smp2-manual.toml

cargo +nightly-2026-07-15 run -p axvmconfig -- \
  check --config-path competition/ivc/config/zephyr-smp1.toml

cargo +nightly-2026-07-15 run -p axvmconfig -- \
  check --config-path competition/ivc/config/linux-smp2-ack-loss.toml

cargo +nightly-2026-07-15 run -p axvmconfig -- \
  check --config-path competition/ivc/config/zephyr-smp1-ack-loss.toml
```

All five invocations above exited zero and reported their configurations valid
during final validation.

Compile and run the Zephyr protocol/endpoint logic on the host with strict C11
warnings before acquiring the full Zephyr tree:

```sh
bash competition/ivc/zephyr/run-host-tests.sh
```

The success line is `host-logic-tests: PASS`.

The real-time result analyzer has a standalone deterministic test suite:

```sh
python3 -m unittest discover \
  -s scripts/benchmark/axvisor-rt/tests \
  -p 'test_*.py'
```

After all source changes, use the repository orchestration for the final
clippy sweep where supported, then format:

```sh
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask clippy --package ivcproto
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask clippy --package axvm-net
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask clippy --package axdevice
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask clippy --package axvmconfig
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask clippy --package axvm
cargo +nightly-2026-07-15 fmt --all -- --check
```

If an `xtask clippy` command is blocked by an unrelated workspace/platform
build, record that failure and run the corresponding direct Cargo clippy with
the same target/features rather than silently dropping the check.

## 4. Regenerate host evidence

### UDP fault injection

The checked-in run used 100 commands and dropped the first ACK for every fifth
new sequence. Generate a new, non-overwriting result directory:

```sh
IVC_COUNT=100 IVC_DROP_EVERY=5 IVC_PORT=45500 \
  bash competition/ivc/run-host-loopback.sh \
  tmp/competition/ivc/host-loopback-100-drop5
```

Expected functional invariants are 100 acknowledged commands, 20
retransmissions, 20 suppressed duplicates, zero protocol errors/timeouts, and
safe fallback after controller silence. Latency values vary with the host and
must not be forced to match the retained log.

### Deterministic neural/manual comparison

```sh
cargo +nightly-2026-07-15 run -p ivcproto -- \
  evaluate-csv tmp/competition/ivc/host-ai.csv

sha256sum tmp/competition/ivc/host-ai.csv
```

The deterministic raw CSV should match the retained
[`raw.csv`](results/host-ai-reference/raw.csv), whose recorded SHA-256 is:

```text
9e2f2e8f471413afc08066898621e7e00ddd0f844a3d043adf64fd181f3be584
```

## 5. Build the Linux controller image

The script pulls the managed AArch64 image when missing, builds a static
`aarch64-unknown-linux-musl` controller with `rust-lld`, makes a private image
copy, and injects `/usr/local/bin/ivcproto` plus `/ivc-init.sh`:

```sh
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  bash competition/ivc/linux/build-rootfs.sh
```

Output:

```text
tmp/competition/ivc/linux/rootfs.img
```

The retained QEMU profiles use this Linux image. The current physical profile
instead runs the same static controller under StarryOS. Build its two-vCPU
kernel, guest DTB, and finite 20/1,800-command ext4 images with:

```sh
bash competition/ivc/starry/build.sh
```

The outputs are `tmp/competition/ivc/starry/starryos.bin`,
`starry-orangepi-5-plus.dtb`, `starry-ivc-rootfs-smoke.img`, and
`starry-ivc-rootfs.img`. The build script runs ext4 checks before and after
injecting the controller, autorun entry point, and finite profile.

The controller used by the final QEMU captures is 758,608 bytes with SHA-256
`73a825d12ac79a268a28e10ff5e572a313a57f4ce4e2780a5de4df824e430965`.
The freshly built 2 GiB rootfs before the campaign had SHA-256
`3dad2a5733e066b09def9dcbd063adaaf1407df0f344c0be6a4b566f1aa945d5`.
Each run used a private copy. Guest mounting/ext4 recovery changed filesystem
metadata in those copies, so the post-run image hash is not a content ID and
the mutable images are not retained. A rebuilt image may differ because of
filesystem timestamps; always record the new pre-run hash.

## 6. Build the Zephyr endpoint

Create an upstream workspace pinned to v4.3.0. `<repo>` below must be the
absolute path to this repository:

```sh
python3 -m venv .venv-zephyr
. .venv-zephyr/bin/activate
python -m pip install --upgrade pip west

west init -m https://github.com/zephyrproject-rtos/zephyr \
  --mr v4.3.0 zephyrproject
cd zephyrproject
west update
west zephyr-export
python -m pip install -r zephyr/scripts/requirements.txt
git -C zephyr describe --tags --exact-match
```

The final command above must print `v4.3.0`. With an installed and recorded
Zephyr SDK, build directly into the path referenced by the VM configuration:

```sh
export ZEPHYR_TOOLCHAIN_VARIANT=zephyr
export ZEPHYR_SDK_INSTALL_DIR=<absolute-zephyr-sdk-directory>

west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build \
  <repo>/competition/ivc/zephyr

west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build-ack-loss \
  <repo>/competition/ivc/zephyr -- \
  -DEXTRA_CONF_FILE=ack-loss.conf

sha256sum \
  <repo>/competition/ivc/zephyr/build/zephyr/zephyr.elf \
  <repo>/competition/ivc/zephyr/build/zephyr/zephyr.bin \
  <repo>/competition/ivc/zephyr/build-ack-loss/zephyr/zephyr.elf \
  <repo>/competition/ivc/zephyr/build-ack-loss/zephyr/zephyr.bin
```

Build the finite physical-board images into the paths referenced by the
Orange Pi VM configurations:

```sh
west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build-board-smoke \
  <repo>/competition/ivc/zephyr -- \
  -DEXTRA_CONF_FILE=board-smoke.conf

west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build-board \
  <repo>/competition/ivc/zephyr -- \
  -DEXTRA_CONF_FILE=board.conf
```

The validated non-SDK builds used the same source tag with
`ZEPHYR_TOOLCHAIN_VARIANT=cross-compile` and the recorded GCC prefix. Its final
layout was `PT_LOAD` at `0x40000000` with entry `0x4000100c`:

```text
normal zephyr.elf  2170024 bytes  0643a85c9f999cc3780a4f57f9992262e535d2889d0b1d08b3dd1b544acfe7ac
normal zephyr.bin   121568 bytes  13b7bd6cca6398824a947cc7e038b996dd9a29227873bd065158e9873e723f68
fault  zephyr.elf  2170920 bytes  81749add8e14a4db9f3c2d388c07ba7f0f803242745b8c2bc4c2ee47b20227d4
fault  zephyr.bin   121568 bytes  c2ea50effd0b1e910a88b75c6c57b89052269877867e1ef670ecdd20102d1550
```

A native QEMU smoke test produced `IVC-RTOS-SELFTEST PASS`, the configured MAC,
and `IVC-RTOS-READY bind=10.0.0.2:5500`. Exact host-package versions, the
compiler-prefix rationale, and the standalone command are retained in
[`ivc/zephyr/README.md`](ivc/zephyr/README.md).

## 7. Build and boot AxVisor

### Completed Linux-only build

This exact command passed in the recorded WSL environment:

```sh
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask axvisor build \
  --arch aarch64 \
  --smp 4 \
  --config competition/ivc/config/axvisor-aarch64.toml \
  --vmconfigs competition/ivc/config/linux-smp2.toml
```

### Completed two-vCPU Linux boot gate

This separate validated partition test passed 1/1 and observed two online Linux
CPUs:

```sh
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask axvisor test qemu \
  --arch aarch64 \
  --test-group normal \
  --test-case dedicated-smp2
```

The success marker is `AXVISOR_DEDICATED_PARTITION_PASS`; the guest command
uses `getconf _NPROCESSORS_ONLN` and requires exactly 2. This test uses the
standalone pCPU2/pCPU3 partition profile, not the complete two-guest IVC
profile.

### Completed Linux + Zephyr neural, manual, and ACK-loss runs

Each campaign used a private copy of the freshly built rootfs so ext4 recovery
or guest writes could not contaminate another run:

```sh
mkdir -p \
  tmp/competition/ivc/reference-20260731-neural \
  tmp/competition/ivc/reference-20260731-manual \
  tmp/competition/ivc/reference-20260731-ack-loss

cp --reflink=auto tmp/competition/ivc/linux/rootfs.img \
  tmp/competition/ivc/reference-20260731-neural/rootfs-pre-run.img
cp --reflink=auto tmp/competition/ivc/linux/rootfs.img \
  tmp/competition/ivc/reference-20260731-manual/rootfs-pre-run.img
cp --reflink=auto tmp/competition/ivc/linux/rootfs.img \
  tmp/competition/ivc/reference-20260731-ack-loss/rootfs-pre-run.img
```

The final neural command was:

```sh
set -o pipefail
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask axvisor qemu \
  --arch aarch64 --smp 4 \
  --config competition/ivc/config/axvisor-aarch64.toml \
  --qemu-config competition/ivc/config/qemu-aarch64.toml \
  --rootfs tmp/competition/ivc/reference-20260731-neural/rootfs-pre-run.img \
  --vmconfigs competition/ivc/config/linux-smp2.toml \
  --vmconfigs competition/ivc/config/zephyr-smp1.toml \
  2>&1 | tee tmp/competition/ivc/reference-20260731-neural/qemu.log

python3 competition/ivc/analyze_qemu.py \
  tmp/competition/ivc/reference-20260731-neural/qemu.log \
  --output tmp/competition/ivc/reference-20260731-neural/summary.json \
  --expected-count 1800
```

For manual fixed control, use the manual rootfs/output paths and replace the
Linux VM description with
`competition/ivc/config/linux-smp2-manual.toml`; the Zephyr description is
unchanged. Both analyzers require `IVC-RTOS-SELFTEST`, both network-ready
markers, exactly 1,800 terminal records, `IVC-LINUX-DONE exit=0`, zero
errors/timeouts, and monotonic latency families.

```text
neural log  6c7f7e2e404a5c8ef8a9a3f632a24169b35d8be6a8c0ac496775bf9d32a07eb8
             full-loop 3894 / 4652 / 5657 / 20917 us (p50/p95/p99/max)
manual log  39ac8deaf5382490a007bfd47ec7384989c64c6092eed70ac8ff682c076d8a57
             full-loop 3902 / 4670 / 5423 / 19656 us (p50/p95/p99/max)
```

The deterministic ACK-loss campaign uses both fault-specific VM descriptions:

```sh
set -o pipefail
LIBCLANG_PATH=/usr/lib/llvm-14/lib \
  cargo +nightly-2026-07-15 xtask axvisor qemu \
  --arch aarch64 --smp 4 \
  --config competition/ivc/config/axvisor-aarch64.toml \
  --qemu-config competition/ivc/config/qemu-aarch64.toml \
  --rootfs tmp/competition/ivc/reference-20260731-ack-loss/rootfs-pre-run.img \
  --vmconfigs competition/ivc/config/linux-smp2-ack-loss.toml \
  --vmconfigs competition/ivc/config/zephyr-smp1-ack-loss.toml \
  2>&1 | tee tmp/competition/ivc/reference-20260731-ack-loss/qemu.log

python3 competition/ivc/analyze_qemu.py \
  tmp/competition/ivc/reference-20260731-ack-loss/qemu.log \
  --output tmp/competition/ivc/reference-20260731-ack-loss/summary.json \
  --expected-count 100 --profile ack-loss --drop-ack-every 5
```

Its log SHA-256 is
`f15c88c6671db67934ce178e3f113b65ac2811a1538a0c36412f6c156bd279fd`.
The analyzer requires the exact sequence set 5, 10, ..., 100 for all 20
injections and duplicate recoveries, 100 fresh applications, 100/100
acknowledgements, and zero terminal errors/timeouts.

The complete compressed logs, summaries, and image/config/source provenance
are retained in
[`results/axvisor-ivc-reference`](results/axvisor-ivc-reference/). Do not
compare neural and manual unless every non-policy input remains identical.

### Completed Orange Pi 5 Plus StarryOS + Zephyr runs

The physical profile loads StarryOS and Zephyr through AxVisor on the RK3588
and uses the board's Linux ext4 filesystem as the AxVisor image store. First
build the StarryOS artifacts as described above and both finite Zephyr board
images from Section 6. Stage the StarryOS kernel, two rootfs profiles, and DTB
while holding a board lease. The maintained script uses the ordinary
`orangepi` home directory, atomically renames each upload, calls `sync`, and
prints the remote hashes:

```sh
export ORANGEPI_SSH_TARGET=orangepi@<board-ip>
export ORANGEPI_SSH_IDENTITY=<board-ssh-private-key>

bash competition/ivc/stage-orangepi-5-plus.sh
```

The run selector delegates reset/serial orchestration to
`ORANGEPI_AXVISOR_RUNNER`, which defaults to the WSL host helper
`orangepi-axvisor-board-run`. That host integration must acquire the local
board lease, reboot Linux only after `sync`, run `cargo xtask axvisor board`,
require `AXVISOR_HOST_FILESYSTEM_SYNCED`, and restore the TF-card Linux system.
The run script retains the console, invokes `analyze_board.py`, and exits zero
only after the analyzer validates controller/RTOS counts, StarryOS two-vCPU and
network records, both guest shutdowns, host filesystem sync, and Linux restore.
With the validated AxVisor board root selector:

```sh
export ORANGEPI_AXVISOR_HOST_ROOT=/dev/mmcblk0p2

bash competition/ivc/run-orangepi-5-plus.sh smoke
bash competition/ivc/run-orangepi-5-plus.sh full
```

This name is from AxVisor's block enumeration: AxVisor `disk0p2` is the
TF-card root filesystem that restored Linux reports as `/dev/mmcblk1p2`.
Passing Linux's `/dev/mmcblk1p2` string to AxVisor selects its eMMC `disk1p2`
`misc` partition and fails before guest boot. On another board, inspect both
Linux and AxVisor enumeration rather than assuming the numbers are identical.

By default, results are written to
`tmp/competition/ivc/orangepi-last/{smoke,full}-{console.log,summary.json}`.
Set `ORANGEPI_IVC_RESULT_DIR` for a different output directory.

Discover and record the root selector on a different board with
`findmnt -no SOURCE,FSTYPE,OPTIONS /` and `blkid`; do not copy this PARTUUID
blindly. Without the host integration helper, the underlying command is:

```sh
cargo xtask axvisor board \
  -c competition/ivc/config/axvisor-orangepi-5-plus-smoke.toml \
  --board-config competition/ivc/config/board-orangepi-5-plus-smoke.toml \
  -b OrangePi-5-Plus
```

Without the integration helper, the operator must arrange the Linux-to-U-Boot
reboot after the serial lease is acquired and restore Linux after AxVisor
shuts down.

The retained full neural capture completed 1,800/1,800 commands with zero
errors, timeouts, retransmissions, or recoveries. Its full-loop
p50/p95/p99/maximum was 6,751/11,265/11,695/14,405 us and throughput was
9.995 msg/s. The raw-log SHA-256 is
`023ff07b40b4936453eee6d4bbd57bca1c1699e7305dc1af5fe601a5d67492d9`.
The retained smoke completed 20/20, observed two StarryOS CPUs, confirmed both
guest poweroffs and host filesystem sync, and restored `/dev/mmcblk1p2` as
ext4 `rw`; its raw-log SHA-256 is
`8dd16dbcc7608305da9fcf13f393a54e410e16ef26da63ca5c2821878efbf265`.

The shared physical UART can lose spans. Controller and RTOS terminal metrics
are split into short, paced, redundant records. The analyzer requires at least
one complete copy, rejects conflicting complete copies, and never treats the
short success marker alone as sufficient evidence. Compressed logs, generated
summaries, exact artifact/configuration hashes, and verification commands are
retained under
[`results/orangepi-starry-reference`](results/orangepi-starry-reference/).

#### Reproduce the formal ONNX Runtime CPU campaign

First create the locked ORT environment and verify the canonical model as
documented in [`ivc/model/README.md`](ivc/model/README.md). Build the 1,800-cycle
160 MiB control rootfs from the clean formal worktree:

```sh
export LIBCLANG_PATH=/usr/lib/llvm-14/lib
export PATH=/home/seven_wsl/.cargo/bin:/home/seven_wsl/.local/bin:/home/linuxbrew/.linuxbrew/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export IVC_ORT_PYTHON=/home/seven_wsl/.cache/tgoskits/ivc-ort-py312/bin/python

bash competition/ivc/starry/build-ort-control-rootfs.sh full
sha256sum \
  tmp/competition/ivc/starry/starryos.bin \
  tmp/competition/ivc/starry/starry-orangepi-5-plus.dtb \
  tmp/competition/ivc/starry/starry-ivc-rootfs-ort-control.img \
  competition/ivc/zephyr/build-board/zephyr/zephyr.bin \
  competition/ivc/model/thermal-4x6x1-v1.ort
```

The v4 rootfs SHA-256 was
`6884919950d96f38082e7cc647575bc040d25b7de3ae13c8aed828e39747615c`;
the StarryOS kernel, DTB, Zephyr binary, and ORT model hashes were respectively
`3bd0a4982d4b6ef278f1a3c20b88cec888bdac772fad119ed6d9fa4e4f9f33bd`,
`0f533e1107894dd9b3f062f726fee012519c3e55ef0a2e81e2507e7e3ef303cd`,
`d02b6de2677c8f2a26db1514ae7d6e0a1723ada329c59bc17b6f58333bd075ff`,
and `3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887`.
A different build must be treated as a new campaign, not labeled v4.

Configure the existing SSH, serial, lease, and smart-plug integrations without
printing their secrets. The following paths are the validated WSL2 layout;
replace them when reproducing elsewhere:

```sh
export ORANGEPI_SSH_TARGET=orangepi@192.168.31.33
export ORANGEPI_SSH_IDENTITY=/home/seven_wsl/.ssh/orangepi_automation
export ORANGEPI_AXVISOR_HOST_ROOT=/dev/mmcblk0p2
export TGOS_BOARD_POWER_CONFIG=/home/seven_wsl/Workspace/starry/tgoskits-rt-ivc/.board-power.toml
export ORANGEPI_POWER_PYTHON=/home/seven_wsl/.local/share/tgos-board-power-venv/bin/python

test -z "$(git status --porcelain=v1)"
bash competition/ivc/run-ort-control-campaign.sh \
  --result-dir tmp/competition/ivc/ort-control-full-formal-YYYYMMDD-v1 \
  --expected-commit "$(git rev-parse HEAD)" \
  --timeout 900
```

The wrapper refuses a dirty source or an existing result root. Before
`run-001`, it writes the five-run order, artifact hashes, cold-start semantics,
thresholds, and `replacement_runs_allowed=false` into `preregistration.json`.
It then performs five independent boot/run/snapshot/fsck/Linux-restore cycles
and aggregates only if every immutable run passes. A failed directory must be
retained; start any amended campaign in a new directory and rerun all five.

Independently verify the committed v4 archive without modifying it:

```sh
campaign=competition/results/orangepi-5-plus/ort-control-full-formal-20260805-v4/ort-full
verify_dir=$(mktemp -d)

python3 competition/ivc/aggregate_ort_campaign.py "$campaign" \
  --expected-commit 0110647de52f5e2ad6b550cb594780d7506ffecf \
  --output "$verify_dir/campaign-summary.json"

python3 - "$campaign/campaign-summary.json" \
  "$verify_dir/campaign-summary.json" <<'PY'
import json
import sys
from pathlib import Path

captured = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
rebuilt = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
for summary in (captured, rebuilt):
    summary["campaign"]["path"] = "<campaign-root>"
    summary["preregistration"]["path"] = "<preregistration>"
if captured != rebuilt:
    raise SystemExit("reaggregated ORT summary differs beyond relocation paths")
print("ORT_REAGGREGATION_NORMALIZED_MATCH")
PY

for run_dir in "$campaign"/run-*; do
  (cd "$run_dir" && sha256sum -c checksums.sha256)
done
(cd "$campaign" && sha256sum -c campaign-checksums.sha256)
sha256sum \
  "$campaign/preregistration.json" \
  "$campaign/campaign-summary.json" \
  "$campaign/campaign-checksums.sha256"
```

The final three digests must be
`04768defc09ce5e9a0069ead59bd01ea9fc696b32f46fdcd3619797327beded4`,
`57edb5f8a1fc79bcbd43fb3fd77aec25151e7d773985a56b12e6d3530d14d3f9`,
and `601b435f376841dcfbb54e0c8bbac5fd9e6ffb09e4c08c4f67e73f2934d85a25`.
The summary must report five runs, 9,000/9,000 ACK,
`formal_gate_passed=true`, runtime 1.25.0, and
`provider=CPUExecutionProvider`.

The retained summary intentionally preserves the original formal `tmp/` paths.
Reaggregating the relocated repository copy therefore changes exactly
`campaign.path` and `preregistration.path`; the normalization above ignores
only those two self-location fields and requires every measured value,
threshold, identity, digest, and timestamp to remain equal. Do not edit the
frozen summary to make a raw `cmp` pass. A raw byte-for-byte comparison was
also completed before relocation while the campaign remained at its recorded
formal path.

#### Reproduce and verify the formal RKNN NPU campaign

Use the separate CPython 3.10.12 RKNN environment and run every conversion,
simulator, source/license, and deterministic rebuild check from
[`ivc/model/README.md`](ivc/model/README.md). Reuse the SSH, AxVisor root,
smart-plug config, and power-Python exports shown above, then build the
RKNPU-enabled StarryOS kernel/DTB plus 20/1,800-cycle rootfs images:

```sh
export IVC_RKNN_PYTHON=/home/seven_wsl/.cache/tgoskits/ivc-rknn-py310-formal/bin/python

bash competition/ivc/starry/build-rknpu-control.sh
test -z "$(git status --porcelain=v1)"

result_root=tmp/competition/ivc/rknpu-control-full-formal-YYYYMMDD-v1
bash competition/ivc/run-orangepi-5-plus.sh rknpu-full \
  --repeat 5 \
  --board OrangePi-5-Plus \
  --result-dir "$result_root" \
  --timeout 900 \
  --restore-linux \
  --require-clean

python3 competition/ivc/aggregate_rknpu_campaign.py \
  "$result_root/rknpu-full" \
  --expected-runs 5 \
  --expected-count 1800 \
  --expected-commit "$(git rev-parse HEAD)" \
  --output "$result_root/aggregate.json"
```

The generic repeated runner does not create a top-level RKNN preregistration
file. For a new formal claim, freeze source/artifact hashes, five-run order,
cold-start semantics, thresholds, and no-replacement policy in a separate
read-only contract before invoking it. The retained v8 thresholds and
amendments are recorded in [`execution-plan.md`](execution-plan.md); do not
retroactively infer a contract from a successful output.

The repository-retained v8 archive contains no rootfs, Toolkit wheel, Runtime
library, or other vendor binary. It contains the five UART logs, raw/RKNN CSV
pairs, metadata, summaries, stage logs, and per-run manifests. Verify it and
recompute the aggregate as follows:

```sh
rknpu_root=competition/results/orangepi-5-plus/rknpu-control-full-formal-20260805-v8
rknpu_campaign="$rknpu_root/rknpu-full"
rknpu_frozen="$rknpu_root/rknpu-control-full-formal-20260805-v8-aggregate-v2-398932fef.json"
rknpu_verify_dir=$(mktemp -d)

for run_dir in "$rknpu_campaign"/run-*; do
  (cd "$run_dir" && sha256sum -c checksums.sha256)
done

python3 competition/ivc/aggregate_rknpu_campaign.py "$rknpu_campaign" \
  --expected-runs 5 \
  --expected-count 1800 \
  --expected-commit c3f01dc34b83695eddf8da83cf4ed71622f64f7c \
  --output "$rknpu_verify_dir/aggregate.json"

python3 - "$rknpu_frozen" "$rknpu_verify_dir/aggregate.json" <<'PY'
import json
import sys
from pathlib import Path

captured = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
rebuilt = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
for summary in (captured, rebuilt):
    summary["campaign"]["path"] = "<campaign-root>"
if captured != rebuilt:
    raise SystemExit("reaggregated RKNN summary differs beyond relocation path")
print("RKNN_REAGGREGATION_NORMALIZED_MATCH")
PY

sha256sum "$rknpu_frozen"
```

The final digest must be
`dfc7d844b4d219992d72e7b8be22a18be6b49d4e18feca993df2eaad2eff6f27`.
The aggregate must report clean source, five runs, 9,000/9,000 ACK, Runtime
2.3.2, driver 0.9.8, model hash
`2ad3fecedc9767ee57cbcd31787f70297a8f8e2cfcdc8e07b81b949566d53bb8`,
five sequence-1 misses, and zero misses in the other 8,995 cycles. Positive
device times and `host_submit=false` establish NPU execution; they do not make
an acceleration claim.

## 8. Real-time benchmarks

### Completed AxVisor shared-versus-partitioned campaign

Pull the managed rootfs and run the guest probe without modifying the source
image:

```sh
cargo +nightly-2026-07-15 xtask image pull --arch aarch64

scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --profile partitioned \
  --iterations 10000 \
  --warmup 100 \
  --period-us 1000 \
  --workload idle
```

The rootfs argument may name the managed image directory; the runner resolves
its same-named image file. The runner requires `aarch64-linux-musl-gcc`, or the
recorded static probe supplied with `--probe PATH`.

Run the four paired cases into new, non-overwriting directories while keeping
rootfs, probe, QEMU, sample count, warm-up, period, and workload pair identical:

```sh
scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-shared-idle \
  --profile shared --iterations 10000 --warmup 100 \
  --period-us 1000 --workload idle

scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-shared-stress \
  --profile shared --iterations 10000 --warmup 100 \
  --period-us 1000 --workload cpu-stress

scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-partitioned-idle \
  --profile partitioned --iterations 10000 --warmup 100 \
  --period-us 1000 --workload idle

scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-partitioned-stress \
  --profile partitioned --iterations 10000 --warmup 100 \
  --period-us 1000 --workload cpu-stress
```

The final soak used the partitioned stress profile at a 10 ms period:

```sh
scripts/benchmark/axvisor-rt/run.sh \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img \
  --probe competition/results/axvisor-rt-reference/axvisor-rt-probe \
  --output tmp/competition/axvisor-rt/reproduction-partitioned-soak \
  --profile partitioned --iterations 10000 --warmup 100 \
  --period-us 10000 --workload cpu-stress
```

Each normal metric loop measures about 10 seconds. The soak measures 100
seconds per metric, 300 seconds total; its complete metadata interval was 13
minutes because build, boot, setup, warm-up, inter-metric transitions, and
shutdown are outside those loops. Reported CPU percentages are Linux guest CPU
load, not host-pCPU utilization.

The five validated summaries, metadata files, compressed raw logs, hashes, and
qualified comparison are retained under
[`results/axvisor-rt-reference`](results/axvisor-rt-reference/). Follow the
metric definitions and analyzer command in
[`scripts/benchmark/axvisor-rt/README.md`](../scripts/benchmark/axvisor-rt/README.md).

### Completed native Zephyr comparison baseline

The retained native Zephyr v4.3.0 QEMU baseline includes one 10,000-sample idle
run and one verified CPU-stress run at a 1 ms period:

```sh
bash competition/rt-baseline/zephyr/prepare.sh
bash competition/rt-baseline/zephyr/run.sh all \
  tmp/competition/rt-baseline/zephyr/reproduction-1
```

See [`results/native-zephyr-reference`](results/native-zephyr-reference/) for
validated summaries, source provenance, artifact hashes, load distribution,
and platform-difference analysis. It is not an AxVisor capture, a soak, or a
hardware worst-case bound.

## 9. Evidence retention checklist

For every QEMU, native-RTOS, stress, and soak run, retain in a new directory:

- source commit and clean/dirty status;
- UTC start/end, requested duration, actual duration, and exit status;
- host OS/kernel, CPU, QEMU, Rust, compiler/SDK, and image versions;
- exact command and full console log;
- guest and image SHA-256 hashes;
- workload command plus CPU affinity/load evidence;
- raw samples before derived statistics when the benchmark emits them; the
  native Zephyr baseline is an explicit exception whose retained console has
  aggregate records only and no serialized individual-sample series;
- analyzer version/command and derived JSON/CSV;
- backend identity, Runtime/driver version, model hash, and raw inference
  samples when the claim distinguishes native CPU, ORT CPU, or RKNN NPU;
- preregistration created before the first formal run, immutable failed-run
  directories, per-run manifests, and an independently rebuilt campaign
  summary for repeated physical campaigns;
- synchronized block snapshot, read-only fsck result, smart-plug lifecycle,
  and restored Linux root-device/FSType/options for every board run; and
- a limitations note for clock source, QEMU TCG, logging perturbation, and any
  platform difference.

Never copy the planned metadata template into `competition/results` as though
it were a completed measurement.
