# Axvisor x86_64 Linux 支持：第 0 阶段文档

本文档记录 `support_x86_64_linux_phases.md` 中“阶段 0：现状盘点和最小实验环境”的结论、产物和详细盘点内容。第 0 阶段不修改核心启动逻辑，只固定后续 x86_64 Linux direct boot 所需的实验输入和现状边界。

## 阶段目标

第 0 阶段目标是把后续阶段需要依赖的输入和边界先整理清楚：

1. 确认当前 x86_64 Axvisor guest 启动链路仍是 Multiboot 风格。
2. 明确 Linux bzImage direct boot 需要新增 loader 分支，不能直接复用现有 `x86_boot.rs` stub。
3. 准备后续阶段可复用的 Linux VM 配置样例和最小 initramfs 构建脚本。
4. 记录 QEMU x86_64 基础平台资源、PIO/MMIO passthrough 边界和中断注入风险。

## 已完成产物

- `os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml`
  - x86_64 Linux direct boot 后续阶段的 VM 输入样例。
  - 当前用于固定实验输入，不代表现有代码已经能启动 Linux bzImage。
- `os/axvisor/scripts/build_x86_64_linux_initramfs.sh`
  - 最小 initramfs 构建脚本。
  - 生成的 `/init` 会挂载 `devtmpfs`、打开 `/dev/console`、输出验证信息，然后保持运行。
- `os/axvisor/doc/support_x86_64_linux_phase0.md`
  - 本阶段唯一文档，包含总结、现状盘点、实验输入、资源记录和验证边界。

## 现状结论

当前 x86_64 guest 启动路径主要服务 ArceOS/NimbOS 这类非 Linux guest：

- `os/axvisor/src/vmm/images/mod.rs` 按 VM config 加载 kernel 和 ramdisk，再按 `enable_bios` 决定是否加载 BIOS stub。
- `os/axvisor/src/vmm/images/x86_boot.rs` 内置的 `DEFAULT_BIOS_IMAGE` 会从 real mode 切到 protected mode，设置 Multiboot magic 和 Multiboot info 指针，然后跳转到 `0x20_0000`。
- `components/axvm/src/vm.rs` 中 x86 vCPU 创建没有 Linux 专用 create/setup config，BSP 入口来自 VM config。
- `components/x86_vcpu/src/vmx/vcpu.rs` 以 real mode 初始化 guest，启用 unrestricted guest 和 EPT。
- x86 PIO 默认大多 passthrough，仅拦截 QEMU debug-exit port `0x604`；MMIO passthrough 通过 EPT 线性映射。
- `os/axvisor/src/hal/arch/x86_64/mod.rs::inject_interrupt()` 仍为空，后续 Linux timer/interrupt 阶段需要补齐。

因此，Linux 支持应继续按原计划新增 bzImage 识别、payload 布局、`boot_params` 构造和 Linux 专用 boot stub，而不是改写现有 Multiboot 路径。

## 当前 x86_64 启动路径

- `os/axvisor/src/vmm/images/mod.rs` 将 kernel 加载到 `kernel_load_addr`，在 ramdisk 存在时记录 ramdisk size，然后在 `enable_bios = true` 时加载 BIOS image。
- x86_64 下，`load_x86_multiboot_info()` 会在 GPA `0x6000` 写入 Multiboot info，在 GPA `0x6040` 写入 mmap entry，并把 boot image 里的 `mov ebx, imm32` 立即数修补成 Multiboot info GPA。
- `os/axvisor/src/vmm/images/x86_boot.rs` 内置 `DEFAULT_BIOS_IMAGE`，默认加载 GPA 是 `0x8000`。该 stub 从 16-bit real mode 启动，进入 32-bit protected mode，设置 `eax = 0x2badb002`，设置 `ebx` 为修补后的 Multiboot info 指针，然后跳转到 GPA `0x20_0000`。
- `components/axvm/src/vm.rs` 创建 x86 vCPU 时使用 unit create/setup config。BSP entry 来自 `kernel.entry_point`，当前 x86 config 通常设置为 `0x8000`，也就是 BIOS stub 入口。
- `components/x86_vcpu/src/vmx/vcpu.rs` 将 guest 初始化在 real mode，开启 unrestricted guest、EPT 和 I/O bitmap，并把 VM entry RIP 设置为配置里的 entry GPA。

