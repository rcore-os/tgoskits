# Starry K230 KPU NNCase App

This Starry app is the operator-facing K230 KPU/NPU demo. It runs the same
runtime path as the `kpu-nncase-runtime` test case:

```text
yolov8n_320.kmodel
  -> K230 SDK NNCase runtime in StarryOS userspace
  -> KPU command stream
  -> /dev/kpu ioctl/mmap
  -> QEMU K230 KPU model
  -> done/IRQ and output tensor hashes
```

The lower-level device regression remains under `test-suit/starryos`; the demo
application, build helper, and teacher script live here under `apps/starry`.

## Local Assets

This app does not commit the real model, image, K230 SDK static libraries, or
prebuilt guest binaries. Prepare the official SDK assets under:

```text
target/official-k230/k230-sdk-src/
```

See `docs/docs/architecture/driver/k230-kpu-nncase-runtime.md` for the full SDK preparation flow.

Build the ignored StarryOS guest binaries with:

```sh
bash apps/starry/k230-kpu-nncase/c/tools/build-nncase-runtime-binaries.sh
```

The script writes:

```text
apps/starry/k230-kpu-nncase/c/assets/bin/
  kpu-nncase-minimal
  k230-yolov8n-demo
```

## Run As A Starry App

Prepare the K230 QEMU fork first:

```sh
bash apps/starry/k230-qemu/prepare-k230-qemu.sh
```

Then run:

```sh
PATH="$PWD/target/qemu-k230-docker-build:$PATH" \
  cargo xtask starry app qemu -t k230-kpu-nncase --arch riscv64
```

Expected success markers:

```text
NNCASE_MINIMAL_PASS
YOLOV8N_DEMO_PASS
K230_NNCASE_RUNTIME_PASS
```

## Teacher Demo

For a terminal-friendly demonstration with streamed logs and a short evidence
summary, run:

```sh
bash apps/starry/k230-kpu-nncase/demo-teacher.sh
```

The K230 app-QEMU demo wrapper uses the migrated K230 cases:

```sh
bash apps/starry/k230-qemu/qemu-k230/demo-teacher.sh
```
