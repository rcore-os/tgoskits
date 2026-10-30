# RK3588 板卡上的 AxVisor 应用

本目录集中维护客户机功能、IVC、网络、机器人业务和任务切换测量。先选择下表中的场景，再按对应 README 准备资源和运行；不同场景不能同时占用同一块实体板卡或共享同一份可写客户机磁盘。

## 场景选择

场景使用不同的设备分配和判定方式。进入客户机 shell 只证明启动成功，完整通过条件以各应用的结果标记和退出码为准。

### 功能与业务

功能场景提供仓库内交互配置，使用 `cargo xtask axvisor board` 启动后按文档手工操作；自动用例则使用 `cargo xtask axvisor app board`。

| 场景 | 验证内容 | 操作说明 |
| --- | --- | --- |
| `kernel-types` | ArceOS、Linux、StarryOS、Zephyr 功能及 cyclictest、LMBench、IOZone | [客户机检查](kernel-types/README.md) |
| `ivc` | 三种客户机之间的消息通信、超时、取消、恢复及传输基准 | [IVC 通信](ivc/README.md) |
| `pci-network` | Linux 直通 PCIe 网卡、驱动、DHCP 和 ping | [物理网络](pci-network/README.md) |
| `virtio-net-peer` | 64 台 ArceOS 的虚拟网络及同次启动下的移除验证 | [虚拟网络](virtio-net-peer/README.md) |
| `aka-starry-shell` | StarryOS 感知与 Zephyr 控制端协作 | [AKA 联调](aka-starry-shell/README.md) |
| `robot-real-linux`、`robot-real-starry` | 单个客户机通过 USB 运行摄像头、NPU 与完整捡球程序 | [Linux 机器人检查](robot-real-linux/README.md)、[StarryOS 机器人检查](robot-real-starry/README.md) |
| `linux-guest-checks` | 单个直通 Linux 的 NPU、图片推理及基础功能 | [Linux 交互检查](linux-guest-checks/README.md) |
| `triple-vm-test` | Linux、StarryOS、Zephyr 同次启动下的功能和性能检查 | [三客户机交互检查](triple-vm-test/README.md) |

`linux-guest-checks`、`kernel-types/arceos` 和 `triple-vm-test` 只提供手工检查，不通过自动用例发现和判定。AKA 还要求相机、控制设备和真实校准文件；其他场景通过不代表机器人业务可用。

### 基准测量

基准按固定工作量采集结果，不能通过只看启动信息代替测量完成。

| 场景 | 工作量 | 正式入口 |
| --- | --- | --- |
| [ivc/benchmark](ivc/benchmark/README.md) | 四组各 100 次传输，核对字节数、块数和两端结果 | `cargo xtask axvisor app board` |
| [task-switch](task-switch/README.md) | 30 轮，每轮 1,000 万次任务切换 | `cargo xtask ktest board -p axvm --test task_switch` |

`task-switch` 依赖 ktest 安装宿主 vCPU 的 FIFO 策略；运行时使用该应用 README 的完整命令。

## 配置布局

各应用的 `configs/` 集中保存客户机描述、客户机构建参数和手动启动配置。文件只调整位置，客户机资源和测试参数保持原样；已有的 `linux-guest-checks/configs/` 沿用原布局。

| 文件类型 | 位置 | 原因 |
| --- | --- | --- |
| 客户机描述，如 `linux.toml`、`starry-smp1.toml` | 应用的 `configs/` | 集中维护镜像、磁盘、内存和设备分配 |
| 客户机构建参数，如 `guest-build.toml`、`arceos-guest-build.toml` | 应用的 `configs/` | 由正式构建准备声明或显式命令引用 |
| `uboot-shell.toml`、按操作系统划分的交互配置 | 应用的 `configs/` | 通过 `--board-config` 显式指定 |
| 自动发现的 `board-*.toml`、`build-*.toml` | 原有应用或子测例目录 | 保留现有发现、构建和选择入口 |
| `guest-builds.toml` | 与对应运行配置相邻 | 运行器从该位置读取客户机构建准备声明 |

`vm_configs` 使用工作区路径引用移动后的客户机描述；`arceos_build_configs` 则相对 `guest-builds.toml` 所在目录解析。新增场景时按这两个字段各自的路径语义填写，不能直接复制另一目录的相对路径。

## 运行环境

