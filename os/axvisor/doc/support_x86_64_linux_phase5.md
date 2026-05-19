# Axvisor x86_64 Linux 支持：第 5 阶段文档

本文档记录 `support_x86_64_linux_phases.md` 中“阶段 5：early serial console 和 initramfs 最小闭环”的当前实现结果、验证情况和后续补齐项。后续每完成一个阶段，都应继续补充对应阶段文档。

## 阶段目标

第 5 阶段目标是让 x86_64 Linux direct boot 路径具备可观察的 early boot 输出，并通过 initramfs `/init` 输出稳定成功标记。

当前阶段已经把 Linux kernel、initramfs、command line、EPT 缺页处理和若干 Linux early boot 依赖串起来。Axvisor QEMU 已经能看到 Linux 解包 initramfs、执行 `/init`，并通过 `/dev/kmsg` 输出 initramfs marker。第 5 阶段的最小闭环已经成立；普通 console/stdout/stderr 的 userspace 串口输出仍作为后续细化项保留。

## 输入文件

本阶段使用 host-side 文件作为 Linux 输入：

- `/code/tgoskits/tmp/qemu_x86_64_linux/linux/bzImage`
- `/code/tgoskits/tmp/qemu_x86_64_linux/linux/initramfs.cpio`
- `/code/tgoskits/tmp/qemu_x86_64_linux/linux/linux-qemu`

`linux-x86_64-qemu-smp1.toml` 使用 `image_location = "memory"`，因此这些文件会在构建/启动 Axvisor 时从 host 路径嵌入，不需要再复制到 Axvisor rootfs 的 `/guest/...` 路径。

`linux-qemu` 也是合法 x86 bzImage，当前 loader 已经能够识别并按 Linux boot protocol 启动。裸 QEMU direct boot 下，`linux-qemu` 搭配当前 initramfs 能进入 `/init` 并输出 marker。Axvisor 当前默认仍使用 Ubuntu `bzImage`，因为 `linux-qemu` 在 Axvisor 现阶段会更早触发 APIC/topology 相关 panic，需要等后续 APIC 路径补齐后再作为默认测试内核。

initramfs 由以下脚本生成：

```bash
os/axvisor/scripts/build_x86_64_linux_initramfs.sh \
  /code/tgoskits/tmp/qemu_x86_64_linux/linux/initramfs.cpio
```

同一组 `bzImage` 和 `initramfs.cpio` 已经用裸 QEMU direct boot 验证，能够进入 `/init` 并输出：

```text
axvisor x86_64 linux initramfs reached /init
```

## 已完成产物

- `os/axvisor/src/vmm/images/x86/boot_params.rs`
  - `BootParamsBuilder` 新增 `set_command_line()`。
  - command line 写入 `boot_params` 内部 `0xe00` 位置。
  - `cmd_line_ptr` / `ext_cmd_line_ptr` 指向该字符串。
  - 写入前校验：
    - command line 不能包含 NUL。
    - command line 长度不能超过 zero page 中预留空间。
    - 如果 Linux header 提供 `cmdline_size`，同时受该字段限制。
  - 新增 command line 写入、超长和 NUL 字符测试。
- `os/axvisor/src/vmm/images/mod.rs`
  - x86 Linux direct boot 分支从 VM config 的 `kernel.cmdline` 读取启动参数。
  - 如果 command line 非法，返回 `InvalidInput` 并带具体错误原因。
- `os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml`
  - `entry_point = 0x8000`，使用 Linux-specific boot stub。
  - `image_location = "memory"`，从 host-side 路径嵌入 Linux 输入文件。
  - `kernel_path = "/code/tgoskits/tmp/qemu_x86_64_linux/linux/bzImage"`。
  - `ramdisk_path = "/code/tgoskits/tmp/qemu_x86_64_linux/linux/initramfs.cpio"`。
  - guest RAM 调整为 128 MiB。
  - `ramdisk_load_addr = 0x60_00000`，避免 initramfs 被 Ubuntu generic kernel 解压阶段覆盖。
  - Linux command line 写入 boot_params：
    - `console=ttyS0,115200n8`
    - `earlycon=uart8250,io,0x3f8,115200`
    - `rdinit=/init`
    - `acpi=off noapic nolapic pci=off`
    - `i8042.nokbd i8042.noaux i8042.nomux`
  - `acpi=off noapic nolapic pci=off i8042.*` 是阶段 5 bring-up 用的临时参数，用于绕开后续阶段才会补齐的 APIC、PCI 和键盘控制器路径。
- `os/axvisor/configs/qemu/qemu-x86_64.toml`
  - QEMU host memory 调整为 512 MiB，给 Axvisor 和 Linux guest 留出更稳定的调试空间。
