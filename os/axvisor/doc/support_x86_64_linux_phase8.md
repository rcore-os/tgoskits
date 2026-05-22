# x86_64 Linux 支持阶段 8：真实 rootfs 与默认 shell

本文档记录阶段 8 的当前状态、已完成工作、已知问题和后续任务。阶段 8 的目标是让
x86_64 Linux 像 aarch64/riscv64 Linux 一样，通过默认配置直接使用 managed rootfs 启动，
不依赖阶段脚本，也不在 VM 配置里塞一长串 Linux cmdline。

## 当前结论

阶段 8 已证明 x86_64 Linux 可以从真实 ext4 rootfs 启动：

- Linux 能识别外层 QEMU 提供的 virtio-blk 为 `/dev/vda`。
- Linux 能把 `tmp/axbuild/rootfs/rootfs-x86_64-alpine.img` 挂载为读写根文件系统。
- Linux 能执行 rootfs 中的用户态程序。
- 默认 `linux-x86_64-qemu-smp1.toml` 不再依赖 initramfs。
- 默认启动路径已经可以进入 BusyBox shell，不过当前 shell 仍没有 controlling tty。

当前默认启动命令保持和其他架构一致：

```sh
cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-x86_64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-x86_64.toml \
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

验证日志中的关键节点：

```text
virtio_blk virtio0: [vda] 2097152 512-byte logical blocks
EXT4-fs (vda): mounted filesystem ... r/w with ordered data mode
VFS: Mounted root (ext4 filesystem) on device 253:0.
devtmpfs: mounted
Run /bin/sh as init process
/bin/sh: can't access tty; job control turned off
~ #
```

这说明阶段 7 建立的 PCI config、low MMIO、MAP_IDENTICAL DMA、vIOAPIC redirection 和
GSI 23 forwarding 已经足以支撑真实 rootfs 和最小 shell。

## 已完成工作

默认 x86_64 Linux VM 配置已经收敛到真实 rootfs 路线：

- `image_location` 从 `memory` 切到 `fs`。
- `kernel_path` 使用 rootfs 中的 `/guest/linux/linux-qemu`。
- 移除了 `ramdisk_path` 和 `ramdisk_load_addr`。
- 移除了 VM 配置里的显式 `cmdline`，改由 x86 Linux boot params 提供默认值。

为了在没有 `cmdline` 的情况下仍走 x86 Linux direct boot，VMM 不再用
`config.kernel.cmdline.is_some()` 判断 x86 Linux，而是读取 bzImage 头做检测：

- `os/axvisor/src/vmm/config.rs` 中的 `x86_linux_direct_boot_config()` 改成基于镜像检测。
- `os/axvisor/src/vmm/images/mod.rs` 新增 `is_x86_linux_image_config()`，支持 `memory` 和 `fs` 两种 image location。

x86 Linux boot params 新增默认 cmdline，当前形态为：

```text
console=ttyS0 root=/dev/vda rw rootwait devtmpfs.mount=1 init=/bin/sh
acpi=off pci=conf1 pci=nomsi nox2apic no_timer_check pmtmr=0x608
tsc=unstable initcall_blacklist=ahci_pci_driver_init,i8042_init
```

这里每一类参数的目的如下：

- `console=ttyS0`：让内核和 shell 使用串口。
- `root=/dev/vda rw rootwait`：使用 virtio-blk rootfs。
- `devtmpfs.mount=1`：rootfs 缺少静态 `/dev/console` 时，仍让内核挂载 devtmpfs。
- `init=/bin/sh`：绕开当前 rootfs 中不完整的 `/sbin/init` / OpenRC。
- `acpi=off pci=conf1 pci=nomsi nox2apic`：沿用当前 x86 bring-up 的受控 PCI/IRQ/APIC 路线。
- `no_timer_check pmtmr=0x608 tsc=unstable`：绕过当前 timer/clocksource 尚未完整收敛的问题。
- `initcall_blacklist=ahci_pci_driver_init,i8042_init`：避免 QEMU 默认 AHCI/PS2 探测拖慢启动。

还确认并处理了 QEMU 模板问题：

- 曾临时把 `os/axvisor/configs/qemu/qemu-x86_64.toml` 改成 `-nodefaults` / `-no-user-config`
  的最小拓扑，Linux 启动更干净。
- 但这份 QEMU 配置是 x86_64 通用模板，NimbOS/ArceOS 也可能复用；最小拓扑会改变默认
  VGA、NIC、firmware 和 PCI 设备布局。
- 因此不保留这类修改，`qemu-x86_64.toml` 应继续作为通用模板，Linux 专属处理放在
  x86 Linux boot params 内部。

阶段 8 曾使用一次性脚本注入 `/phase8-shell-init` 到临时 rootfs 副本中，以验证稳定交互
shell 和 rootfs 读写。这条路径已经完成历史任务，`run_x86_64_linux_phase8_shell.sh` 已删除，
不再作为默认或推荐入口。

## Rootfs 现状

aarch64、riscv64、loongarch64 和 x86_64 的 QEMU rootfs 都由 `cargo xtask axvisor qemu`
的 managed rootfs helper 统一准备，默认文件名分别是：

```text
rootfs-aarch64-alpine.img
rootfs-riscv64-alpine.img
rootfs-loongarch64-alpine.img
rootfs-x86_64-alpine.img
```

这些镜像位于 `tmp/axbuild/rootfs/`，缺失时会从 `rcore-os/tgosimages` release 下载并解包。
x86_64 默认使用：

```text
tmp/axbuild/rootfs/rootfs-x86_64-alpine.img
```

当前 x86_64 Alpine rootfs 的 `/etc/inittab` 会调用 OpenRC：

```text
::sysinit:/sbin/openrc sysinit
::sysinit:/sbin/openrc boot
::wait:/sbin/openrc default
::shutdown:/sbin/openrc shutdown
```

但镜像里没有 `/sbin/openrc`。因此如果默认执行 `/sbin/init`，会看到：

```text
Run /sbin/init as init process
can't run '/sbin/openrc': No such file or directory
```

这不是内核或 Axvisor 卡住，而是 rootfs 用户态 init 配套不完整。当前默认改用
`init=/bin/sh` 正是为了绕开这个问题。

## 当前问题

第一，默认 shell 能进入，但还没有 controlling tty：

```text
/bin/sh: can't access tty; job control turned off
```

这意味着 shell 可用于基本命令，但交互体验有限。BusyBox shell 可能向串口发送 `ESC[6n`
光标位置查询；如果外层 PTY 不响应，第一次输入命令时可能看起来像卡住。阶段脚本中的
`/phase8-shell-init` 曾通过显式重定向 `/dev/ttyS0` 改善这一点，但该脚本已经删除，默认路径
目前没有再注入 rootfs。

第二，尝试用 `init=/usr/bin/setsid -c /bin/sh` 修复 controlling tty，实测不稳定，触发过
Axvisor 侧 vCPU borrow panic：

```text
panic
RefCell already borrowed
```

因此没有保留这个方案。后续若要彻底解决交互 shell，应优先从 rootfs/init 配套或串口终端
处理入手，而不是继续把复杂入口塞进 boot params。

第三，x86_64 Linux 启动仍比 aarch64/riscv64 慢。主要原因不是 rootfs，而是当前 x86 平台
支持仍处在 bring-up 状态：

- Linux 会枚举 QEMU q35 默认设备，如 VGA、e1000、ICH/AHCI、SMBus 等。
- AHCI 和 i8042 已通过 `initcall_blacklist` 避开，但 e1000/SMBus/firmware 相关探测仍会增加启动时间。
- timer/clocksource 仍依赖 `no_timer_check`、`pmtmr=0x608`、`tsc=unstable` 等参数。
- virtio-blk IRQ completion 当前仍依赖 q35 smoke 拓扑中的 GSI 23 forwarding。

第四，`qemu-x86_64.toml` 不能随意收敛为 Linux-only 最小拓扑。该文件服务 x86_64 通用 QEMU
运行，NimbOS/ArceOS 也可能使用。Linux 需要的特殊拓扑或参数应放到 Linux 专属路径，或未来
新增明确命名的 Linux 专用 QEMU 模板，而不是污染通用模板。

## 验证记录

已执行的关键验证：

```sh
cargo fmt
cargo run -p axvmconfig --bin axvmconfig -- check \
  --config-path os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
cargo clippy -p axvisor --target x86_64-unknown-none ... -- -D warnings
```

默认启动验证已经确认：

- rootfs 能挂载到 `/dev/vda`。
- `devtmpfs` 能挂载。
- `/sbin/init` 的 OpenRC 问题被 `init=/bin/sh` 绕开。
- 能看到 BusyBox shell 提示符 `~ #`。
- `tsc=unstable` 用于避免进入 shell 后再刷出 TSC watchdog 日志。

短时验证中，`TERM=dumb` 放入 kernel cmdline 没有稳定改善 BusyBox shell 的交互问题，因此未保留。

## 后续任务

1. 固化更稳定的 shell/init 方案：
   - 修复 x86_64 Alpine rootfs，补齐 `/sbin/openrc`，或调整 `/etc/inittab` 打开 `ttyS0` getty。
   - 或在 rootfs 中提供一个正式的最小 init wrapper，负责挂载伪文件系统、打开 `/dev/ttyS0` 并进入 shell。
   - 不再恢复阶段 8 一次性脚本，除非作为临时本地调试手段。

2. 收敛 x86 timer/clocksource：
   - 补虚拟 PIT/IOAPIC timer 或等价 timer interrupt 路径。
   - 逐步去掉 `no_timer_check`、`pmtmr=0x608`、`tsc=unstable`。
   - 处理当前 APIC/PM timer 校准与 TSC watchdog 噪声的根因。

3. 收敛 PCI/IRQ routing：
   - 当前 virtio-blk completion 依赖 hardcode 的 q35 GSI 23 最小 forwarding。
   - 后续需要正式 PCI IRQ router，支持通用 INTx/MSI/MSI-X 路径。

4. 收敛 QEMU 设备拓扑：
   - 保持 `qemu-x86_64.toml` 为通用模板，不影响 NimbOS/ArceOS。
   - 如果 Linux 需要最小拓扑，应新增明确命名的 Linux 专用 QEMU config，或在 runner 层按 guest 类型选择。
   - 不应把 Linux-only 的 `-nodefaults` 等参数直接放进通用 x86_64 QEMU 模板。

5. 处理 rootfs ownership：
   - 当前默认 host `fs` 和 guest rootfs 仍通过同一外层 virtio-blk 镜像完成加载/启动验证。
   - 长期应避免 host 和 guest 同时驱动同一块外层 QEMU virtio-blk。
   - 如果需要隔离 rootfs smoke，可使用 `--rootfs` 显式传入 guest 独占镜像。

## 阶段状态

阶段 8 的最小目标已经达成：x86_64 Linux 可以使用默认配置从 managed rootfs 启动并进入
BusyBox shell。当前剩余问题不再是“能否挂载 rootfs”，而是交互 shell 的 controlling tty、
rootfs init 配套、timer/clocksource、通用 IRQ routing 和 QEMU 拓扑收敛。
