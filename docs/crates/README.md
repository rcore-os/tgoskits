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

## 手工精修批次

下表按实际执行时采用的 **15 批次口径** 汇总了这轮全量手工精修的顺序、每批覆盖的 crate，以及为什么要这样分组。

说明：

- 前 5 批属于对前期连续精修工作的规划口径归并。
- 其中第 4、5 批在实际落地时各自包含连续子轮，但这里统一按 15 批总表呈现。

| 批次 | 主题 | 数量 | Crates 列表 | 为什么这样分组 |
| --- | --- | ---: | --- | --- |
| 1 | 核心主干第一批 | 5 | `axhal`、`axtask`、`axvm`、`starry-kernel`、`axvisor` | 这是三套系统最上层、最能定义全局叙事的“总脊柱”文档。先把 HAL、任务调度、VM 生命周期、Starry 主内核和 Axvisor 主运行时写稳，后面所有文档才有统一参照系。 |
| 2 | 核心主干第二批 | 5 | `axruntime`、`axmm`、`axdriver`、`arceos_api`、`axsync` | 这批是第 1 批的直接支撑层，分别对应运行时装配、内存管理、驱动聚合、应用 API 出口和同步原语。把它们紧跟在主干后面，可以尽早稳定“运行时主链”的术语。 |
| 3 | 平台/架构基础第一批 | 6 | `axplat-riscv64-qemu-virt`、`axplat-x86-pc`、`axplat-macros`、`arm_vcpu`、`arm_vgic`、`arm_pl031` | 这一批专门处理“平台 bring-up + ARM 虚拟化 + 宏契约”三类高耦合基础件。它们既连接平台抽象，也连接后面的虚拟化主线，所以必须尽早统一边界。 |
| 4 | 平台抽象与虚拟化主链 | 12 | `axplat`、`axplat-aarch64-peripherals`、`axplat-aarch64-qemu-virt`、`axdevice`、`riscv_vcpu`、`riscv_vplic`、`axvmconfig`、`axaddrspace`、`axdevice_base`、`axvcpu`、`axvisor_api`、`page_table_multiarch` | 这一批的共同点是都位于“平台契约 / 虚拟化公共主链”的中心位置。它们共同定义了平台接口、vCPU 接口、设备接口、VM 配置和页表引擎，不连续写就很容易出现术语漂移。 |
| 5 | 页表/地址与 per-CPU/接口基础设施 | 12 | `page_table_entry`、`memory_addr`、`memory_set`、`x86_vcpu`、`riscv-h`、`percpu`、`percpu_macros`、`axcpu`、`crate_interface`、`riscv_plic`、`kernel_guard`、`scope-local` | 这批都是“横向复用的低层基础件”，共同特点是定义抽象或运行时语义，而不是做上层系统装配。它们必须集中处理，才能统一“地址、页表、per-CPU、接口绑定、临界区、局部状态”这些公共概念。 |
| 6 | 平台剩余与板级变体 | 7 | `axplat-aarch64-bsta1000b`、`axplat-aarch64-phytium-pi`、`axplat-aarch64-raspi`、`axplat-loongarch64-qemu-virt`、`axplat-x86-qemu-q35`、`axplat-dyn`、`cargo-axplat` | 前面先写了平台抽象和主流板级实现，这一批才补其余板级变体和平台接入工具。这样可以避免每份平台文档都重新定义一次 `axplat` 概念。 |
| 7 | 驱动子工作区与设备类别 | 11 | `axdriver_base`、`axdriver_block`、`axdriver_display`、`axdriver_input`、`axdriver_net`、`axdriver_pci`、`axdriver_virtio`、`axdriver_vsock`、`axdisplay`、`axinput`、`axdma` | 这批都围绕“设备类别契约、总线适配、设备聚合到模块层”的同一主题展开。集中处理能把“驱动叶子层”“驱动聚合层”“用户可见能力层”之间的边界一次写清。 |
| 8 | 文件系统与 VFS | 7 | `axfs`、`axfs-ng`、`axfs-ng-vfs`、`axfs_vfs`、`axfs_devfs`、`axfs_ramfs`、`rsext4` | 这些 crate 形成了最典型的纵向文件系统链：旧栈聚合、新栈聚合、旧/新 VFS、具体 FS 实现和 ext4 引擎。必须放在同一批里，才能把新旧两套栈的差异写明白。 |
| 9 | 网络、I/O 与轮询 | 9 | `axio`、`axpoll`、`axnet`、`axnet-ng`、`smoltcp`、`smoltcp-fuzz`、`arceos-httpclient`、`arceos-httpserver`、`bwbench-client` | 这批的共同点是都围绕“同步 I/O 语义、就绪模型、协议栈与上层示例程序”展开。集中处理能明确 `axio`/`axpoll`、`smoltcp`、`axnet`/`axnet-ng` 和示例程序各自所处层次。 |
| 10 | 运行时叶子基础件 | 15 | `axalloc`、`axallocator`、`axbacktrace`、`axerrno`、`axlog`、`axipi`、`axsched`、`axklib`、`kspin`、`cpumask`、`handler_table`、`int_ratio`、`lazyinit`、`linked_list_r4l`、`timer_list` | 这些 crate 复用度极高，但都属于“窄职责叶子件”。放在主链完成后统一整理，可以把它们准确写成基础件，而不是误写成内存、调度、同步或中断主系统。 |
| 11 | 架构周边与元编程辅助 | 10 | `aarch64_sysreg`、`arm_pl011`、`axhvc`、`axvisor_api_proc`、`crate_interface_lite`、`ctor_bare`、`ctor_bare_macros`、`cap_access`、`bitmap-allocator`、`range-alloc-arceos` | 这批大多是“支持主链但不构成主链”的组件：寄存器编码、单设备叶子、ABI 编号、过程宏、能力位、分配算法。集中处理可以统一强调“辅助件”定位。 |
| 12 | 配置、API、构建链与用户态封装 | 10 | `axfeat`、`axconfig`、`axconfig-gen`、`axconfig-macros`、`axstd`、`axlibc`、`arceos_posix_api`、`axbuild`、`tg-xtask`、`deptool` | 这一批都位于“编译期装配 / 用户态接口 / 宿主构建工具”交界处。必须放在一起，才能把构建期和运行期的职责严格分开。 |
| 13 | Starry 扩展栈 | 5 | `starry-process`、`starry-signal`、`starry-vm`、`starryos`、`starryos-test` | `starry-kernel` 已在前面先立住主线，这一批就专注补齐 Starry 的进程关系、信号语义、用户虚拟内存访问、启动包和测试入口，形成完整 Starry 叙事。 |
| 14 | ArceOS 示例与系统行为样例 | 14 | `arceos-affinity`、`arceos-helloworld`、`arceos-helloworld-myplat`、`arceos-irq`、`arceos-memtest`、`arceos-parallel`、`arceos-priority`、`arceos-shell`、`arceos-sleep`、`arceos-wait-queue`、`arceos-yield`、`hello-kernel`、`irq-kernel`、`smp-kernel` | 这些都不是复用库，而是“能力链验证样例”。放到靠后位置，可以直接把它们写成对前面系统能力的演示和 smoke test，而不是主功能组件。 |
| 15 | 接口测试桩与剩余实验件 | 9 | `define-simple-traits`、`define-weak-traits`、`impl-simple-traits`、`impl-weak-traits`、`impl-weak-partial`、`test-simple`、`test-weak`、`test-weak-partial`、`mingo` | 最后一批都是非主线运行时资产：`crate_interface` 的测试矩阵和一个特殊实验/工具型二进制 `mingo`。把它们放最后，能避免它们干扰前面的系统主线叙事。 |

