# LoongArch Linux Direct Boot

本文只记录 LoongArch Linux direct boot 问题：Axvisor 不经过 guest UEFI/bootloader，直接把 Linux kernel、cmdline、DTB/initrd 信息放到 guest 可见的位置，然后跳到 Linux entry。

## 这个文件是什么

`linux-direct-boot-analysis.md` 用来说明 LoongArch Linux 的启动协议和 Axvisor 当前怎么模拟该启动环境。

它不跟踪 virtio-blk rootfs、IRQ 注入、idle 唤醒或 stage-2 TLB refill。这些问题分别放在独立文档中。

## 当前启动链路

当前 LoongArch Linux quick-start 的 direct boot 链路是：

```text
VM TOML
  -> ImageLoader 加载 LoongArch ELF kernel
  -> 加载 DTB 和 initramfs
  -> setup_bootinfo 构造 LoongArch Linux bootinfo
  -> 设置 guest boot_args
  -> vCPU 进入 Linux entry
```

关键代码位置：

- `os/axvisor/src/vmm/images/mod.rs`
- `components/loongarch_vcpu/src/vcpu.rs`
- `components/axvm/src/config.rs`
- `components/axvm/src/vm.rs`

## Kernel 格式

LoongArch loader 当前先尝试 ELF：

```rust
loongarch_elf::try_load(...)
```

如果是 LoongArch ELF，就按 `PT_LOAD` segment 加载，并用 ELF entry 更新：

```rust
config.cpu_config.bsp_entry = info.entry;
config.cpu_config.ap_entry = info.entry;
```

如果不是 ELF，则 fallback 为 raw image，按 `kernel_load_gpa` 直接拷贝。

当前 quick-start 使用的是 LoongArch ELF kernel。

## Boot 参数

LoongArch Linux 当前通过 boot args 接收 direct boot 信息：

```text
a0 = 1
a1 = cmdline GPA
a2 = EFI system table GPA
```

Axvisor 中对应设置为：

```rust
config.cpu_config.boot_args = [1, CMDLINE_GPA, SYSTAB_GPA];
```

其中 `setup_bootinfo()` 会构造：

- cmdline buffer；
- EFI vendor string；
- EFI system table；
- EFI config table；
- EFI memory map；
- initrd table；
- optional DTB pointer。

## Cmdline 原则

`kernel.cmdline` 必须来自 VM 配置文件。loader 不应该自动生成或追加 `console=`、`root=`、`init=` 等参数。

当前原则：

- `kernel.cmdline` 缺失时直接报错；
- 缺少 `console=` 只 warning；
- 缺少 `init=` / `rdinit=` 只 warning；
- rootfs 方式由配置决定。

这样可以确保使用者能从配置文件明确知道 guest 是如何启动的。

## 与 ARM/RISC-V 的区别

AArch64 Linux direct boot 通常是：

```text
x0 = DTB physical address
entry = raw Image entry
```

RISC-V Linux direct boot 通常是：

```text
a0 = hart id
a1 = DTB physical address
entry = raw Image entry
```

LoongArch 这里不是简单传一个 DTB 地址，而是需要模拟 EFI-style bootinfo：

```text
a0 = efi_boot flag
a1 = cmdline
a2 = EFI system table
```

因此不能直接复用 ARM 的 raw Image + `x0=dtb` 方法。

## 需要继续确认

1. EFI memory map 的 memory type 是否足够准确。
2. DTB config table GUID 和 Linux LoongArch 实际读取路径是否完全匹配。
3. 多内存段、多 vCPU、较大 initrd 时 bootinfo 边界是否正确。
4. raw image fallback 是否需要更严格校验 entry/load address。
5. `kernel.cmdline` 缺失时报错是否覆盖所有 direct boot 入口。
