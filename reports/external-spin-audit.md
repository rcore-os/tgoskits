# External `spin` crate audit

Date: 2026-05-21

This note records the current direct and indirect references to the external
`spin` crate. It is intended to support the lockdep follow-up that migrates
lockdep-relevant kernel locks away from third-party `spin::{Mutex,RwLock}` and
toward project-local primitives such as `ax_kspin`.

The migration plan based on this audit is recorded in
[`reports/external-spin-migration-plan.md`](external-spin-migration-plan.md).

## Method

Two static sources were checked:

- `Cargo.lock`, to find packages that directly or transitively depend on the
  external `spin` crate.
- `Cargo.toml` and Rust sources, to find workspace crates that explicitly
  declare or use `spin`.

No ArceOS or StarryOS build is required for this audit. Building only validates a
specific target/feature combination. For dependency reachability, `Cargo.lock`
and source scanning are broader. If a later step needs per-target/per-feature
reachability, prefer `cargo tree -e features --target ...` over full builds.

At the time of this audit, `cargo tree` is not a reliable first step in this
workspace because dependency resolution attempts to query the configured
`rsproxy-sparse` index and the local offline index does not contain the locked
`sg200x-bsp = 0.6.0` entry. Parsing `Cargo.lock` avoids that network/index
dependency.

## `Cargo.lock` entries

The workspace lockfile contains two external `spin` versions:

- `spin 0.9.8`
- `spin 0.10.0`

## Direct `Cargo.lock` dependents

These packages directly depend on external `spin` according to `Cargo.lock`:

```text
arm-scmi-rs 0.1.2 -> spin 0.10.0
arm_vcpu 0.5.8 -> spin 0.10.0
arm_vgic 0.4.9 -> spin 0.10.0
ax-driver-net 0.3.13 -> spin 0.9.8
ax-fs 0.5.13 -> spin 0.10.0
ax-fs-devfs 0.3.10 -> spin 0.9.8
ax-fs-ng 0.5.14 -> spin 0.10.0
ax-fs-ramfs 0.3.11 -> spin 0.9.8
ax-hal 0.5.14 -> spin 0.10.0
ax-net 0.5.13 -> spin 0.10.0
ax-net-ng 0.6.0 -> spin 0.10.0
ax-percpu 0.4.11 -> spin 0.10.0
ax-plat-aarch64-peripherals 0.5.9 -> spin 0.10.0
ax-posix-api 0.5.15 -> spin 0.10.0
ax-std 0.5.14 -> spin 0.10.0
ax-task 0.5.15 -> spin 0.10.0
axaddrspace 0.5.10 -> spin 0.10.0
axbacktrace 0.3.9 -> spin 0.10.0
axdevice 0.4.9 -> spin 0.10.0
axfs-ng-vfs 0.4.1 -> spin 0.10.0
axplat-dyn 0.6.1 -> spin 0.10.0
axpoll 0.3.9 -> spin 0.10.0
axvisor 0.5.7 -> spin 0.10.0
axvm 0.5.8 -> spin 0.10.0
buddy-slab-allocator 0.4.0 -> spin 0.10.0
buddy_system_allocator 0.12.0 -> spin 0.10.0
crab-usb 0.9.3 -> spin 0.10.0
dma-api 0.7.3 -> spin 0.10.0
lazy_static 1.5.0 -> spin 0.9.8
loongarch_vcpu 0.5.2 -> spin 0.10.0
nvme-driver 0.4.2 -> spin 0.10.0
ramdisk 0.1.1 -> spin 0.10.0
rdif-serial 0.7.1 -> spin 0.10.0
rdrive 0.20.1 -> spin 0.10.0
realtek-rtl8125 0.2.0 -> spin 0.10.0
riscv_vplic 0.4.11 -> spin 0.10.0
rockchip-npu 0.2.0 -> spin 0.10.0
rockchip-soc 0.2.0 -> spin 0.10.0
scope-local 0.3.7 -> spin 0.10.0
sg2002-tpu 0.1.1 -> spin 0.10.0
some-serial 0.4.1 -> spin 0.10.0
someboot 0.1.15 -> spin 0.10.0
somehal 0.6.7 -> spin 0.10.0
starry-kernel 0.5.11 -> spin 0.10.0
usb-if 0.7.1 -> spin 0.10.0
x86_vcpu 0.5.8 -> spin 0.10.0
```

