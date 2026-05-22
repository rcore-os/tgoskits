# x86_64 Linux 支持阶段 9：单 vCPU 平台收敛

阶段 9 承接阶段 8 的真实 rootfs 闭环，目标不再包含多 vCPU。当前重点是让单 vCPU
x86_64 Linux 平台从“靠启动参数绕过问题”逐步收敛到稳定、可解释、可回归的设备和中断模型。

## 范围调整

阶段 9 不实现 Linux SMP / 多 vCPU：

- 不处理 AP startup。
- 不处理多 LAPIC / IPI / per-vCPU timer 的完整 SMP 语义。
- 默认配置继续保持 `cpu_num = 1` 和 QEMU `-smp 1`。

多 vCPU 后续如果需要，应作为新的独立阶段，而不是阻塞当前 x86_64 Linux rootfs 质量收敛。

## 当前第一步

阶段 8 遗留的主要慢路径有两类，本轮已经先收掉其中一部分：

- Linux 默认 cmdline 原本包含 `no_timer_check`、`pmtmr=0x608`、`tsc=unstable`，用于绕过
  timer / clocksource 尚未完全收敛的问题；本轮移除了 `no_timer_check` 和 `pmtmr=0x608`，
  仍暂时保留 `tsc=unstable`。
- 通用 `qemu-x86_64.toml` 会暴露 QEMU 默认设备拓扑，Linux 会枚举 VGA、e1000、ICH/AHCI
  等当前 guest 不真正需要的设备。

本轮先做五个收敛：

1. x86 IRQ forwarding 从 q35 virtio-blk 专用的 GSI 23 hardcode 改为按 vIOAPIC
   redirection table 转发 0..23 的 host GSI。
2. 新增 Linux 专用 QEMU 模板 `qemu-x86_64-linux.toml`，使用 `-nodefaults` 收敛外层 QEMU
   设备拓扑，不污染 NimbOS / ArceOS 可能复用的通用 `qemu-x86_64.toml`。
3. 新增最小 x86 PIT/8254 port 设备，并通过 VMX I/O bitmap 捕获 PIT 编程端口。
4. 将 VMX preemption timer 作为 vCPU timer exit 上报给 VMM，使 guest 早期 busy wait
   中也能调度 PIT channel 0 IRQ0 检查。默认 Linux direct boot 不再依赖
   `no_timer_check` 和 `pmtmr=0x608`。
5. 新增 Linux 专用 board 配置并把 x86 Linux VM 的 kernel 改为 `image_location = "memory"`，
   避免 Axvisor host 挂载 guest rootfs，保证外层 virtio-blk 磁盘只归 Linux guest 使用。

## IRQ Routing 变更

阶段 7/8 的 virtio-blk completion 依赖固定假设：

```text
host GSI 23 -> guest vIOAPIC programmed vector
```

这只覆盖当前 q35 virtio-blk smoke 拓扑。阶段 9 第一版改为：

- 注册 host IOAPIC vectors `0x20..0x37`，对应 GSI `0..23`。
- host 外部中断到达 vCPU run loop 后，根据 `vector - 0x20` 得到 host GSI。
- 查询 guest vIOAPIC redirection table。
- 只有 guest 已经 unmask 并配置 fixed delivery vector 时才注入对应 guest vector。

这样 timer IRQ0、legacy INTx 和 virtio-blk GSI 23 都走同一条路径，后续可以继续在
vIOAPIC / PCI routing 上补语义，而不是继续增加单设备 hardcode。

## Timer 变更

去掉 `no_timer_check` 和 `pmtmr=0x608` 后，Linux 早期会在 IOAPIC timer 自检中失败：

```text
Kernel panic - not syncing: IO-APIC + timer doesn't work!
```

失败点说明当前缺的不是另一个启动参数，而是 PIT channel 0 产生的 IRQ0 能否按 Linux
期望经 IOAPIC pin 0 进入 guest。阶段 9 先补最小设备闭环：

- 新增 `x86-pit` emulated device 类型，覆盖 PIT/8254 channel 0、channel 2、command port
  和 legacy speaker control port。
- VMX I/O bitmap 捕获 `0x40..=0x43` 和 `0x61`，由设备模型解析 Linux 对 PIT 的编程。
- PIT channel 0 根据 guest 写入的 reload value 计算周期；reload `0` 按 8254 语义视为
  `65536`。
- VMX preemption timer 不再由 x86 vCPU 层静默消费，而是上报为 `AxVCpuExitReason::VTimer`。
- VMM 在 `VTimer` exit 上检查 PIT channel 0 deadline；如果到期并且 vIOAPIC GSI 0 已经
  unmask/configured，则注入 guest 配置的 IRQ0 vector。