- `os/axvisor/scripts/build_x86_64_linux_initramfs.sh`
  - `/init` 会挂载 `devtmpfs`。
  - marker 会先写入 `/dev/kmsg`，用于确认 `/init` 已经执行。
  - 优先打开 `/dev/console`，失败时回退到 `/dev/ttyS0`。
  - marker 同时写入原 console fd、stdout 和 stderr。
- `components/x86_vcpu/src/vmx/vcpu.rs`
  - `EPT_VIOLATION` 现在转换为 `AxVCpuExitReason::NestedPageFault`，使 Linux decompressor 和 kernel 后续 GPA 访问能够由上层处理。
  - 修正 `XSETBV` 校验逻辑，允许 Linux 启用合法的 xstate 组合。
  - 增加 AMD64 `DE_CFG` MSR (`0xc001_1029`) 的最小读写处理；读返回 0，写忽略，并推进 RIP。该 MSR 是当前 Ubuntu generic kernel early boot 会访问的兼容性路径。

## 串口策略和当前状态

x86 VMX 当前使用 passthrough-all I/O bitmap，guest 的 PIO 访问不会 trap 到 Axvisor 设备模型。因此 Linux 访问 COM1 `0x3f8` 时，应直接落到 QEMU 的 16550 串口。

当前 command line 同时设置 `console=ttyS0` 和 `earlycon=uart8250,io,0x3f8`，目标是让 Linux early boot 和 initramfs `/init` 都通过同一条串口路径输出。

裸 QEMU direct boot 使用同一份 `bzImage` 和 `initramfs.cpio` 时，可以看到 `/dev/kmsg`、console、stdout 和 stderr 产生的 initramfs marker。Axvisor QEMU 当前能看到 Linux early boot，并已经到达：

```text
Run /init as init process
axvisor x86_64 linux initramfs reached /init
```

其中 Axvisor 里的 marker 来自 `/dev/kmsg`，说明 `/init` 已经执行。普通 console/stdout/stderr 的 userspace 串口输出尚未稳定出现，剩余问题更可能在 Axvisor 下 userspace console/ttyS0 输出路径，而不是 initramfs 文件本身。

## 当前行为

合法 Linux bzImage：

1. ImageLoader 自动识别 bzImage。
2. 加载 kernel payload、initramfs、boot_params 和 Linux boot stub。
3. 将 VM config 中的 `kernel.cmdline` 写入 boot_params。
4. vCPU 从 `0x8000` 的 Linux boot stub 启动。
5. Linux boot stub 设置 `esi = boot_params` 并跳转到 kernel protected-mode entry。
6. Linux 根据 boot_params 中的 command line 初始化 early console，解包 initramfs，并执行 `/init`。

非 Linux x86 image：

1. bzImage header 解析失败。
2. loader 继续原有 kernel load、ramdisk load、BIOS stub load 和 Multiboot info patch 流程。

## 验证情况

已完成验证：

```bash
cargo fmt --all
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

```bash
timeout 20s qemu-system-x86_64 \
  -m 256M \
  -smp 1 \
  -nographic \
  -kernel /code/tgoskits/tmp/qemu_x86_64_linux/linux/bzImage \
  -initrd /code/tgoskits/tmp/qemu_x86_64_linux/linux/initramfs.cpio \
  -append 'console=ttyS0,115200n8 rdinit=/init panic=-1'
```

结果：裸 QEMU 能看到 initramfs marker。

```bash
timeout 20s qemu-system-x86_64 \
  -m 256M \
  -smp 1 \
  -nographic \
  -kernel /code/tgoskits/tmp/qemu_x86_64_linux/linux/linux-qemu \
  -initrd /code/tgoskits/tmp/qemu_x86_64_linux/linux/initramfs.cpio \
  -append 'console=ttyS0,115200n8 rdinit=/init panic=-1'
```

结果：`linux-qemu` 也是合法 bzImage，裸 QEMU 能进入 `/init` 并看到 initramfs marker。

```bash
cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-x86_64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

结果：Axvisor QEMU 能看到 Linux early boot，并到达 `Run /init as init process`；随后能看到 `/dev/kmsg` 输出的 initramfs marker：

```text
axvisor x86_64 linux initramfs reached /init
```

## 后续补齐项

- 调查 Axvisor 下 `/dev/console` / `/dev/ttyS0` 的 userspace 写入路径，确认 stdout/stderr marker 未出现是串口输出路由、设备中断还是 console 绑定问题。
- 在第 6/7 阶段逐步恢复被临时关闭的 APIC、PCI 和 virtio 路径。
- 去掉阶段 5 bring-up command line 中的 `acpi=off noapic nolapic pci=off i8042.*` 临时参数，改为依赖 Axvisor 的真实设备模型和中断模型。