Notable root closures from `Cargo.lock`:

```text
starryos 0.5.11 -> spin 0.10.0, spin 0.9.8
starry-kernel 0.5.11 -> spin 0.10.0, spin 0.9.8
ax-std 0.5.14 -> spin 0.10.0, spin 0.9.8
ax-fs-ng 0.5.14 -> spin 0.10.0, spin 0.9.8
axfs-ng-vfs 0.4.1 -> spin 0.10.0
ax-kspin 0.3.8 -> no spin in Cargo.lock closure
```

The full reverse closure from `spin 0.9.8` and `spin 0.10.0` contains 93
packages in the current lockfile. That number includes workspace packages,
test/demo packages, and third-party packages.

## Direct `Cargo.toml` declarations

Workspace manifests that declare external `spin` directly:

```text
virtualization/arm_vcpu/Cargo.toml:23: spin = "0.10"
virtualization/arm_vgic/Cargo.toml:34: spin = "0.10"
virtualization/axaddrspace/Cargo.toml:41: spin = "0.10"
components/axbacktrace/Cargo.toml:20: spin = { version = "0.10", default-features = false, features = ["once"] }
virtualization/axdevice/Cargo.toml:20: spin = "0.10"
components/axdriver_crates/axdriver_net/Cargo.toml:29: spin = "0.9"
components/axfs-ng-vfs/Cargo.toml:20: spin = { version = "0.10", default-features = false, features = ["mutex"] }
components/axfs_crates/axfs_devfs/Cargo.toml:14: spin = "0.9"
components/axfs_crates/axfs_ramfs/Cargo.toml:14: spin = "0.9"
platforms/ax-plat-aarch64-peripherals/Cargo.toml:18: spin = "0.10"
components/axpoll/Cargo.toml:22: spin = { version = "0.10", default-features = false, features = ["lazy", ...] }
virtualization/axvm/Cargo.toml:21: spin = "0.10"
virtualization/loongarch_vcpu/Cargo.toml:17: spin = "0.10"
components/percpu/percpu/Cargo.toml:41: spin = "0.10"
virtualization/riscv_vplic/Cargo.toml:24: spin = "0.10"
components/scope-local/Cargo.toml:13: spin = { version = "0.10", default-features = false, features = ["lazy"] }
components/someboot/Cargo.toml:43: spin = "0.10"
virtualization/x86_vcpu/Cargo.toml:39: spin = { version = "0.10", default-features = false }
drivers/blk/nvme-driver/Cargo.toml:18: spin = "0.10"
drivers/blk/ramdisk/Cargo.toml:15: spin = "0.10"
drivers/firmware/arm-scmi-rs/Cargo.toml:20: spin = "0.10"
drivers/interface/rdif-serial/Cargo.toml:16: spin = "0.10"
drivers/npu/rockchip-npu/Cargo.toml:21: spin = "0.10"
drivers/rdrive/Cargo.toml:18: spin = "0.10"
drivers/soc/rockchip/rockchip-soc/Cargo.toml:26: spin = "0.10"
drivers/tpu/sg2002-tpu/Cargo.toml:17: spin = "0.10"
drivers/usb/usb-host/Cargo.toml:33: spin = { version = "0.10" }
drivers/usb/usb-if/Cargo.toml:15: spin = "0.10"
os/StarryOS/kernel/Cargo.toml:117: spin = "0.10"
os/arceos/modules/axfs-ng/Cargo.toml:36: spin = { workspace = true }
os/arceos/modules/axfs/Cargo.toml:27: spin = { workspace = true }
os/arceos/modules/axnet-ng/Cargo.toml:34: spin = { workspace = true }
os/arceos/modules/axtask/Cargo.toml:70: spin = { workspace = true, optional = true }
os/axvisor/Cargo.toml:57: spin = "0.10"
platforms/axplat-dyn/Cargo.toml:84: spin = "0.10"
platforms/somehal/Cargo.toml:31: spin = "0.10"
```