第 0 阶段最重要的边界是：现有 x86 路径是 ArceOS/NimbOS 使用的 Multiboot-style 路径。Linux bzImage direct boot 后续要新增独立 loader 分支，在第 1 阶段通过 Linux setup header 自动识别，在第 3 阶段使用 Linux x86 `boot_params`。

## Device 和 VMExit 基线

- RAM 和 MMIO passthrough range 在 `components/axvm/src/vm.rs` 中通过 `AddrSpace::map_linear()` 建立 device mapping。
- VMX port I/O 默认大多 passthrough。当前 I/O bitmap 初始为 passthrough-all，setup 阶段只拦截 QEMU debug-exit port `0x604`。
- 如果某个 port I/O 被拦截，`AxVM::run_vcpu()` 会把它分发给 `AxVmDevices::handle_port_read/write()`。
- emulated MMIO exit 会通过 `AxVmDevices::handle_mmio_read/write()` 处理。已经通过 EPT 映射的 MMIO passthrough device 正常访问时不应退出。
- Nested page fault 会交给 VM address space page-fault handler。
- `os/axvisor/src/hal/arch/x86_64/mod.rs::inject_interrupt()` 当前为空。后续 Linux 阶段不能假设 interrupt reinjection 已经可用，需要在 timer/interrupt 阶段补齐或验证。

## 第 0 阶段实验输入

初始 x86_64 Linux VM 模板：

```text
os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

该模板有意只包含：

- 1 个 vCPU；
- 128 MiB guest RAM；
- 指向 bzImage 的 `kernel_path`；
- 指向最小 initramfs 的 `ramdisk_path`；
- 不启用 BIOS image；
- 不配置 emulated devices；
- QEMU q35 APIC/HPET MMIO passthrough ranges。

该模板当前应能被配置解析，但不要求能启动 Linux。Linux 启动要等后续 direct boot 阶段完成。

## Minimal Initramfs

从仓库根目录构建第 0 阶段 initramfs：

```bash
os/axvisor/scripts/build_x86_64_linux_initramfs.sh
```

默认输出：

```text
tmp/linux-x86_64/initramfs.cpio
```

生成的 `/init` 是一个很小的静态程序，会挂载 `devtmpfs`，打开 `/dev/console`，输出：

```text
axvisor x86_64 linux initramfs reached /init
```

然后进入 idle loop。串口输出依赖内核 command line 选择 `ttyS0`，userspace init 不直接访问 I/O port。该 initramfs 不依赖 block device、PCI、network、busybox 或真实 rootfs。

## 可复现 bzImage 输入

使用 upstream Linux tree 或固定的内部 Linux tree revision。第一轮 direct boot 实验的最低内核配置要求：

- `CONFIG_X86_64=y`
- `CONFIG_BLK_DEV_INITRD=y`
- `CONFIG_DEVTMPFS=y`
- 启用 built-in command line，因为第 0 阶段不通过 VM config 传递 command line：
  - `CONFIG_CMDLINE_BOOL=y`
  - `CONFIG_CMDLINE="console=ttyS0,115200 earlyprintk=serial,ttyS0,115200 rdinit=/init"`
  - `CONFIG_CMDLINE_OVERRIDE=y`
- 串口 console 支持：
  - `CONFIG_SERIAL_8250=y`
  - `CONFIG_SERIAL_8250_CONSOLE=y`
  - `CONFIG_EARLY_PRINTK=y`

一种可重复的构建方式：

```bash
make ARCH=x86_64 x86_64_defconfig
scripts/config \
  --enable BLK_DEV_INITRD \
  --enable DEVTMPFS \
  --enable CMDLINE_BOOL \
  --set-str CMDLINE "console=ttyS0,115200 earlyprintk=serial,ttyS0,115200 rdinit=/init" \
  --enable CMDLINE_OVERRIDE \
  --enable SERIAL_8250 \
  --enable SERIAL_8250_CONSOLE \
  --enable EARLY_PRINTK