命令从 TGOSKits 仓库根目录执行。启动和连接板卡时，显式传入 `--board-type <板卡类型> --server <服务地址> --port <端口>`，将占位符替换为实际值。

### 服务与工具链

先查询服务中的板卡类型和可用性，交叉工具链路径按本机安装位置调整。`board ls` 查询整个服务的板卡列表，只传服务地址和端口；实际启动时再指定板卡类型。

以下命令从 TGOSKits 仓库根目录执行，配置文件直接使用仓库内路径。

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
cargo xtask board ls --server <服务地址> --port <端口>
cargo xtask axvisor app board -g normal -t board-ballpick-robot --list \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

`--list` 只展示自动用例，不构建或占用板卡。交互配置使用 `configs/` 下不以 `board-` 开头的文件名，通过 `--board-config` 显式指定。

### 镜像与文件系统

各应用的 `vm_configs` 决定打包的客户机描述；`/guest` 绝对路径由板卡 AxVisor 宿主读取。Linux 和 StarryOS 常用的独立磁盘分别是 `rootfs-aarch64-orangepi-jammy-0.img` 和 `-1.img`。部署时先备份原文件，再配套替换内核、DTB 和 rootfs，执行 `sync` 后释放原生 Linux 会话。

`linux-guest-checks` 使用 `/guest/axvisor/` 下的独立内核、initramfs 和推理包，并让 Linux 访问板卡原生根分区，资源准备以该文档为准。ArceOS 网络与 task-switch 客户机由本仓库构建；其余发布资源和工具由 tgosimages 提供。

## 结果与会话

交互入口负责保持串口，自动入口负责按配置执行和判定；两种方式的退出码含义不同。

### 通过条件

手工运行时，保存客户机命令、输出和退出码，并按各 README 的完整检查项判定。自动运行时，要求目标成功标记与外层命令返回 0 同时成立。计时结果只描述本次环境，不能将某次测量值写成所有板卡必须达到的值。

### 退出与记录

`Ctrl+X` 后按 `h` 返回 AxVisor，`Ctrl+A` 后按 `x` 结束串口会话。Linux、StarryOS 的文件写入完成后先执行 `sync`；涉及直通设备的场景按应用说明执行关机或板级复位，不用 VM 重置替代设备回收。

记录源码版本、发布版本、内核与 DTB 摘要、磁盘来源、运行命令及日志。历史通过记录不能替代修改后的实测；启动停滞、退出失败和未完成的阶段应独立记录。

### 板卡复核入口

从仓库根目录使用仓库内的 StarryOS 交互配置启动宿主，直接指定配置路径和板卡连接参数。该配置启动 VM2，但不会注入客户机测试命令。

```bash
cargo xtask axvisor board \
  --config apps/axvisor/normal/board-ballpick-robot/kernel-types/starry/functional/build-aarch64-unknown-none-softfloat.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/kernel-types/configs/starry-uboot-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

在宿主执行 `vm console 2`，等待真正的 `root@starry:~#` 后，在客户机执行以下功能检查。命令在独立 shell 中运行，失败时不打印通过标记。

```sh
sh -eu <<'GUEST_CHECK'
mkdir -p /proc /sys /tmp
mount -t proc proc /proc 2>/dev/null || grep -q ' /proc proc ' /proc/mounts
[ -d /sys/class/net ] || mount -t sysfs sysfs /sys
uname -a
grep '^processor' /proc/cpuinfo
printf '%s\n' AXVISOR_RESET_RW > /tmp/axvisor-reset-rw
test "$(cat /tmp/axvisor-reset-rw)" = AXVISOR_RESET_RW
ping -n -c 5 -W 2 -w 15 10.3.10.60
sync
printf '\nVM_RESET_FUNCTIONAL_PASSED\n'
GUEST_CHECK
```

看到通过标记后，按 `Ctrl+X`、`h` 返回宿主，执行 `vm reset 2`、`vm console 2`，再等待客户机 shell 并重复检查，共复位三轮。`reset and started successfully` 只表示宿主完成了复位与启动请求，不能代替重启后的功能结果；示例 ping 地址应与当前网络中的可达测试目标一致。

冷启动 cyclictest 使用已有自动用例；其判定仍要求完整采样、用例成功标记和外层退出码，不放宽失败匹配或启动截止时间。

```bash
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/kernel-types/starry/cyclictest \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```
