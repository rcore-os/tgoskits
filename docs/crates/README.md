# Crate 技术文档总览

当前仓库共识别到 **137** 个带 `[package]` 的 Rust crate。本文档索引与 `docs/crates/*.md` 一起构成按 crate 维度的技术参考集合。

当前这 **137** 份 crate 文档都已经过源码核对与手工精修，并全部加入了 `scripts/gen_crate_docs.py` 的 `CURATED_DOCS` 保留名单，用于防止后续批量生成覆盖人工内容。

如果你更关心“组件处在哪一层、如何流到 ArceOS / StarryOS / Axvisor”，建议先配合阅读 [`docs/components.md`](../components.md)。

## 分类统计

- ArceOS 层：`30` 个
- Axvisor 层：`1` 个
- StarryOS 层：`2` 个
- 工具层：`2` 个
- 平台层：`2` 个
- 测试层：`9` 个
- 组件层：`91` 个

## 文档索引

| Crate | 分类 | 路径 | 直接本地依赖 | 直接被依赖 | 文档 |
| --- | --- | --- | ---: | ---: | --- |
| `aarch64_sysreg` | 组件层 | `components/aarch64_sysreg` | 0 | 1 | [查看](./aarch64_sysreg.md) |
| `arceos-affinity` | 测试层 | `test-suit/arceos/task/affinity` | 1 | 0 | [查看](./arceos-affinity.md) |
| `arceos-helloworld` | ArceOS 层 | `os/arceos/examples/helloworld` | 1 | 0 | [查看](./arceos-helloworld.md) |
| `arceos-helloworld-myplat` | ArceOS 层 | `os/arceos/examples/helloworld-myplat` | 8 | 0 | [查看](./arceos-helloworld-myplat.md) |
| `arceos-httpclient` | ArceOS 层 | `os/arceos/examples/httpclient` | 1 | 0 | [查看](./arceos-httpclient.md) |
| `arceos-httpserver` | ArceOS 层 | `os/arceos/examples/httpserver` | 1 | 0 | [查看](./arceos-httpserver.md) |
| `arceos-irq` | 测试层 | `test-suit/arceos/task/irq` | 1 | 0 | [查看](./arceos-irq.md) |
| `arceos-memtest` | 测试层 | `test-suit/arceos/memtest` | 1 | 0 | [查看](./arceos-memtest.md) |
| `arceos-parallel` | 测试层 | `test-suit/arceos/task/parallel` | 1 | 0 | [查看](./arceos-parallel.md) |
| `arceos-priority` | 测试层 | `test-suit/arceos/task/priority` | 1 | 0 | [查看](./arceos-priority.md) |
| `arceos-shell` | ArceOS 层 | `os/arceos/examples/shell` | 1 | 0 | [查看](./arceos-shell.md) |
| `arceos-sleep` | 测试层 | `test-suit/arceos/task/sleep` | 1 | 0 | [查看](./arceos-sleep.md) |
| `arceos-wait-queue` | 测试层 | `test-suit/arceos/task/wait_queue` | 1 | 0 | [查看](./arceos-wait-queue.md) |
| `arceos-yield` | 测试层 | `test-suit/arceos/task/yield` | 1 | 0 | [查看](./arceos-yield.md) |
| `arceos_api` | ArceOS 层 | `os/arceos/api/arceos_api` | 17 | 1 | [查看](./arceos_api.md) |
| `arceos_posix_api` | ArceOS 层 | `os/arceos/api/arceos_posix_api` | 13 | 1 | [查看](./arceos_posix_api.md) |
| `arm_pl011` | 组件层 | `components/arm_pl011` | 0 | 1 | [查看](./arm_pl011.md) |
| `arm_pl031` | 组件层 | `components/arm_pl031` | 0 | 1 | [查看](./arm_pl031.md) |
| `arm_vcpu` | 组件层 | `components/arm_vcpu` | 6 | 1 | [查看](./arm_vcpu.md) |
| `arm_vgic` | 组件层 | `components/arm_vgic` | 6 | 2 | [查看](./arm_vgic.md) |
| `axaddrspace` | 组件层 | `components/axaddrspace` | 6 | 11 | [查看](./axaddrspace.md) |
| `axalloc` | ArceOS 层 | `os/arceos/modules/axalloc` | 6 | 11 | [查看](./axalloc.md) |
| `axallocator` | 组件层 | `components/axallocator` | 2 | 2 | [查看](./axallocator.md) |
| `axbacktrace` | 组件层 | `components/axbacktrace` | 0 | 5 | [查看](./axbacktrace.md) |
| `axbuild` | 工具层 | `scripts/axbuild` | 1 | 2 | [查看](./axbuild.md) |
| `axconfig` | ArceOS 层 | `os/arceos/modules/axconfig` | 1 | 12 | [查看](./axconfig.md) |
| `axconfig-gen` | 组件层 | `components/axconfig-gen/axconfig-gen` | 0 | 1 | [查看](./axconfig-gen.md) |
| `axconfig-macros` | 组件层 | `components/axconfig-gen/axconfig-macros` | 1 | 12 | [查看](./axconfig-macros.md) |
| `axcpu` | 组件层 | `components/axcpu` | 6 | 14 | [查看](./axcpu.md) |
| `axdevice` | 组件层 | `components/axdevice` | 8 | 2 | [查看](./axdevice.md) |
| `axdevice_base` | 组件层 | `components/axdevice_base` | 4 | 7 | [查看](./axdevice_base.md) |
| `axdisplay` | ArceOS 层 | `os/arceos/modules/axdisplay` | 3 | 4 | [查看](./axdisplay.md) |
| `axdma` | ArceOS 层 | `os/arceos/modules/axdma` | 7 | 2 | [查看](./axdma.md) |
| `axdriver` | ArceOS 层 | `os/arceos/modules/axdriver` | 15 | 10 | [查看](./axdriver.md) |
| `axdriver_base` | 组件层 | `components/axdriver_crates/axdriver_base` | 0 | 8 | [查看](./axdriver_base.md) |
| `axdriver_block` | 组件层 | `components/axdriver_crates/axdriver_block` | 1 | 3 | [查看](./axdriver_block.md) |
| `axdriver_display` | 组件层 | `components/axdriver_crates/axdriver_display` | 1 | 2 | [查看](./axdriver_display.md) |
| `axdriver_input` | 组件层 | `components/axdriver_crates/axdriver_input` | 1 | 2 | [查看](./axdriver_input.md) |
| `axdriver_net` | 组件层 | `components/axdriver_crates/axdriver_net` | 1 | 2 | [查看](./axdriver_net.md) |
| `axdriver_pci` | 组件层 | `components/axdriver_crates/axdriver_pci` | 0 | 1 | [查看](./axdriver_pci.md) |
| `axdriver_virtio` | 组件层 | `components/axdriver_crates/axdriver_virtio` | 6 | 2 | [查看](./axdriver_virtio.md) |
| `axdriver_vsock` | 组件层 | `components/axdriver_crates/axdriver_vsock` | 1 | 2 | [查看](./axdriver_vsock.md) |
| `axerrno` | 组件层 | `components/axerrno` | 0 | 35 | [查看](./axerrno.md) |
| `axfeat` | ArceOS 层 | `os/arceos/api/axfeat` | 16 | 7 | [查看](./axfeat.md) |
| `axfs` | ArceOS 层 | `os/arceos/modules/axfs` | 9 | 4 | [查看](./axfs.md) |
| `axfs-ng` | ArceOS 层 | `os/arceos/modules/axfs-ng` | 10 | 4 | [查看](./axfs-ng.md) |
| `axfs-ng-vfs` | 组件层 | `components/axfs-ng-vfs` | 2 | 3 | [查看](./axfs-ng-vfs.md) |
| `axfs_devfs` | 组件层 | `components/axfs_crates/axfs_devfs` | 1 | 1 | [查看](./axfs_devfs.md) |
| `axfs_ramfs` | 组件层 | `components/axfs_crates/axfs_ramfs` | 1 | 1 | [查看](./axfs_ramfs.md) |
| `axfs_vfs` | 组件层 | `components/axfs_crates/axfs_vfs` | 1 | 3 | [查看](./axfs_vfs.md) |
| `axhal` | ArceOS 层 | `os/arceos/modules/axhal` | 13 | 13 | [查看](./axhal.md) |
| `axhvc` | 组件层 | `components/axhvc` | 1 | 1 | [查看](./axhvc.md) |
| `axinput` | ArceOS 层 | `os/arceos/modules/axinput` | 3 | 3 | [查看](./axinput.md) |
| `axio` | 组件层 | `components/axio` | 1 | 9 | [查看](./axio.md) |
| `axipi` | ArceOS 层 | `os/arceos/modules/axipi` | 5 | 3 | [查看](./axipi.md) |
| `axklib` | 组件层 | `components/axklib` | 2 | 3 | [查看](./axklib.md) |
| `axlibc` | ArceOS 层 | `os/arceos/ulib/axlibc` | 4 | 0 | [查看](./axlibc.md) |
| `axlog` | ArceOS 层 | `os/arceos/modules/axlog` | 2 | 5 | [查看](./axlog.md) |
| `axmm` | ArceOS 层 | `os/arceos/modules/axmm` | 8 | 4 | [查看](./axmm.md) |
| `axnet` | ArceOS 层 | `os/arceos/modules/axnet` | 8 | 4 | [查看](./axnet.md) |
| `axnet-ng` | ArceOS 层 | `os/arceos/modules/axnet-ng` | 11 | 2 | [查看](./axnet-ng.md) |
| `axplat` | 组件层 | `components/axplat_crates/axplat` | 6 | 15 | [查看](./axplat.md) |
| `axplat-aarch64-bsta1000b` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-bsta1000b` | 6 | 1 | [查看](./axplat-aarch64-bsta1000b.md) |
| `axplat-aarch64-peripherals` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-peripherals` | 7 | 4 | [查看](./axplat-aarch64-peripherals.md) |
| `axplat-aarch64-phytium-pi` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-phytium-pi` | 5 | 1 | [查看](./axplat-aarch64-phytium-pi.md) |
| `axplat-aarch64-qemu-virt` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-qemu-virt` | 5 | 5 | [查看](./axplat-aarch64-qemu-virt.md) |
| `axplat-aarch64-raspi` | 组件层 | `components/axplat_crates/platforms/axplat-aarch64-raspi` | 5 | 1 | [查看](./axplat-aarch64-raspi.md) |
| `axplat-dyn` | 平台层 | `platform/axplat-dyn` | 11 | 2 | [查看](./axplat-dyn.md) |
| `axplat-loongarch64-qemu-virt` | 组件层 | `components/axplat_crates/platforms/axplat-loongarch64-qemu-virt` | 6 | 5 | [查看](./axplat-loongarch64-qemu-virt.md) |
| `axplat-macros` | 组件层 | `components/axplat_crates/axplat-macros` | 1 | 1 | [查看](./axplat-macros.md) |
| `axplat-riscv64-qemu-virt` | 组件层 | `components/axplat_crates/platforms/axplat-riscv64-qemu-virt` | 6 | 5 | [查看](./axplat-riscv64-qemu-virt.md) |
| `axplat-x86-pc` | 组件层 | `components/axplat_crates/platforms/axplat-x86-pc` | 7 | 5 | [查看](./axplat-x86-pc.md) |
| `axplat-x86-qemu-q35` | 平台层 | `platform/x86-qemu-q35` | 7 | 1 | [查看](./axplat-x86-qemu-q35.md) |
| `axpoll` | 组件层 | `components/axpoll` | 0 | 5 | [查看](./axpoll.md) |
| `axruntime` | ArceOS 层 | `os/arceos/modules/axruntime` | 20 | 4 | [查看](./axruntime.md) |
| `axsched` | 组件层 | `components/axsched` | 1 | 1 | [查看](./axsched.md) |
| `axstd` | ArceOS 层 | `os/arceos/ulib/axstd` | 6 | 14 | [查看](./axstd.md) |
| `axsync` | ArceOS 层 | `os/arceos/modules/axsync` | 2 | 9 | [查看](./axsync.md) |
| `axtask` | ArceOS 层 | `os/arceos/modules/axtask` | 12 | 8 | [查看](./axtask.md) |
| `axvcpu` | 组件层 | `components/axvcpu` | 5 | 5 | [查看](./axvcpu.md) |
| `axvisor` | Axvisor 层 | `os/axvisor` | 24 | 0 | [查看](./axvisor.md) |
| `axvisor_api` | 组件层 | `components/axvisor_api` | 4 | 7 | [查看](./axvisor_api.md) |
| `axvisor_api_proc` | 组件层 | `components/axvisor_api/axvisor_api_proc` | 0 | 1 | [查看](./axvisor_api_proc.md) |
| `axvm` | 组件层 | `components/axvm` | 15 | 1 | [查看](./axvm.md) |
| `axvmconfig` | 组件层 | `components/axvmconfig` | 1 | 5 | [查看](./axvmconfig.md) |
| `bitmap-allocator` | 组件层 | `components/bitmap-allocator` | 0 | 1 | [查看](./bitmap-allocator.md) |
| `bwbench-client` | ArceOS 层 | `os/arceos/tools/bwbench_client` | 0 | 0 | [查看](./bwbench-client.md) |
| `cap_access` | 组件层 | `components/cap_access` | 0 | 1 | [查看](./cap_access.md) |
| `cargo-axplat` | 组件层 | `components/axplat_crates/cargo-axplat` | 0 | 0 | [查看](./cargo-axplat.md) |
| `cpumask` | 组件层 | `components/cpumask` | 0 | 3 | [查看](./cpumask.md) |
| `crate_interface` | 组件层 | `components/crate_interface` | 0 | 19 | [查看](./crate_interface.md) |
| `crate_interface_lite` | 组件层 | `components/crate_interface/crate_interface_lite` | 0 | 0 | [查看](./crate_interface_lite.md) |
| `ctor_bare` | 组件层 | `components/ctor_bare/ctor_bare` | 1 | 1 | [查看](./ctor_bare.md) |
| `ctor_bare_macros` | 组件层 | `components/ctor_bare/ctor_bare_macros` | 0 | 1 | [查看](./ctor_bare_macros.md) |
| `define-simple-traits` | 组件层 | `components/crate_interface/test_crates/define-simple-traits` | 1 | 2 | [查看](./define-simple-traits.md) |
| `define-weak-traits` | 组件层 | `components/crate_interface/test_crates/define-weak-traits` | 1 | 4 | [查看](./define-weak-traits.md) |
| `deptool` | ArceOS 层 | `os/arceos/tools/deptool` | 0 | 0 | [查看](./deptool.md) |
| `handler_table` | 组件层 | `components/handler_table` | 0 | 1 | [查看](./handler_table.md) |
| `hello-kernel` | 组件层 | `components/axplat_crates/examples/hello-kernel` | 5 | 0 | [查看](./hello-kernel.md) |
| `impl-simple-traits` | 组件层 | `components/crate_interface/test_crates/impl-simple-traits` | 2 | 1 | [查看](./impl-simple-traits.md) |
| `impl-weak-partial` | 组件层 | `components/crate_interface/test_crates/impl-weak-partial` | 2 | 1 | [查看](./impl-weak-partial.md) |
| `impl-weak-traits` | 组件层 | `components/crate_interface/test_crates/impl-weak-traits` | 2 | 1 | [查看](./impl-weak-traits.md) |
| `int_ratio` | 组件层 | `components/int_ratio` | 0 | 3 | [查看](./int_ratio.md) |
| `irq-kernel` | 组件层 | `components/axplat_crates/examples/irq-kernel` | 7 | 0 | [查看](./irq-kernel.md) |
| `kernel_guard` | 组件层 | `components/kernel_guard` | 1 | 6 | [查看](./kernel_guard.md) |
| `kspin` | 组件层 | `components/kspin` | 1 | 21 | [查看](./kspin.md) |
| `lazyinit` | 组件层 | `components/lazyinit` | 0 | 17 | [查看](./lazyinit.md) |
| `linked_list_r4l` | 组件层 | `components/linked_list_r4l` | 0 | 1 | [查看](./linked_list_r4l.md) |
| `memory_addr` | 组件层 | `components/axmm_crates/memory_addr` | 0 | 24 | [查看](./memory_addr.md) |
| `memory_set` | 组件层 | `components/axmm_crates/memory_set` | 2 | 3 | [查看](./memory_set.md) |
| `mingo` | ArceOS 层 | `os/arceos/tools/raspi4/chainloader` | 0 | 0 | [查看](./mingo.md) |
| `page_table_entry` | 组件层 | `components/page_table_multiarch/page_table_entry` | 1 | 12 | [查看](./page_table_entry.md) |
| `page_table_multiarch` | 组件层 | `components/page_table_multiarch/page_table_multiarch` | 3 | 7 | [查看](./page_table_multiarch.md) |
| `percpu` | 组件层 | `components/percpu/percpu` | 2 | 17 | [查看](./percpu.md) |
| `percpu_macros` | 组件层 | `components/percpu/percpu_macros` | 0 | 1 | [查看](./percpu_macros.md) |
| `range-alloc-arceos` | 组件层 | `components/range-alloc-arceos` | 0 | 1 | [查看](./range-alloc-arceos.md) |
| `riscv-h` | 组件层 | `components/riscv-h` | 0 | 2 | [查看](./riscv-h.md) |
| `riscv_plic` | 组件层 | `components/riscv_plic` | 0 | 1 | [查看](./riscv_plic.md) |
| `riscv_vcpu` | 组件层 | `components/riscv_vcpu` | 8 | 1 | [查看](./riscv_vcpu.md) |
| `riscv_vplic` | 组件层 | `components/riscv_vplic` | 5 | 1 | [查看](./riscv_vplic.md) |
| `rsext4` | 组件层 | `components/rsext4` | 0 | 1 | [查看](./rsext4.md) |
| `scope-local` | 组件层 | `components/scope-local` | 1 | 3 | [查看](./scope-local.md) |
| `smoltcp` | 组件层 | `components/starry-smoltcp` | 0 | 3 | [查看](./smoltcp.md) |
| `smoltcp-fuzz` | 组件层 | `components/starry-smoltcp/fuzz` | 1 | 0 | [查看](./smoltcp-fuzz.md) |
| `smp-kernel` | 组件层 | `components/axplat_crates/examples/smp-kernel` | 9 | 0 | [查看](./smp-kernel.md) |
| `starry-kernel` | StarryOS 层 | `os/StarryOS/kernel` | 29 | 2 | [查看](./starry-kernel.md) |
| `starry-process` | 组件层 | `components/starry-process` | 2 | 1 | [查看](./starry-process.md) |
| `starry-signal` | 组件层 | `components/starry-signal` | 3 | 1 | [查看](./starry-signal.md) |
| `starry-vm` | 组件层 | `components/starry-vm` | 1 | 2 | [查看](./starry-vm.md) |
| `starryos` | StarryOS 层 | `os/StarryOS/starryos` | 2 | 0 | [查看](./starryos.md) |
| `starryos-test` | 测试层 | `test-suit/starryos` | 2 | 0 | [查看](./starryos-test.md) |
| `test-simple` | 组件层 | `components/crate_interface/test_crates/test-simple` | 3 | 0 | [查看](./test-simple.md) |
| `test-weak` | 组件层 | `components/crate_interface/test_crates/test-weak` | 3 | 0 | [查看](./test-weak.md) |
| `test-weak-partial` | 组件层 | `components/crate_interface/test_crates/test-weak-partial` | 3 | 0 | [查看](./test-weak-partial.md) |
| `tg-xtask` | 工具层 | `xtask` | 1 | 0 | [查看](./tg-xtask.md) |
| `timer_list` | 组件层 | `components/timer_list` | 0 | 1 | [查看](./timer_list.md) |
| `x86_vcpu` | 组件层 | `components/x86_vcpu` | 8 | 1 | [查看](./x86_vcpu.md) |

## 使用建议

- 若要理解系统分层，建议先阅读与自己目标系统最接近的 crate 文档，再沿“直接被依赖”列表向上追踪。
- 若要做底层修改，建议先看组件层 crate 的文档，再检查其在 ArceOS、StarryOS、Axvisor 中的跨项目定位段落。
- 本目录文档均已结合源码进行手工精修；涉及 feature 条件编译、QEMU 行为和外部镜像配置时，仍应与对应系统总文档联合阅读。