make ARCH=x86_64 olddefconfig
make ARCH=x86_64 -j"$(nproc)" bzImage
```

将生成的 `arch/x86/boot/bzImage` 复制到 VM config 使用的路径，或者把 VM config patch 成本地绝对路径。

## QEMU x86_64 平台资源记录

当前仓库内的 QEMU platform 文件：

```text
os/axvisor/configs/qemu/qemu-x86_64.toml
```

该配置使用 `-machine q35`、`-cpu host`、`-accel kvm`、1 个 vCPU、`-nographic` 和 128 MiB host memory。quickstart 曾引用 `.github/workflows/qemu-x86_64-kvm.toml`，但当前 workspace 没有该文件；除非后续恢复 workflow config，否则使用 `os/axvisor/configs/qemu/qemu-x86_64.toml`。

后续 boot_params/e820 需要关注的资源：

| Resource | Address or port | Phase-0 handling |
| --- | --- | --- |
| COM1 serial | PIO `0x3f8..0x3ff`, IRQ 4 | Port I/O passthrough by current VMX bitmap; not an EPT MMIO mapping |
| PCI config address/data | PIO `0xcf8`, `0xcfc` | Port I/O passthrough baseline; later PCI phases must validate behavior |
| QEMU debug exit | PIO `0x604` | Intercepted by VMX path |
| IOAPIC | MMIO `0xfec0_0000..0xfec0_0fff` | Listed in VM passthrough devices |
| Local APIC | MMIO `0xfee0_0000..0xfee0_0fff` | Listed in VM passthrough devices |
| HPET | MMIO `0xfed0_0000..0xfed0_0fff` | Listed in VM passthrough devices |
| PIT | PIO `0x40..0x43`, `0x61` | Port I/O passthrough baseline; timer behavior still unvalidated |
| PIC | PIO `0x20..0x21`, `0xa0..0xa1` | Port I/O passthrough baseline; interrupt behavior still unvalidated |
| RTC/CMOS | PIO `0x70..0x71` | Port I/O passthrough baseline |

## 验证情况

已完成的轻量验证：

- `bash -n os/axvisor/scripts/build_x86_64_linux_initramfs.sh`
- 实际运行过 initramfs builder，确认能生成 cpio 归档。
- 新增 VM TOML 通过 Python `tomllib` 解析检查。

本阶段没有修改 Rust 逻辑代码，因此未运行 `cargo fmt` 和 `cargo clippy`。

## 验证命令

现有 non-Linux x86 smoke path 应作为后续回归检查：

```bash
cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-x86_64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/nimbos-x86_64-qemu-smp1.toml
```

对第 0 阶段 Linux 输入，生成 `bzImage` 和 `initramfs.cpio` 后，复制文件到 `linux-x86_64-qemu-smp1.toml` 中记录的路径，或把路径 patch 成本地绝对路径。该配置是阶段 1-4 的输入约定；Linux 成功启动不是第 0 阶段验收标准。

## 遗留风险和下一阶段入口

- `linux-x86_64-qemu-smp1.toml` 是后续阶段输入样例，当前阶段不要求可启动 Linux。
- Linux early console 依赖内核内建 command line，例如 `console=ttyS0,115200 rdinit=/init`。
- 中断注入和 timer 仍未验证，不能作为第 0 阶段验收前提。
- 下一阶段进入 `x86_linux.rs`：解析 Linux x86 setup header，识别 bzImage，并在 loader 中保持 Linux / 非 Linux x86 image 分流。

## 后续阶段文档约定

以后每完成一个阶段，新增一份同类阶段文档：

- `support_x86_64_linux_phase1.md`
- `support_x86_64_linux_phase2.md`
- `support_x86_64_linux_phase3.md`
- 以此类推。

每份阶段文档至少包含：

1. 阶段目标。
2. 已完成产物。
3. 关键实现或现状结论。
4. 验证命令和结果。
5. 遗留风险及下一阶段入口。