## 批次与三大系统子系统对照

下表从系统视角补充说明每一批文档主要影响或覆盖到的 ArceOS、StarryOS、Axvisor 子系统。

| 批次 | ArceOS 主要影响子系统 | StarryOS 主要影响子系统 | Axvisor 主要影响子系统 |
| --- | --- | --- | --- |
| 1 | `axhal`、`axtask` 所在的 HAL、任务调度、等待/唤醒主链 | `starry-kernel` 主内核骨架，以及复用的 HAL/任务调度链 | `axvisor` 主运行时、`axvm` VM 生命周期主线 |
| 2 | `axruntime` 启动链、`axmm` 内存管理、`axdriver` 驱动聚合、`arceos_api` 应用接口、`axsync` 同步层 | 通过复用 `axmm`、`axsync`、`arceos_api` 等公共层间接受影响 | 共享的内存/驱动/同步基础层，以及 Host 侧运行时公共能力 |
| 3 | RISC-V/x86 平台 bring-up、AArch64 RTC 支撑 | 复用 `axplat` 平台包时的启动链和部分 AArch64 平台语义 | ARM vCPU、虚拟 GIC、宿主平台 bring-up 边界 |
| 4 | `axplat` 主契约、AArch64 QEMU virt 平台、部分公共页表/平台接口 | 复用 `axplat` 与 `page_table_multiarch` 时的平台/页表公共语义 | `axvcpu`、`axvisor_api`、`axdevice`、`axaddrspace`、`axvmconfig`、`riscv_vcpu`、`riscv_vplic` 等虚拟化主链 |
| 5 | `memory_addr`、`memory_set`、`percpu`、`percpu_macros`、`axcpu`、`kernel_guard`、`crate_interface` 等低层运行时基础件 | 同样复用 `axcpu`、`percpu`、`scope-local`、`crate_interface` 等公共基础语义 | `x86_vcpu`、`riscv-h`、`riscv_plic` 以及共享的 `crate_interface` / `percpu` / 地址语义基础层 |
| 6 | 剩余板级平台包、`axplat-dyn` 动态平台桥接、`cargo-axplat` 平台脚手架 | 共享的平台包与平台配置接线方式 | `axplat-x86-qemu-q35`、`axplat-dyn` 等宿主平台接入链 |
| 7 | `axdriver` 下游类别层，以及 `axdisplay` / `axinput` / `axdma` 模块入口 | 通过共享驱动能力影响输入/显示/块/网卡等设备接入认知 | VirtIO、PCI、vsock、DMA 等 Hypervisor Host 侧设备链认知 |
| 8 | 旧栈 `axfs`、新栈 `axfs-ng`、VFS、`ramfs`/`devfs`/`rsext4` | 新栈 `axfs-ng-vfs`、伪文件系统与部分共享 FS 基础语义 | 直接运行时影响较弱，更多体现在镜像/rootfs 准备与共享 FS 认知 |
| 9 | `axio`、`axpoll`、`axnet`、`axnet-ng`、HTTP 示例程序 | `axnet-ng`、socket 路径、`smoltcp` 协议引擎及 syscall 侧网络语义 | 直接主链影响较弱，主要是共享 I/O、网络与协议栈层次认知 |
| 10 | 分配器、日志、错误码、IPI、调度算法、容器与时间队列等运行时叶子基础件 | 大量复用这些低层件支撑 StarryOS 运行时 | 同样复用这些叶子基础件支撑 Hypervisor 运行时 |
| 11 | AArch64 系统寄存器编码、PL011 设备叶子、`axhvc`、构造函数、位图/区间分配器等辅助件 | 共享过程宏、能力位和分配算法组件 | `axvisor_api_proc`、`axhvc`、部分寄存器/辅助宏路径与虚拟化侧强相关 |
| 12 | `axfeat`、`axconfig*`、`axstd`、`axlibc`、`arceos_posix_api`、`axbuild`、`tg-xtask`、`deptool` | 构建/配置继承链、用户态 ABI 理解，以及与 ArceOS API 的边界 | 构建链、配置生成、宿主工具链和部分 API/feature 装配认知 |
| 13 | 间接受影响，主要是通过共享公共层理解 Starry 扩展 | `starry-process`、`starry-signal`、`starry-vm`、`starryos`、`starryos-test` 直接组成 Starry 扩展栈 | 基本无直接主链影响，更多是与公共基础层的分层对照 |
| 14 | ArceOS 示例程序、测试入口、`axplat` 最小内核样例链 | 基本无直接运行时主链影响 | 基本无直接运行时主链影响 |
| 15 | `crate_interface` 测试矩阵，以及 `mingo` 对树莓派链加载工作流的影响 | 几乎无直接子系统影响，仅间接帮助理解 `crate_interface` 机制 | 几乎无直接子系统影响，仅间接帮助理解 `crate_interface` 机制 |
