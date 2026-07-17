# Axvisor IVC Linux Guest Support

This directory contains the Linux-side user-space pieces used by the Axvisor
IVC QEMU test:

- `include/`: shared ioctl and user library headers.
- `lib/`: small userspace wrapper over the IVC device ioctls.
- `publisher/`: Linux publisher program for Linux-to-ArceOS tests.
- `subscriber/`: Linux subscriber program used by the ArceOS-to-Linux test.

The Linux kernel module that exposes `/dev/axivc` is not kept in tgoskits. It
is built by tgosimages together with the target Linux kernel and installed into
the rootfs as `/root/axvisor.ko`.

Build the test payloads with:

```bash
AXVISOR_IVC_ARCH=aarch64 \
AXVISOR_IVC_OUT_DIR=/path/to/out \
apps/linux/ivc/build.sh
```

The output directory contains:

```text
ivc-publish
ivc-subscribe
```

`cargo xtask axvisor test qemu --arch aarch64 --test-case ivc` builds these
payloads as part of the test and injects them into the selected Linux rootfs.
