# K230 KPU real model assets

This directory is for local real `.kmodel` files used by the K230 KPU smoke
test. Model binaries are intentionally ignored because the official packages are
large.

Install the default official YOLOv8n model into this directory with:

```sh
bash test-suit/starryos/k230-qemu/qemu-k230/kpu-smoke/c/tools/prepare-real-kmodel.sh
```

The smoke test treats the model as an optional asset. When
`yolov8n_320.kmodel` is present at build time, CMake installs it into the guest
rootfs and the test verifies the `LDMK` header, size, version, and content hash.
When the file is absent, the rest of the KPU device/runtime checks still run.

Runtime captures that produce large model outputs should prefer `.krun`
`check_hash` records instead of embedding full output tensors as byte lists.