The workspace root also defines:

```text
Cargo.toml:466: spin = "0.10"
```

## Source-level direct uses

Direct Rust source references were counted by primitive:

```text
Mutex direct lines: 54
RwLock direct lines: 22
Once direct lines: 20
Lazy direct lines: 8
```

Grouped by area:

```text
17 os/arceos/modules
14 os/StarryOS/kernel
 8 platforms/axplat-dyn/src
 8 drivers/usb/usb-host
 4 drivers/rdrive/src
 4 virtualization/arm_vgic/src
 3 os/axvisor/src
 3 os/arceos/api
 3 components/axfs_crates/axfs_ramfs
 2 drivers/tpu/sg2002-tpu
 2 drivers/firmware/arm-scmi-rs
 2 components/axfs_crates/axfs_devfs
 1 platforms/somehal/src
 1 drivers/soc/rockchip
 1 drivers/serial/some-serial
 1 drivers/net/realtek-rtl8125
 1 drivers/interface/rdif-serial
 1 drivers/blk/ramdisk
 1 drivers/blk/nvme-driver
 1 virtualization/x86_vcpu/src
 1 components/scope-local/src
 1 virtualization/riscv_vplic/src
 1 components/percpu/percpu
 1 virtualization/loongarch_vcpu/src
 1 components/kspin/src
 1 memory/dma-api/src
 1 virtualization/axvm/src
 1 components/axpoll/src
 1 components/axfs-ng-vfs/src
 1 components/axdriver_crates/axdriver_net
 1 virtualization/axdevice/src
 1 components/axbacktrace/src
 1 virtualization/axaddrspace/tests
```

The `components/kspin/src/base.rs` entry is documentation text referencing
`spin::Mutex`, not an actual external dependency from `ax-kspin`.

## Lockdep-relevant source uses

These are direct external `spin::{Mutex,RwLock}` uses in kernel/runtime code
that are more likely to matter for lockdep visibility:

```text
virtualization/arm_vgic/src/v3/gits.rs
virtualization/arm_vgic/src/v3/vgicd.rs
virtualization/arm_vgic/src/v3/vgicr.rs
virtualization/arm_vgic/src/vgic.rs
virtualization/axdevice/src/device.rs
components/axdriver_crates/axdriver_net/src/net_buf.rs
components/axfs-ng-vfs/src/lib.rs
components/axfs_crates/axfs_devfs/src/dir.rs
components/axfs_crates/axfs_ramfs/src/dir.rs
components/axfs_crates/axfs_ramfs/src/file.rs
virtualization/axvm/src/vm.rs
memory/dma-api/src/pool.rs
virtualization/loongarch_vcpu/src/registers.rs
virtualization/riscv_vplic/src/vplic.rs
drivers/blk/nvme-driver/src/block.rs
drivers/blk/ramdisk/src/lib.rs
drivers/firmware/arm-scmi-rs/src/lib.rs
drivers/firmware/arm-scmi-rs/src/protocol/mod.rs
drivers/interface/rdif-serial/src/serial.rs
drivers/net/realtek-rtl8125/src/lib.rs
drivers/rdrive/src/lib.rs
drivers/rdrive/src/osal.rs
drivers/rdrive/src/probe/fdt/mod.rs
drivers/rdrive/src/probe/pci/mod.rs
drivers/tpu/sg2002-tpu/src/ion/buffer.rs
drivers/tpu/sg2002-tpu/src/tpu/device.rs
drivers/usb/usb-host/src/backend/kmod/xhci/cmd.rs
drivers/usb/usb-host/src/backend/kmod/xhci/device.rs
drivers/usb/usb-host/src/backend/kmod/xhci/endpoint.rs
drivers/usb/usb-host/src/backend/kmod/xhci/host.rs
drivers/usb/usb-host/src/backend/kmod/xhci/port.rs
drivers/usb/usb-host/src/backend/kmod/xhci/reg.rs
drivers/usb/usb-host/src/backend/kmod/xhci/sync.rs
os/StarryOS/kernel/src/file/mod.rs
os/StarryOS/kernel/src/file/netlink.rs
os/StarryOS/kernel/src/file/signalfd.rs
os/StarryOS/kernel/src/pseudofs/dev/cvi_camera.rs
os/StarryOS/kernel/src/pseudofs/dev/cvi_usb_camera.rs
os/StarryOS/kernel/src/pseudofs/usbfs/manager.rs
os/StarryOS/kernel/src/pseudofs/usbfs/mod.rs
os/StarryOS/kernel/src/syscall/fs/lock.rs
os/StarryOS/kernel/src/task/mod.rs
os/StarryOS/kernel/src/task/ops.rs
os/StarryOS/kernel/src/task/posix_timer.rs
os/StarryOS/kernel/src/task/timer.rs
os/arceos/api/arceos_posix_api/src/imp/fd_ops.rs
os/arceos/api/arceos_posix_api/src/imp/pthread/mod.rs
os/arceos/modules/axfs-ng/src/highlevel/file.rs
os/arceos/modules/axfs-ng/src/highlevel/fs.rs
os/arceos/modules/axfs/src/dev.rs
os/arceos/modules/axfs/src/fs/ext4fs.rs
os/arceos/modules/axfs/src/fs/fatfs.rs
os/arceos/modules/axfs/src/root.rs
os/arceos/modules/axnet-ng/src/raw.rs
os/arceos/modules/axnet-ng/src/udp.rs
os/arceos/modules/axnet-ng/src/unix/dgram.rs
os/arceos/modules/axnet/src/smoltcp_impl/udp.rs
os/axvisor/src/hal/arch/loongarch64/mod.rs
os/axvisor/src/vmm/fdt/mod.rs
os/axvisor/src/vmm/vm_list.rs
platforms/axplat-dyn/src/drivers/blk/mod.rs
platforms/axplat-dyn/src/drivers/blk/virtio_pci.rs
platforms/axplat-dyn/src/drivers/mod.rs
platforms/axplat-dyn/src/drivers/net/virtio_pci.rs
platforms/axplat-dyn/src/drivers/pci.rs
platforms/axplat-dyn/src/drivers/soc/scmi.rs
```

## Initialization-only source uses

These direct references are `spin::Once`, `spin::once::Once`, or `spin::Lazy`.
They are usually not lockdep targets because they do not represent ordinary
runtime lock-order edges, although some may still be candidates for replacing
with a project-local initialization primitive later.

```text
components/axbacktrace/src/lib.rs
components/axfs_crates/axfs_devfs/src/lib.rs
components/axfs_crates/axfs_ramfs/src/lib.rs
components/axpoll/src/lib.rs
components/percpu/percpu/src/imp.rs
components/scope-local/src/scope.rs
drivers/rdrive/src/lib.rs
drivers/rdrive/src/probe/fdt/mod.rs
drivers/rdrive/src/probe/pci/mod.rs
os/StarryOS/kernel/src/pseudofs/dev/ion/mod.rs
os/StarryOS/kernel/src/pseudofs/dev/mod.rs
os/arceos/api/arceos_posix_api/src/imp/stdio.rs
os/arceos/modules/axfs-ng/src/highlevel/fs.rs
os/arceos/modules/axhal/src/dtb.rs
os/arceos/modules/axhal/src/lib.rs
os/arceos/modules/axhal/src/mem.rs
os/arceos/modules/axnet-ng/src/lib.rs
os/arceos/modules/axnet-ng/src/tcp.rs
os/arceos/modules/axtask/src/api.rs
platforms/axplat-dyn/src/drivers/blk/rockchip_mmc.rs
platforms/axplat-dyn/src/mem.rs
platforms/somehal/src/arch/aarch64/systick.rs
```