这个实现让 Linux 通过 IOAPIC timer 自检，默认 direct-boot cmdline 可以移除
`no_timer_check` 和 `pmtmr=0x608`。它不是固定周期的 VMM 合成 tick；IRQ0 的周期来自 guest
实际写入 PIT 的 reload value。不过当前仍是最小 PIT/IOAPIC 闭环：后续还应补完整 8259/PIC、
Virtual Wire/ExtINT、LAPIC timer 和更完整的 legacy timer 语义。

`tsc=unstable` 暂时保留。对照验证显示，不带 `tsc=unstable` 时 Linux 能通过 IRQ0 自检并继续
启动，但切到 `clocksource tsc` 后 75 秒内没有进入 shell；只保留 `tsc=unstable` 时可以挂载
`/dev/vda` 并运行 `/bin/sh`。

## QEMU 拓扑变更

通用 x86_64 QEMU 模板保持不变：

```text
os/axvisor/configs/qemu/qemu-x86_64.toml
```

新增 Linux 专用模板：

```text
os/axvisor/configs/qemu/qemu-x86_64-linux.toml
```

该模板显式使用：

- `-nodefaults`
- `-no-user-config`
- `-display none`
- `-serial stdio`
- `-monitor none`
- `q35,sata=off,smbus=off,i8042=off,usb=off,graphics=off`
- `virtio-blk-pci,drive=disk0,addr=03.0`

这样 Linux rootfs 验证可以避开 QEMU 默认 VGA、NIC、AHCI、SMBus、USB、PS/2 等设备探测，
同时不改变 NimbOS / ArceOS 的默认运行环境。Linux 模板禁用 stdio monitor 复用，避免交互
shell 中误触 QEMU monitor escape 导致 `QEMU: Terminated`。`addr=03.0` 用于保持当前 MP table 中已验证的
`00:03.0 INTA -> GSI 23` 路由；没有固定 slot 时，QEMU 会把 virtio-blk 放到 `00:01.0`，
Linux 会按 MP table 转成 GSI 17，而该拓扑尚未完成 block completion smoke。

## Rootfs / Init 变更

当前 x86_64 Alpine rootfs 的 `/sbin/init` 会按 `/etc/inittab` 调用 `/sbin/openrc`，但镜像中
没有 `/sbin/openrc`；直接 `init=/bin/sh` 又没有 controlling tty，BusyBox shell 交互时会出现：

```text
/bin/sh: can't access tty; job control turned off
```

阶段 9 默认 command line 改为使用 rootfs 已有的 BusyBox getty 作为最小 init：

```text
init=/sbin/getty -- -n -l /bin/sh -L 115200 ttyS0 vt100
```

这样由 getty 打开 `ttyS0` 并拉起 `/bin/sh`，不需要向 rootfs 注入阶段脚本，也不要求补完整
OpenRC init。后续如果要支持完整发行版 init，再单独修 rootfs/OpenRC 或提供更正式的 init
配置。

Linux 专用验证命令：

```sh
cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-x86_64-linux.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-x86_64-linux.toml \
  --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

## Block Ownership 变更

之前的 x86_64 rootfs 路径沿用了 aarch64/riscv64 的便利方式：Axvisor host 启用 `fs` feature，
从外层 virtio-blk rootfs 里读取 `/guest/linux/linux-qemu`，Linux guest 又把同一块盘作为
`/dev/vda rw` 挂载。这个结构在只读 bring-up 时还能工作，但真实交互 shell 会出现两个所有者
同时操作同一个 ext4 文件系统的问题；异常退出后还会留下 journal dirty，使后续启动在 Axvisor
host mount 阶段报 `EUCLEAN`。

阶段 9 改为：

- Linux 专用 board 配置 `qemu-x86_64-linux.toml` 不启用 `fs` feature。
- x86 Linux VM 配置使用 `image_location = "memory"`，构建期把
  `tmp/qemu_x86_64_linux/linux/linux-qemu` 嵌入 Axvisor。
- 外层 QEMU 的 virtio-blk 磁盘只暴露给 Linux guest 作为 `/dev/vda`。

这让最小 Linux rootfs 运行时有清晰的块设备 ownership，不再依赖 host/guest 同盘并发访问。

## 待处理

1. 继续补完整 8259/PIC、Virtual Wire/ExtINT、LAPIC timer 和 PIT 边角语义。
2. 继续修 TSC / clocksource，使 `tsc=unstable` 也可以移除。
3. 如果移除参数失败，优先补 timer / clocksource 根因，而不是把更多 workaround 放进
   boot params。
4. 继续收敛 PCI IRQ router，后续支持更通用的 INTx，并评估 MSI/MSI-X。
5. 继续把 getty shell 路径纳入自动化 smoke；完整 `/sbin/init`/OpenRC 支持另列后续任务。
6. 后续评估是否需要 xtask 为 x86 Linux 自动创建 guest 独占 rootfs 副本，避免手动验证污染
   managed rootfs 基线。
