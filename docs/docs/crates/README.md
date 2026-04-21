# Crate 技术文档总览

当前仓库共识别到 **149** 个带 `[package]` 的 Rust crate。本文档索引与 `docs/crates/*.md` 一起构成按 crate 维度的技术参考集合。

## 分类统计

- ArceOS 层：`30` 个
- Axvisor 层：`2` 个
- StarryOS 层：`2` 个
- 其他：`1` 个
- 工具层：`2` 个
- 平台层：`2` 个
- 测试层：`17` 个
- 组件层：`93` 个

## 文档索引

| Crate | 分类 | 路径 | 直接本地依赖 | 直接被依赖 | 文档 |
| --- | --- | --- | ---: | ---: | --- |
| `aarch64_sysreg` | 组件层 | `components/aarch64_sysreg` | 0 | 1 | [查看](./aarch64_sysreg.md) |
| `arceos-affinity` | 测试层 | `test-suit/arceos/rust/task/affinity` | 1 | 0 | [查看](./arceos-affinity.md) |
| `arceos-display` | 测试层 | `test-suit/arceos/rust/display` | 1 | 0 | [查看](./arceos-display.md) |
| `arceos-exception` | 测试层 | `test-suit/arceos/rust/exception` | 1 | 0 | [查看](./arceos-exception.md) |
| `arceos-fs-shell` | 测试层 | `test-suit/arceos/rust/fs/shell` | 4 | 0 | [查看](./arceos-fs-shell.md) |
| `arceos-irq` | 测试层 | `test-suit/arceos/rust/task/irq` | 1 | 0 | [查看](./arceos-irq.md) |
| `arceos-memtest` | 测试层 | `test-suit/arceos/rust/memtest` | 1 | 0 | [查看](./arceos-memtest.md) |
| `arceos-net-echoserver` | 测试层 | `test-suit/arceos/rust/net/echoserver` | 1 | 0 | [查看](./arceos-net-echoserver.md) |
| `arceos-net-httpclient` | 测试层 | `test-suit/arceos/rust/net/httpclient` | 1 | 0 | [查看](./arceos-net-httpclient.md) |
| `arceos-net-httpserver` | 测试层 | `test-suit/arceos/rust/net/httpserver` | 1 | 0 | [查看](./arceos-net-httpserver.md) |
| `arceos-net-udpserver` | 测试层 | `test-suit/arceos/rust/net/udpserver` | 1 | 0 | [查看](./arceos-net-udpserver.md) |
| `arceos-parallel` | 测试层 | `test-suit/arceos/rust/task/parallel` | 1 | 0 | [查看](./arceos-parallel.md) |
| `arceos-priority` | 测试层 | `test-suit/arceos/rust/task/priority` | 1 | 0 | [查看](./arceos-priority.md) |
| `arceos-sleep` | 测试层 | `test-suit/arceos/rust/task/sleep` | 1 | 0 | [查看](./arceos-sleep.md) |
| `arceos-tls` | 测试层 | `test-suit/arceos/rust/task/tls` | 1 | 0 | [查看](./arceos-tls.md) |
| `arceos-wait-queue` | 测试层 | `test-suit/arceos/rust/task/wait_queue` | 1 | 0 | [查看](./arceos-wait-queue.md) |
| `arceos-yield` | 测试层 | `test-suit/arceos/rust/task/yield` | 1 | 0 | [查看](./arceos-yield.md) |
| `arm_vcpu` | 组件层 | `components/arm_vcpu` | 6 | 1 | [查看](./arm_vcpu.md) |
| `arm_vgic` | 组件层 | `components/arm_vgic` | 6 | 2 | [查看](./arm_vgic.md) |
| `ax-alloc` | ArceOS 层 | `os/arceos/modules/axalloc` | 6 | 11 | [查看](./ax-alloc.md) |
| `ax-allocator` | 组件层 | `components/axallocator` | 2 | 2 | [查看](./ax-allocator.md) |
| `ax-api` | ArceOS 层 | `os/arceos/api/arceos_api` | 17 | 1 | [查看](./ax-api.md) |
| `ax-arm-pl011` | 组件层 | `components/arm_pl011` | 0 | 1 | [查看](./ax-arm-pl011.md) |
| `ax-arm-pl031` | 组件层 | `components/arm_pl031` | 0 | 1 | [查看](./ax-arm-pl031.md) |
| `ax-cap-access` | 组件层 | `components/cap_access` | 0 | 1 | [查看](./ax-cap-access.md) |
| `ax-config` | ArceOS 层 | `os/arceos/modules/axconfig` | 1 | 12 | [查看](./ax-config.md) |
| `ax-config-gen` | 组件层 | `components/axconfig-gen/axconfig-gen` | 0 | 1 | [查看](./ax-config-gen.md) |
| `ax-config-macros` | 组件层 | `components/axconfig-gen/axconfig-macros` | 1 | 12 | [查看](./ax-config-macros.md) |
| `ax-cpu` | 组件层 | `components/axcpu` | 6 | 14 | [查看](./ax-cpu.md) |
| `ax-cpumask` | 组件层 | `components/cpumask` | 0 | 4 | [查看](./ax-cpumask.md) |
| `ax-crate-interface` | 组件层 | `components/crate_interface` | 0 | 22 | [查看](./ax-crate-interface.md) |
| `ax-crate-interface-lite` | 组件层 | `components/crate_interface/crate_interface_lite` | 0 | 0 | [查看](./ax-crate-interface-lite.md) |
| `ax-ctor-bare` | 组件层 | `components/ctor_bare/ctor_bare` | 1 | 1 | [查看](./ax-ctor-bare.md) |
| `ax-ctor-bare-macros` | 组件层 | `components/ctor_bare/ctor_bare_macros` | 0 | 1 | [查看](./ax-ctor-bare-macros.md) |
| `ax-display` | ArceOS 层 | `os/arceos/modules/axdisplay` | 3 | 4 | [查看](./ax-display.md) |
| `ax-dma` | ArceOS 层 | `os/arceos/modules/axdma` | 7 | 2 | [查看](./ax-dma.md) |
| `ax-driver` | ArceOS 层 | `os/arceos/modules/axdriver` | 15 | 10 | [查看](./ax-driver.md) |
| `ax-driver-base` | 组件层 | `components/axdriver_crates/axdriver_base` | 0 | 8 | [查看](./ax-driver-base.md) |
| `ax-driver-block` | 组件层 | `components/axdriver_crates/axdriver_block` | 1 | 3 | [查看](./ax-driver-block.md) |
| `ax-driver-display` | 组件层 | `components/axdriver_crates/axdriver_display` | 1 | 2 | [查看](./ax-driver-display.md) |
| `ax-driver-input` | 组件层 | `components/axdriver_crates/axdriver_input` | 1 | 2 | [查看](./ax-driver-input.md) |
| `ax-driver-net` | 组件层 | `components/axdriver_crates/axdriver_net` | 2 | 2 | [查看](./ax-driver-net.md) |
| `ax-driver-pci` | 组件层 | `components/axdriver_crates/axdriver_pci` | 0 | 1 | [查看](./ax-driver-pci.md) |
| `ax-driver-virtio` | 组件层 | `components/axdriver_crates/axdriver_virtio` | 6 | 2 | [查看](./ax-driver-virtio.md) |
| `ax-driver-vsock` | 组件层 | `components/axdriver_crates/axdriver_vsock` | 1 | 2 | [查看](./ax-driver-vsock.md) |
| `ax-errno` | 组件层 | `components/axerrno` | 0 | 36 | [查看](./ax-errno.md) |
| `ax-feat` | ArceOS 层 | `os/arceos/api/axfeat` | 16 | 7 | [查看](./ax-feat.md) |
| `ax-fs` | ArceOS 层 | `os/arceos/modules/axfs` | 10 | 4 | [查看](./ax-fs.md) |
| `ax-fs-devfs` | 组件层 | `components/axfs_crates/axfs_devfs` | 1 | 1 | [查看](./ax-fs-devfs.md) |
| `ax-fs-ng` | ArceOS 层 | `os/arceos/modules/axfs-ng` | 10 | 4 | [查看](./ax-fs-ng.md) |
| `ax-fs-ramfs` | 组件层 | `components/axfs_crates/axfs_ramfs` | 1 | 2 | [查看](./ax-fs-ramfs.md) |
| `ax-fs-vfs` | 组件层 | `components/axfs_crates/axfs_vfs` | 1 | 4 | [查看](./ax-fs-vfs.md) |
| `ax-hal` | ArceOS 层 | `os/arceos/modules/axhal` | 13 | 15 | [查看](./ax-hal.md) |
| `ax-handler-table` | 组件层 | `components/handler_table` | 0 | 1 | [查看](./ax-handler-table.md) |
| `ax-helloworld` | ArceOS 层 | `os/arceos/examples/helloworld` | 1 | 0 | [查看](./ax-helloworld.md) |
| `ax-helloworld-myplat` | ArceOS 层 | `os/arceos/examples/helloworld-myplat` | 8 | 0 | [查看](./ax-helloworld-myplat.md) |
| `ax-httpclient` | ArceOS 层 | `os/arceos/examples/httpclient` | 1 | 0 | [查看](./ax-httpclient.md) |
| `ax-httpserver` | ArceOS 层 | `os/arceos/examples/httpserver` | 1 | 0 | [查看](./ax-httpserver.md) |
| `ax-input` | ArceOS 层 | `os/arceos/modules/axinput` | 3 | 3 | [查看](./ax-input.md) |
| `ax-int-ratio` | 组件层 | `components/int_ratio` | 0 | 3 | [查看](./ax-int-ratio.md) |
| `ax-io` | 组件层 | `components/axio` | 1 | 9 | [查看](./ax-io.md) |
| `ax-ipi` | ArceOS 层 | `os/arceos/modules/axipi` | 5 | 3 | [查看](./ax-ipi.md) |
| `ax-kernel-guard` | 组件层 | `components/kernel_guard` | 1 | 6 | [查看](./kernel_guard.md) |
| `ax-kspin` | 组件层 | `components/kspin` | 1 | 21 | [查看](./ax-kspin.md) |
| `ax-lazyinit` | 组件层 | `components/ax-lazyinit` | 0 | 17 | [查看](./ax-lazyinit.md) |
| `ax-libc` | ArceOS 层 | `os/arceos/ulib/axlibc` | 4 | 0 | [查看](./ax-libc.md) |
| `ax-linked-list-r4l` | 组件层 | `components/linked_list_r4l` | 0 | 1 | [查看](./ax-linked-list-r4l.md) |
| `ax-log` | ArceOS 层 | `os/arceos/modules/axlog` | 2 | 5 | [查看](./ax-log.md) |
| `ax-memory-addr` | 组件层 | `components/axmm_crates/memory_addr` | 0 | 24 | [查看](./ax-memory-addr.md) |
| `ax-memory-set` | 组件层 | `components/axmm_crates/memory_set` | 2 | 3 | [查看](./ax-memory-set.md) |
| `ax-mm` | ArceOS 层 | `os/arceos/modules/axmm` | 8 | 4 | [查看](./ax-mm.md) |
| `ax-net` | ArceOS 层 | `os/arceos/modules/axnet` | 8 | 4 | [查看](./ax-net.md) |
| `ax-net-ng` | ArceOS 层 | `os/arceos/modules/axnet-ng` | 11 | 2 | [查看](./ax-net-ng.md) |
| `ax-page-table-entry` | 组件层 | `components/page_table_multiarch/page_table_entry` | 1 | 12 | [查看](./ax-page-table-entry.md) |
| `ax-page-table-multiarch` | 组件层 | `components/page_table_multiarch/page_table_multiarch` | 3 | 7 | [查看](./ax-page-table-multiarch.md) |
| `ax-percpu` | 组件层 | `components/percpu/percpu` | 2 | 17 | [查看](./ax-percpu.md) |
| `ax-percpu-macros` | 组件层 | `components/percpu/percpu_macros` | 0 | 1 | [查看](./ax-percpu-macros.md) |
| `ax-plat` | 组件层 | `components/axplat_crates/axplat` | 6 | 15 | [查看](./ax-plat.md) |
| `ax-plat-aarch64-bsta1000b` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-bsta1000b` | 6 | 1 | [查看](./ax-plat-aarch64-bsta1000b.md) |
| `ax-plat-aarch64-peripherals` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-peripherals` | 7 | 4 | [查看](./ax-plat-aarch64-peripherals.md) |
| `ax-plat-aarch64-phytium-pi` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-phytium-pi` | 5 | 1 | [查看](./ax-plat-aarch64-phytium-pi.md) |
| `ax-plat-aarch64-qemu-virt` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-qemu-virt` | 5 | 5 | [查看](./ax-plat-aarch64-qemu-virt.md) |
| `ax-plat-aarch64-raspi` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-raspi` | 5 | 1 | [查看](./ax-plat-aarch64-raspi.md) |
| `ax-plat-loongarch64-qemu-virt` | 组件层 | `components/axplat_crates/platforms/axplat-loongarch64-qemu-virt` | 6 | 5 | [查看](./ax-plat-loongarch64-qemu-virt.md) |
| `ax-plat-macros` | 组件层 | `components/axplat_crates/axplat-macros` | 1 | 1 | [查看](./ax-plat-macros.md) |
| `ax-plat-riscv64-qemu-virt` | 组件层 | `components/axplat_crates/platforms/axplat-riscv64-qemu-virt` | 6 | 6 | [查看](./ax-plat-riscv64-qemu-virt.md) |
| `axplat-riscv64-qemu-virt-hv` | Axvisor 层 | `platform/riscv64-qemu-virt` | 8 | 6 | [查看](./ax-plat-riscv64-qemu-virt.md) |
| `ax-plat-x86-pc` | 组件层 | `components/axplat_crates/platforms/axplat-x86-pc` | 7 | 5 | [查看](./ax-plat-x86-pc.md) |
| `ax-posix-api` | ArceOS 层 | `os/arceos/api/arceos_posix_api` | 13 | 1 | [查看](./ax-posix-api.md) |
| `ax-runtime` | ArceOS 层 | `os/arceos/modules/axruntime` | 20 | 4 | [查看](./ax-runtime.md) |
| `ax-sched` | 组件层 | `components/axsched` | 1 | 1 | [查看](./ax-sched.md) |
| `ax-shell` | ArceOS 层 | `os/arceos/examples/shell` | 1 | 0 | [查看](./ax-shell.md) |
| `ax-std` | ArceOS 层 | `os/arceos/ulib/axstd` | 6 | 22 | [查看](./ax-std.md) |
| `ax-sync` | ArceOS 层 | `os/arceos/modules/axsync` | 2 | 9 | [查看](./ax-sync.md) |
| `ax-task` | ArceOS 层 | `os/arceos/modules/axtask` | 13 | 8 | [查看](./ax-task.md) |
| `axaddrspace` | 组件层 | `components/axaddrspace` | 6 | 12 | [查看](./axaddrspace.md) |
| `axbacktrace` | 组件层 | `components/axbacktrace` | 0 | 5 | [查看](./axbacktrace.md) |
| `axbuild` | 工具层 | `scripts/axbuild` | 1 | 3 | [查看](./axbuild.md) |
| `axdevice` | 组件层 | `components/axdevice` | 8 | 2 | [查看](./axdevice.md) |
| `axdevice_base` | 组件层 | `components/axdevice_base` | 3 | 8 | [查看](./axdevice_base.md) |
| `axfs-ng-vfs` | 组件层 | `components/axfs-ng-vfs` | 2 | 3 | [查看](./axfs-ng-vfs.md) |
| `axhvc` | 组件层 | `components/axhvc` | 1 | 1 | [查看](./axhvc.md) |
| `axklib` | 组件层 | `components/axklib` | 2 | 3 | [查看](./axklib.md) |
| `axplat-dyn` | 平台层 | `platform/axplat-dyn` | 11 | 2 | [查看](./axplat-dyn.md) |
| `axplat-x86-qemu-q35` | 平台层 | `platform/x86-qemu-q35` | 7 | 1 | [查看](./axplat-x86-qemu-q35.md) |
| `axpoll` | 组件层 | `components/axpoll` | 0 | 5 | [查看](./axpoll.md) |
| `axvcpu` | 组件层 | `components/axvcpu` | 5 | 5 | [查看](./axvcpu.md) |
| `axvisor` | Axvisor 层 | `os/axvisor` | 27 | 0 | [查看](./axvisor.md) |
| `axvisor_api` | 组件层 | `components/axvisor_api` | 5 | 10 | [查看](./axvisor_api.md) |
| `axvisor_api_proc` | 组件层 | `components/axvisor_api/axvisor_api_proc` | 0 | 1 | [查看](./axvisor_api_proc.md) |
| `axvm` | 组件层 | `components/axvm` | 16 | 1 | [查看](./axvm.md) |
| `axvmconfig` | 组件层 | `components/axvmconfig` | 1 | 4 | [查看](./axvmconfig.md) |
| `bitmap-allocator` | 组件层 | `components/bitmap-allocator` | 0 | 1 | [查看](./bitmap-allocator.md) |
| `bwbench-client` | ArceOS 层 | `os/arceos/tools/bwbench_client` | 0 | 0 | [查看](./bwbench-client.md) |
| `cargo-axplat` | 组件层 | `components/axplat_crates/cargo-axplat` | 0 | 0 | [查看](./cargo-axplat.md) |
| `define-simple-traits` | 组件层 | `components/crate_interface/test_crates/define-simple-traits` | 1 | 2 | [查看](./define-simple-traits.md) |
| `define-weak-traits` | 组件层 | `components/crate_interface/test_crates/define-weak-traits` | 1 | 4 | [查看](./define-weak-traits.md) |
| `deptool` | ArceOS 层 | `os/arceos/tools/deptool` | 0 | 0 | [查看](./deptool.md) |
| `fxmac_rs` | 组件层 | `components/fxmac_rs` | 1 | 1 | [查看](./fxmac_rs.md) |
| `hello-kernel` | 组件层 | `components/axplat_crates/examples/hello-kernel` | 5 | 0 | [查看](./hello-kernel.md) |
| `impl-simple-traits` | 组件层 | `components/crate_interface/test_crates/impl-simple-traits` | 2 | 1 | [查看](./impl-simple-traits.md) |
| `impl-weak-partial` | 组件层 | `components/crate_interface/test_crates/impl-weak-partial` | 2 | 1 | [查看](./impl-weak-partial.md) |
| `impl-weak-traits` | 组件层 | `components/crate_interface/test_crates/impl-weak-traits` | 2 | 1 | [查看](./impl-weak-traits.md) |
| `irq-kernel` | 组件层 | `components/axplat_crates/examples/irq-kernel` | 7 | 0 | [查看](./irq-kernel.md) |
| `mingo` | ArceOS 层 | `os/arceos/tools/raspi4/chainloader` | 0 | 0 | [查看](./mingo.md) |
| `range-alloc-arceos` | 组件层 | `components/range-alloc-arceos` | 0 | 1 | [查看](./range-alloc-arceos.md) |
| `riscv-h` | 组件层 | `components/riscv-h` | 0 | 2 | [查看](./riscv-h.md) |
| `ax-riscv-plic` | 组件层 | `components/riscv_plic` | 0 | 1 | [查看](./ax-riscv-plic.md) |
| `riscv_vcpu` | 组件层 | `components/riscv_vcpu` | 8 | 2 | [查看](./riscv_vcpu.md) |
| `riscv_vplic` | 组件层 | `components/riscv_vplic` | 5 | 2 | [查看](./riscv_vplic.md) |
| `rsext4` | 组件层 | `components/rsext4` | 0 | 1 | [查看](./rsext4.md) |
| `scope-local` | 组件层 | `components/scope-local` | 1 | 3 | [查看](./scope-local.md) |
| `smoltcp` | 组件层 | `components/starry-smoltcp` | 0 | 3 | [查看](./smoltcp.md) |
| `smoltcp-fuzz` | 组件层 | `components/starry-smoltcp/fuzz` | 1 | 0 | [查看](./smoltcp-fuzz.md) |
| `smp-kernel` | 组件层 | `components/axplat_crates/examples/smp-kernel` | 9 | 0 | [查看](./smp-kernel.md) |
| `starry-kernel` | StarryOS 层 | `os/StarryOS/kernel` | 29 | 2 | [查看](./starry-kernel.md) |
| `starry-process` | 组件层 | `components/starry-process` | 2 | 1 | [查看](./starry-process.md) |
| `starry-signal` | 组件层 | `components/starry-signal` | 3 | 1 | [查看](./starry-signal.md) |
| `starry-vm` | 组件层 | `components/starry-vm` | 1 | 2 | [查看](./starry-vm.md) |
| `starryos` | StarryOS 层 | `os/StarryOS/starryos` | 3 | 0 | [查看](./starryos.md) |
| `starryos-test` | 测试层 | `test-suit/starryos` | 2 | 0 | [查看](./starryos-test.md) |
| `test-simple` | 组件层 | `components/crate_interface/test_crates/test-simple` | 3 | 0 | [查看](./test-simple.md) |
| `test-weak` | 组件层 | `components/crate_interface/test_crates/test-weak` | 3 | 0 | [查看](./test-weak.md) |
| `test-weak-partial` | 组件层 | `components/crate_interface/test_crates/test-weak-partial` | 3 | 0 | [查看](./test-weak-partial.md) |
| `tg-xtask` | 工具层 | `xtask` | 1 | 0 | [查看](./tg-xtask.md) |
| `tgmath` | 其他 | `examples/tgmath` | 0 | 0 | [查看](./tgmath.md) |
| `ax-timer-list` | 组件层 | `components/timer_list` | 0 | 2 | [查看](./ax-timer-list.md) |
| `x86_vcpu` | 组件层 | `components/x86_vcpu` | 9 | 1 | [查看](./x86_vcpu.md) |
| `x86_vlapic` | 组件层 | `components/x86_vlapic` | 5 | 1 | [查看](./x86_vlapic.md) |

## 使用建议

- 若要理解系统分层，建议先阅读与自己目标系统最接近的 crate 文档，再沿“直接被依赖”列表向上追踪。
- 若要做底层修改，建议先看组件层 crate 的文档，再检查其在 ArceOS、StarryOS、Axvisor 中的跨项目定位段落。
- 本目录文档依据源码静态分析自动整理；涉及 feature 条件编译、QEMU 行为和外部镜像配置时，应与对应系统总文档联合阅读。
