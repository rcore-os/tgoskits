# Axvisor x86_64 Linux 支持：第 1 阶段文档

本文档记录 `support_x86_64_linux_phases.md` 中“阶段 1：x86 Linux bzImage 识别和 header 解析”的实现结果、验证情况和下一阶段入口。

## 阶段目标

第 1 阶段目标是让 x86_64 ImageLoader 能识别 Linux bzImage，并解析后续 direct boot 所需的 Linux x86 setup header 字段，同时保持现有 x86 非 Linux Multiboot 路径不回归。

本阶段不启动 Linux，不加载 protected-mode payload，也不构造 `boot_params`。

## 已完成产物

- `os/axvisor/src/vmm/images/x86/linux.rs`
  - 新增 Linux x86 boot protocol setup header 解析模块。
  - 校验 `boot_flag == 0xaa55`。
  - 校验 `header == "HdrS"`。
  - 解析 `setup_sects`，并按协议处理 `setup_sects == 0` 代表 4 个 setup sectors。
  - 解析后续阶段需要的字段：
    - `boot_protocol_version`
    - `code32_start`
    - `cmdline_size`
    - `initrd_addr_max`
    - `kernel_alignment`
    - `relocatable_kernel`
    - `loadflags`
    - `heap_end_ptr`
  - 提供 `payload_offset()`，用于后续阶段定位 protected-mode kernel payload。
  - 增加 header parser 单元测试，覆盖合法 header、默认 `setup_sects`、非法 magic 和 truncated buffer。
- `os/axvisor/src/vmm/images/mod.rs`
  - x86_64 下在 memory image 和 fs image 加载前先尝试识别 bzImage。
  - 合法 bzImage 会进入 Linux direct boot 分支，并明确返回 `Unsupported`，提示 payload loading 从阶段 2 开始。
  - 非 Linux image 解析失败时只打印 debug log，然后继续走原有 Multiboot/BIOS 加载路径。

## 当前行为

合法 Linux bzImage：

1. ImageLoader 读取 kernel image 的 setup header。
2. `X86LinuxHeader::parse()` 校验并解析字段。
3. loader 打印 header 和 `payload_offset`。
4. loader 返回 `Unsupported`，明确说明 Linux direct boot 已识别，但 payload loading 要等阶段 2。

非 Linux x86 image：

1. bzImage header 解析失败。
2. loader debug 记录失败原因。
3. 继续原有 kernel load、ramdisk load、BIOS stub load 和 Multiboot info patch 流程。

这个行为能让后续阶段在明确的 Linux 分支上继续实现，同时避免 Linux bzImage 被误送进现有 Multiboot stub。

## 验证情况

已完成验证：

```bash
cargo fmt --all
```

```bash
cargo xtask axvisor build \
  --config os/axvisor/configs/board/qemu-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/nimbos-x86_64-qemu-smp1.toml
```

结果：x86_64 Axvisor release build 通过，无 warning。

```bash
AX_CONFIG_PATH=/code/tgoskits/tmp/axbuild/axconfig/axvisor/x86_64-unknown-none/.axconfig.toml \
AX_TARGET=x86_64-unknown-none \
AXVISOR_VM_CONFIGS=/code/tgoskits/os/axvisor/configs/vms/nimbos-x86_64-qemu-smp1.toml \
AX_LOG=info \
AX_PLATFORM=x86-qemu-q35 \
AX_IP=10.0.2.15 \
AX_ARCH=x86_64 \
AX_GW=10.0.2.2 \
cargo clippy -p axvisor \
  --target x86_64-unknown-none \
  -Z unstable-options \
  --target-dir /code/tgoskits/target \
  --features ax-std/myplat,ept-level-4,fs,vmx,log/release_max_level_info \
  --config 'target.x86_64-unknown-none.rustflags=["-Clink-arg=-Tlinker.x","-Clink-arg=-no-pie","-Clink-arg=-znostart-stop-gc"]' \
  --bin axvisor \
  --release \
  -- -D warnings
```

结果：target-specific x86_64 Axvisor clippy 通过。

尝试过的非适用验证：

```bash
cargo test -p axvisor --target x86_64-unknown-linux-gnu x86_linux -- --nocapture
```

失败原因：`axvisor` 是 `no_std` / `no_main` bare-metal binary，host test link 阶段找不到 `main`。

```bash
cargo xtask clippy --package axvisor
```

失败原因：当前通用 clippy wrapper 对 `axvisor` 使用 host clippy 组合，bare-metal binary 缺少 host panic handler，并非本阶段代码触发的 lint。实际采用上面的 x86_64 target-specific clippy 作为有效验证。

## 遗留风险和下一阶段入口

- 本阶段只识别和解析 bzImage，不加载 protected-mode payload。
- 合法 bzImage 当前会返回 `Unsupported`，这是阶段 1 的预期行为。
- header parser 的单元测试已写入源码，但受 `axvisor` bare-metal binary 测试环境限制，当前没有通过 host `cargo test` 执行。
- 第 2 阶段应接入 `payload_offset()`，实现 bzImage protected-mode payload、initramfs、boot_params 预留区和 boot stub 的 guest memory layout。
