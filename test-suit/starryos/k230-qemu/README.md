# StarryOS K230 QEMU Test Group

This group contains StarryOS QEMU tests for the K230 machine.

The `boot` case validates the board bring-up path:

- dynamic RISC-V platform boot with the K230 DTB;
- K230 SDHCI rootfs wiring through `-drive if=sd,...`;
- a minimal user-space shell command from the mounted rootfs.

The `kpu-smoke` case validates the first KPU userspace interface:

- `/dev/kpu` and `/dev/kpu0` are registered from the FDT KPU node;
- KPU CFG/L2/fake-output/runtime windows can be accessed through restricted
  `mmap`;
- KPU ioctl submit and wait paths observe QEMU done status and IRQ progress;
- small runtime direct-I/O commands produce checkable output bytes.

Local validation requires a QEMU build with the K230 machine and KPU model. This
machine is not in upstream QEMU 10.1, 10.2, or 11.0, so a normal
`qemu-system-riscv64` from the host package manager fails with:

```text
unsupported machine type: "k230"
```

Use the K230 QEMU fork and pinned commit used by this PR:

- repository: `https://github.com/zevorn/qemu.git`
- ref: `chao-k230-dev`
- commit: `539bd413497ccac9d3cf878036210e64830e7fd6`

Run the preparation script from the repository root inside the Docker/Linux test
environment:

```sh
bash test-suit/starryos/k230-qemu/prepare-k230-qemu.sh
```

Required build tools are `git`, `make`, `ninja` or `ninja-build`, `python3`,
`pkg-config`, and the development packages for `glib-2.0` and `pixman-1`.
On a Debian/Ubuntu based container:

```sh
apt-get update
apt-get install -y git build-essential ninja-build pkg-config python3 \
  python3-venv libglib2.0-dev libpixman-1-dev zlib1g-dev
```

The script clones the fork into `target/qemu-k230-source`, builds only the
`riscv64-softmmu` target, validates that `qemu-system-riscv64 -machine help`
lists `k230`, and leaves this layout:

```text
target/qemu-k230-docker-build/
  qemu-system-riscv64
  pc-bios/
```

If the source has already been cloned elsewhere, reuse it with:

```sh
QEMU_SOURCE_DIR=/path/to/qemu bash test-suit/starryos/k230-qemu/prepare-k230-qemu.sh
```

The K230 test configs use `target/qemu-k230-docker-build/pc-bios` for QEMU
firmware assets. Put the same directory before the default QEMU path so
`cargo xtask` picks the matching `qemu-system-riscv64` binary:

Example:

```sh
PATH="$PWD/target/qemu-k230-docker-build:$PATH" \
  cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke
```

The smoke output should include:

```text
KPU_SMOKE: opened /dev/kpu
KPU_SMOKE: info cfg=0x80400000+0x800 l2=0x80000000+0x200000 irq=189 flags=0xf
KPU_SMOKE: fake_output_zeroed
KPU_SMOKE: runtime_image file_runtime_arg_table_direct_io
KPU_SMOKE_PASS
```

NNCase runtime and real `.kmodel` demos are intentionally left to a later PR.
