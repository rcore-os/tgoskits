---
sidebar_position: 30
sidebar_label: "AxVisor x86_64 HTTP Boot NUC15 阶段记录"
---

# AxVisor x86_64 HTTP Boot NUC15 阶段记录

本文记录 ASUS NUC15CRH x86_64 真机 HTTP Boot 调试链路当前阶段的结构调整。

## 背景

ASUS NUC15CRH 通过 ostool UEFI loader 从 `ostool-server` 下载 AxVisor 镜像并跳转运行。前期调试已经验证：

- UEFI loader 可以下载 `manifest.json` 和 `kernel.bin`
- loader 可以通过 `ExitBootServices`
- loader 可以跳转到 AxVisor 的 x86_64 入口
- AxVisor 可以在 COM1 115200 串口输出早期日志

这些行为与 QEMU Q35 的 multiboot 启动路径不同，不适合继续放在 `x86-qemu-q35` 平台中。

## 本阶段目标

本阶段的目标是把 NUC15 真机 HTTP Boot 行为从 QEMU 平台中拆出，形成独立的 board 配置和平台包：

- QEMU 继续使用 `x86-qemu-q35`
- NUC15 真机 HTTP Boot 使用 `x86-asus-nuc15crh`
- AxVisor 构建脚本尊重 `AX_PLATFORM`
- HTTP Boot 检查器能够识别 direct-entry 入口

## 实现内容

新增 board 配置：

```text
os/axvisor/configs/board/asus-nuc15crh-x86_64.toml
```

该配置显式启用：

```toml
"ax-hal/x86-asus-nuc15crh"
```

新增平台包：

```text
platform/x86-asus-nuc15crh
```

该平台包保留 x86_64 multiboot 兼容路径，并新增 ostool HTTP Boot direct-entry 路径：

- `httpboot_entry` 作为 loader manifest 的入口
- `rust_httpboot_entry(boot_info)` 接收 ostool boot-info 指针
- early init 优先解析 ostool boot-info 中的内存图
- COM1 固定初始化为 115200 baud，匹配当前调试串口

AxVisor 的 `build.rs` 已改为优先读取 `AX_PLATFORM`。这避免了使用 NUC15 配置构建时，AxVisor 内部仍然认为自己运行在 `x86-qemu-q35`。

`axbuild` 平台解析中加入了 `x86-asus-nuc15crh`，因此 `ax-hal/x86-asus-nuc15crh` 可以正确解析到 `axplat-x86-asus-nuc15crh`。

HTTP Boot 检查器现在会识别 `httpboot_entry`：

- 有 direct-entry 时输出 `phase0_status: ready`
- 没有 direct-entry 时输出 `phase0_status: partial`

## 验证结果

NUC15 配置检查命令：

```bash
cargo xtask axvisor httpboot check \
  --config os/axvisor/configs/board/asus-nuc15crh-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

关键输出：

```text
AX_PLATFORM=x86-asus-nuc15crh
httpboot_entry: 0xffff800000200006
phase0_status: ready
manifest_v1_candidate: kernel_load_addr=0x200000 entry_point=0x200006
```

QEMU 配置检查命令：

```bash
cargo xtask axvisor httpboot check \
  --config os/axvisor/configs/board/qemu-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

关键输出：

```text
AX_PLATFORM=x86-qemu-q35
httpboot_entry: missing
phase0_status: partial
```

这说明 QEMU 平台没有被 NUC15 direct-entry 改动污染。

## 当前边界

当前阶段只完成平台和 board 配置隔离，不解决 VMX enable 失败问题。真机继续启动后仍需要定位：

- BIOS 中是否启用 Intel VT-x
- `IA32_FEATURE_CONTROL` 是否 lock 且允许 VMXON
- CPUID VMX bit 是否可见
- AxVisor VMX 初始化失败时的更详细诊断日志

后续调试 VMX 时，应继续使用：

```bash
cargo axvisor httpboot \
  --config os/axvisor/configs/board/asus-nuc15crh-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```
