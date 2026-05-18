# Axvisor x86_64 Linux 支持：第 2 阶段文档

本文档记录 `support_x86_64_linux_phases.md` 中“阶段 2：bzImage payload、initramfs 和 guest 内存布局”的实现结果、验证情况和下一阶段入口。

## 阶段目标

第 2 阶段目标是在 x86 Linux direct boot 分支中完成第一版 guest memory layout，并把 bzImage protected-mode payload 与 initramfs 加载到 guest RAM。

本阶段不构造 Linux `boot_params`，不切换 vCPU 初始状态，也不真正启动 Linux。

## 已完成产物

- `os/axvisor/src/vmm/images/x86_linux.rs`
  - 新增 x86 Linux direct boot 固定低地址布局常量：
    - `BOOT_PARAMS_GPA = 0x7000`
    - `BOOT_PARAMS_SIZE = 0x1000`
    - `BOOT_STUB_GPA = 0x8000`
    - `BOOT_STUB_SIZE = 0x1000`
  - 新增 `X86LinuxRange`，统一描述 guest physical range。
  - 新增 `X86LinuxLoadLayout`，记录 boot params、boot stub、kernel payload 和 initrd range。
  - 新增布局校验：
    - kernel payload 不能为空。
    - range end 不能溢出。
    - kernel、initrd、boot_params、boot_stub 不能互相重叠。
    - initrd end 不能超过 Linux header 中的 `initrd_addr_max + 1`。
  - 新增布局相关单元测试，覆盖合法布局、kernel/boot stub 冲突、initrd/kernel 冲突和 initrd 超限。
- `os/axvisor/src/vmm/images/mod.rs`
  - Linux bzImage 分支不再停在“payload loading starts in phase 2”。
  - memory image 和 fs image 都会：
    - 根据 `setup_sects` 计算 `payload_offset()`。
    - 将 protected-mode kernel payload 加载到 `kernel_load_addr`。
    - 按 VM config 的 `ramdisk_load_addr` 加载 initramfs，并记录 ramdisk size。
    - 写入零填充的 boot_params 预留页。
    - 写入零填充的 boot_stub 预留页。
    - 打印完整布局日志，包含 boot_params、boot_stub、kernel 和 initrd range。
  - 完成 payload/initramfs 加载后返回 `Unsupported`，明确提示 `boot_params` 构造从阶段 3 开始。

## 当前布局

第一版固定布局如下：

| 区域 | GPA | 大小 | 状态 |
| --- | --- | --- | --- |
| `boot_params` 预留区 | `0x7000` | `0x1000` | 本阶段写零，阶段 3 构造内容 |
| Linux boot stub 预留区 | `0x8000` | `0x1000` | 本阶段写零，阶段 4 写入 stub |
| protected-mode kernel payload | VM config `kernel_load_addr` | bzImage size - `payload_offset()` | 本阶段加载 |
| initramfs | VM config `ramdisk_load_addr` | ramdisk 文件大小 | 本阶段加载 |

当前配置样例使用：

- kernel payload GPA: `0x20_0000`
- initramfs GPA: `0x40_0000`

这组地址不会覆盖低地址 boot metadata，也方便后续阶段继续填充 `boot_params` 和 boot stub。

## 当前行为

合法 Linux bzImage：

1. ImageLoader 自动识别 bzImage。
2. 解析 setup header 并计算 protected-mode payload offset。
3. 根据 VM config 和 header 构造 `X86LinuxLoadLayout`。
4. 如果地址冲突、payload 为空、initrd 超过 `initrd_addr_max`，返回明确错误。
5. 如果布局合法，加载 kernel payload、initramfs、boot_params 预留页和 boot_stub 预留页。
6. 返回 `Unsupported`，提示阶段 3 继续构造 `boot_params`。

非 Linux x86 image：

1. bzImage header 解析失败。
2. loader 继续原有 kernel load、ramdisk load、BIOS stub load 和 Multiboot info patch 流程。

## 验证情况

已完成验证：

```bash
cargo fmt --all -- --check
```

结果：通过。

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

## 遗留风险和下一阶段入口

- `boot_params` 目前只是零填充预留区，Linux 还不能从这里启动。
- boot stub 目前只是零填充预留区，阶段 4 才写入真正的 Linux boot stub。
- 当前布局使用固定低地址，后续如遇不同 kernel/initrd 体积或 e820 保留区冲突，再考虑参数化。
- 第 3 阶段应新增 `x86_boot_params.rs`，把 setup header、e820、initrd start/size 和必要 loader flags 写入 `boot_params`。
