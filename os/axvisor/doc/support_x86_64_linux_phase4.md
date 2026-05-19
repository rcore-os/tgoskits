# Axvisor x86_64 Linux 支持：第 4 阶段文档

本文档记录 `support_x86_64_linux_phases.md` 中“阶段 4：x86 Linux boot stub 和 vCPU 初始状态”的实现结果、验证情况和下一阶段入口。

## 阶段目标

第 4 阶段目标是让 x86 Linux direct boot 分支不再停在 ImageLoader，而是写入 Linux 专用 boot stub，并让 BSP 从该 stub 进入 Linux protected-mode entry。

本阶段只保证 Axvisor 能把 vCPU 入口接到 Linux boot stub，并从 stub 跳转到 Linux 早期入口；不要求已经看到完整 Linux early console 或进入 initramfs shell。

## 已完成产物

- `os/axvisor/src/vmm/images/x86/linux_boot.rs`
  - 新增 Linux 专用 boot stub 构造模块。
  - 新增 `DEFAULT_LINUX_BOOT_LOAD_GPA = 0x8000`。
  - 新增 `build_boot_image()`，生成固定大小 `0x1000` 的 boot stub 页。
  - boot stub 从 Axvisor 当前 x86 real-mode 初始状态启动：
    - 关闭中断并设置方向标志为递增。
    - 设置基础段寄存器和栈。
    - 加载 stub 内置 GDT。
    - 设置 `CR0.PE` 进入 32-bit protected mode。
    - 设置 `CS = 0x10`，`DS/ES/SS/FS/GS = 0x18`。
    - 清零 `ebx`、`ebp`、`edi`。
    - 设置 `esi = boot_params GPA`。
    - 跳转到 Linux protected-mode kernel entry。
  - boot stub 模板中可 patch 两个立即数：
    - `boot_params` GPA。
    - kernel protected-mode entry GPA。
  - 新增单元测试，覆盖 stub patch 和加载地址校验。
- `os/axvisor/src/vmm/images/mod.rs`
  - Linux direct boot 分支不再写零填充 boot stub。
  - Linux direct boot 分支改为调用 `x86::linux_boot::build_boot_image()`。
  - ImageLoader 在 `vm.init()` 之前更新 `cpu_config.bsp_entry` 和 `cpu_config.ap_entry`，让 vCPU 从 `0x8000` 的 Linux boot stub 启动。
  - Linux memory image 和 fs image 分支在完成 payload、initramfs、boot_params 和 boot stub 加载后返回 `Ok(())`，不再返回阶段性 `Unsupported`。
  - 启动日志新增 Linux direct boot entry、boot_params GPA、kernel entry 和 initrd range。

## 当前入口约定

当前走 Linux 32-bit boot protocol 的 protected-mode entry 路线：

| 项目 | 当前值 |
| --- | --- |
| vCPU 初始入口 | `0x8000` |
| boot stub GPA | `0x8000` |
| boot_params GPA | `0x7000` |
| kernel protected-mode entry | VM config `kernel_load_addr`，当前样例为 `0x20_0000` |
| initramfs GPA | VM config `ramdisk_load_addr`，当前样例为 `0x40_0000` |

Axvisor 当前 x86 vCPU 初始化已经提供 real-mode、flat zero-based segment state，因此本阶段不修改 `components/x86_vcpu`。stub 自己负责从 real mode 切到 32-bit protected mode 后进入 Linux。

## 当前行为

合法 Linux bzImage：

1. ImageLoader 自动识别 bzImage。
2. 解析 setup header 并计算 protected-mode payload offset。
3. 构造并校验 `X86LinuxLoadLayout`。
4. 构造 `boot_params`。
5. 构造 Linux boot stub。
6. 将 `boot_params` 加载到 `0x7000`。
7. 将 Linux boot stub 加载到 `0x8000`。
8. 将 kernel payload 加载到 `kernel_load_addr`。
9. 将 initramfs 加载到 `ramdisk_load_addr`。
10. 更新 VM BSP/AP entry 为 `0x8000`。
11. ImageLoader 返回 `Ok(())`，后续 `vm.init()` 设置 vCPU 初始入口。

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
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

结果：x86_64 Linux VM 配置下 Axvisor release build 通过。

```bash
AX_CONFIG_PATH=/code/tgoskits/tmp/axbuild/axconfig/axvisor/x86_64-unknown-none/.axconfig.toml \
AX_TARGET=x86_64-unknown-none \
AXVISOR_VM_CONFIGS=/code/tgoskits/os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml \
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

- 本阶段尚未要求 Linux early console 可见，也未确认能进入 initramfs shell。
- 当前 boot stub 使用 32-bit protected-mode entry，依赖 `boot_params` 内容足以支撑 Linux 早期初始化。
- 当前 command line 仍为空，early console 依赖内核内建 command line 或镜像构建流程。
- 第 5 阶段应通过 QEMU 启动验证 Linux early boot log、串口输出路径和 initramfs `/init`。