## Test-only source uses

These direct uses are in test utilities or tests:

```text
virtualization/axaddrspace/tests/test_utils/mod.rs
virtualization/x86_vcpu/src/test_utils.rs
drivers/serial/some-serial/tests/test.rs
drivers/soc/rockchip/rockchip-soc/tests/test.rs
```

## Migration priority

Suggested priority for eliminating lockdep-relevant blind spots:

1. `components/axfs-ng-vfs`: directly blocks FAT32/VFS lock-order visibility.
   This is the lockdep follow-up recorded in
   `os/StarryOS/kernel/src/pseudofs/lockdep-tmpfs-analysis.md`.
2. `os/arceos/modules/axfs-ng`: adjacent to the VFS/FAT/ext4 paths and already
   mixes `ax_kspin` with external `spin`.
3. `os/StarryOS/kernel`: Starry runtime locks are user-visible and participate
   in lockdep-enabled Starry debug runs.
4. `os/arceos/modules/axnet-ng`, `ax-posix-api`, and `axvisor`: runtime locks
   that may matter for broader lockdep coverage.
5. Drivers and portable component crates: migrate only after checking whether
   `ax_kspin` is an acceptable dependency boundary for each crate. Some driver
   crates may need a smaller synchronization abstraction instead of a direct
   ArceOS-specific dependency.
6. `Once`/`Lazy` users: handle separately from `Mutex`/`RwLock`. They are not
   the main lockdep visibility gap.

## Notes

- Treat external `spin::Mutex` as a busy-wait mutual-exclusion lock, not as a
  sleepable mutex. The misleading name should not push migrations toward
  `ax_sync::Mutex`; the first replacement target is normally the `ax-kspin`
  family, with any later move to a sleepable lock handled as a separate design
  change.
- Do not mechanically replace `spin::Mutex` with `ax_kspin::SpinNoPreempt`.
  Each site needs a context check: task context, IRQ context, preemption
  requirements, and whether the crate is intended to stay OS-neutral.
- Prefer `SpinNoIrq` for replacements that may be acquired from IRQ-enabled
  contexts unless the code can prove that the lock is never shared with IRQ
  handlers; `SpinNoPreempt` is only safe under that stricter condition.
- `SpinNoIrq` is not a universal repair for `SpinNoPreempt`: if a critical
  section can sleep, reschedule, fault on user memory, or call filesystem/device
  backends that can do so, the fix is to shorten the critical section or use a
  sleepable lock design.
- Current `SpinNoPreempt` follow-ups:
  - `components/axfs-ng-vfs`: the migration corrected the lock flavor to
    `SpinNoIrq`; backend callbacks under VFS spin locks are now intentionally
    left as a separately exposed follow-up issue.
  - `os/arceos/modules/axfs-ng` FAT/ext4: large filesystem locks around I/O and
    flush paths need a broader lock strategy, not a mechanical IRQ-disabling
    replacement.
  - Starry `epoll`, `pty`, and terminal metadata: short critical sections that
    can be considered for `SpinNoIrq` after wakeup/tty ordering review.
  - Starry loop-device cache: tied to the ext4 block-device path and should be
    reviewed with the axfs-ng ext4 lock strategy.
- If migrating `axfs-ng-vfs` exposes the suspected FAT32/VFS ABBA ordering, the
  fix should be a real ordering fix, not a lockdep subclass annotation.
