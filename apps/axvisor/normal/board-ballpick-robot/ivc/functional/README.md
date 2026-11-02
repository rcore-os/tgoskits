# Linux、StarryOS 与 Zephyr IVC 通信

## 测试用途

一次启动三个客户机，分别验证 Linux ↔ Zephyr、Linux ↔ StarryOS、Zephyr ↔ StarryOS 的请求与回复。三条链路保持在同一次 AxVisor 启动中完成。

IVC（inter-VM communication）是 AxVisor 提供的客户机间通信机制：每个通道由一块双方共享的内存区和通知中断构成，客户机分别以发布者和订阅者端点收发消息。Linux 经 `axvisor` 内核模块的 `/dev/axivc` 使用端点，StarryOS 以普通流式读写访问，Zephyr 使用 `ivc` shell 命令。两端的测试程序都来自 ivc-sdk：Linux 和 StarryOS 运行用户态 `ivc-roundtrip`，Zephyr 的 `ivc` 命令集（`subscriber`、`ivc_demo`、`latency` 等）编译在 orangepi-shell 镜像内。

`ivc` 命令同步等待测试完成才返回 shell，结束时打印 `state=passed/failed/canceled result=...`；默认总超时 30000 毫秒，可用毫秒参数指定，等待期间按 Ctrl+C 主动取消。

| VM | 客户机             | 本例使用的程序                                 |
| -- | ------------------ | ---------------------------------------------- |
| 1  | Linux Ubuntu shell | `/usr/bin/ivc-roundtrip` 和 `axvisor` 模块 |
| 2  | StarryOS           | `/usr/bin/ivc-roundtrip`                     |
| 3  | Zephyr shell       | `ivc subscriber`、`ivc ivc_demo`           |

每条链路由一端发布通道、另一端订阅，校验5条请求及对应回复。通道由发布者 VM ID 和 key 共同标识，因此 VM1 的 `0x100` 与 VM3 的 `0x100` 是不同通道。

## 边界

手工流程通过 `cargo xtask axvisor board` 加载 `configs/uboot-shell.toml`，只启动 AxVisor。客户机启动、控制台切换、发布、订阅、取消和结果检查均由测试者手工完成。三条通信链路及取消后的恢复分别判定；通信成功不代表独立的中断时延基准通过。

目录中另有 `board-*.toml` 自动测试配置，运行方式见文末“自动执行”。手工流程不使用其中的自动输入或通过条件。

## 运行前确认

在 tgosimages 仓库执行：

```bash
./build.sh platform orangepi-5-plus all
```

将同一轮构建的镜像和两份独立客户机 ext4 部署到板卡原生 Linux：

```text
/guest/
├── linux/
│   └── orangepi-5-plus
├── starry/orangepi-5-plus
├── zephyr/
│   ├── orangepi-5-plus
│   └── orangepi-5-plus.dtb
├── rootfs-aarch64-orangepi-jammy-0.img
└── rootfs-aarch64-orangepi-jammy-1.img
```

Linux 以 `-0.img` 为根文件系统，StarryOS 以 `-1.img` 为根文件系统，两台客户机都从独立 ext4 中运行 `/usr/bin/ivc-roundtrip`。Linux 的匹配内核模块位于 `/lib/modules/$(uname -r)/extra/axvisor.ko`，由 `modprobe axvisor` 加载。tgosimages 的 IVC 构建将驱动和索引发布到正式模块树，再写入客户机 ext4。Zephyr 不挂磁盘。

Linux 提示符为 `root@linux-ivc:~#`。Linux 从 `/dev/vda` 挂载 ext4，不加载旧 IVC cpio。Zephyr 二进制与 DTB 必须配套。

## 启动

将命令中的 `<板卡类型>`、`<服务地址>`、`<端口>` 替换为实际值。

在构建机的 TGOSKits 仓库根目录执行以下命令，直接指定配置文件路径：

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
cargo xtask board ls --server <服务地址> --port <端口>

cargo xtask axvisor board \
  --config apps/axvisor/normal/board-ballpick-robot/ivc/functional/build-aarch64-unknown-none-softfloat.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/ivc/functional/configs/uboot-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

构建配置的 `vm_configs` 将三台客户机的定义打包进 AxVisor；交互配置不设置 `axvisor.vm_start`，不会自动启动客户机。看到 `axvisor:/$` 后，在 AxVisor 控制台逐条执行：

```text
vm start 1
vm start 2
vm start 3
vm list
```

三条 `vm start` 命令分别创建并启动 builtin 清单中对应 ID 的客户机，`vm list` 应显示三台客户机均为 `running`。启动失败时先处理启动错误，再执行通信检查。`vm console ID` 只接入已经运行的客户机，不负责创建或启动。

从客户机返回 AxVisor，按 Ctrl+X，松开后按 h；再用 `vm console ID` 进入另一个客户机。下面每段标明输入位置。

交互配置不设置终端总超时，空的 `shell_check_steps` 和 `fail_regex` 将输入与结果判定交给测试者。`Ctrl+X` 后按 `h` 返回 AxVisor；`Ctrl+A` 后按 `x` 释放板卡。外层命令返回 0 仅表示会话正常结束，不替代后续功能判定。

## 场景 1：三条通信链路

按 Linux 与 Zephyr、Linux 与 StarryOS、Zephyr 与 StarryOS 的顺序检查。各段分别准备发布端和订阅端，完成后核对两端消息与退出码。

### Linux ↔ Zephyr

在 AxVisor 输入：

```text
vm console 1
```

在 Linux 的 `root@linux-ivc:~#` 下执行：

```bash
# PID 1 是交互 shell，需要手工挂载 proc 和 sysfs
mkdir -p /proc /sys /tmp
mount -t proc proc /proc 2>/dev/null || grep ' /proc proc ' /proc/mounts
test -d /sys/class/net || mount -t sysfs sysfs /sys
# 根文件系统应为 /dev/vda 上的 ext4，而不是旧的 IVC cpio
findmnt -n -o SOURCE,FSTYPE /
# IVC 内核模块已由 tgosimages 发布到客户机模块树
modprobe axvisor
echo MODPROBE_EXIT=$?
# 字符设备就绪后用户态才能创建通道
test -c /dev/axivc
echo DEVICE_EXIT=$?
# 发布端在后台运行，期间可以把控制台切到订阅端
( /usr/bin/ivc-roundtrip publish 0x100 linux-to-zephyr zephyr-to-linux \
  || echo IVC_LINK_FAILED ) >/tmp/linux-zephyr.log 2>&1 </dev/null &
```

根文件系统应为 `/dev/vda` 的 ext4，`MODPROBE_EXIT` 和 `DEVICE_EXIT` 均为0。`ivc-roundtrip` 的两个参数是请求和回复方向的消息前缀：发布端发送5条以 `linux-to-zephyr` 开头的请求，逐条等待并校验 `zephyr-to-linux` 开头的回复。发布端在后台运行，控制台可以切走。

返回 AxVisor，执行 `vm console 3`。Zephyr 的 `ivc subscriber` 充当该通道的订阅端，收到请求即回复，5条校验完成后返回，在 `zephyr:~$` 下执行：

```text
ivc subscriber
```

预期输出 `state=passed result=0`。随后返回 Linux，查看：

```bash
cat /tmp/linux-zephyr.log
```

必须看到：

```text
IVC_ROUNDTRIP_PASSED role=publish key=0x100 count=5
```

### Linux ↔ StarryOS

继续在 Linux 中启动第二个发布端：

```bash
( /usr/bin/ivc-roundtrip publish 0x101 linux-to-starry starry-to-linux \
  || echo IVC_LINK_FAILED ) >/tmp/linux-starry.log 2>&1 </dev/null &
```

返回 AxVisor，执行 `vm console 2`，在 StarryOS shell 中执行：

```bash
mkdir -p /tmp
# 参数依次为：对端 VM ID、通道 key、两个方向的消息前缀
/usr/bin/ivc-roundtrip subscribe 1 0x101 linux-to-starry starry-to-linux
echo SUBSCRIBE_EXIT=$?
```

订阅端连接 VM1 发布的 `0x101` 通道，每收到一条请求回复一条，完成5条后通过。预期返回 0，并打印：

```text
IVC_ROUNDTRIP_PASSED role=subscribe key=0x101 count=5
```

返回 Linux，执行：

```bash
cat /tmp/linux-starry.log
```

Linux 也必须打印 `IVC_ROUNDTRIP_PASSED role=publish key=0x101 count=5`。

### Zephyr ↔ StarryOS

进入 Zephyr，执行 `ivc ivc_demo`。该命令让 Zephyr 充当发布端，发布通道后批量发送5条请求并等待订阅端接入：

```text
ivc ivc_demo
```

看到 `ORANGEPI_ZEPHYR_IVC_DEMO_WAITING` 后，表示发布端已就绪。此时按 Ctrl+X、h 返回 AxVisor，进入 VM2，在 StarryOS 执行：

```bash
mkdir -p /tmp
# 订阅端在后台运行，结果最终从日志检查
( /usr/bin/ivc-roundtrip subscribe 3 0x100 zephyr-to-starry starry-to-zephyr \
  || echo IVC_LINK_FAILED ) >/tmp/starry-zephyr.log 2>&1 </dev/null &
```

返回 Zephyr 后执行顶层 `status` 命令，查询 `ivc_demo` 的进度：

```text
status
```

等待 `state=passed result=0`。再进入 StarryOS 查看：

```bash
cat /tmp/starry-zephyr.log
```

必须包含 `IVC_ROUNDTRIP_PASSED role=subscribe key=0x100 count=5`。

## 场景 2：超时和主动停止

通信通过后，在 Zephyr 侧依次验证无发布者超时、Ctrl+C 主动取消和取消后的恢复。

### 无发布者超时

完成三条链路后，在 Zephyr 中执行；参数是超时毫秒数，省略时默认 30000：

```text
ivc subscriber 1000
```

此时 Linux 发布端已经结束。本项预期等待1000～1999 ms 后返回：

```text
elapsed_ms=... timeout_ms=1000
state=failed result=-116
```

这是明确无发布者时的超时检查；正常通信阶段出现该结果仍属于失败。

### 主动取消与恢复

再执行：

```text
ivc subscriber 30000
```

看到 `TEST_RUNNING timeout_ms=30000` 后按 Ctrl+C。预期在5秒内结束，打印 `state=canceled result=-140`。最后执行：

```text
ivc latency
```

要求 `state=passed result=0`，确认取消后工作线程仍可使用。`irq latency` 是独立的中断测量功能，不属于本通信验收。

## 手工检查判定

通信通过需要三条链路两端的结果都通过，不能只看 shell 提示符或一端的输出。缺少回复、内容不匹配、通信阶段超时，或出现 `IVC_ROUNDTRIP_FAILED` / `IVC_LINK_FAILED`，都判失败。

在 AxVisor 执行：

```text
vm list
```

三台客户机应仍为 `running`。超时场景应返回 `state=failed result=-116`，主动取消应返回 `state=canceled result=-140`，随后 `ivc latency` 应返回 `state=passed result=0`。这些预期的超时、取消结果只适用于对应场景，不能作为正常通信阶段的通过结果。

## 结束

完成后分别在 Linux、StarryOS 中执行 `sync`，等待命令返回，再按 Ctrl+X、h 返回 AxVisor，最后按 Ctrl+A、x 释放板卡会话。

本例的 Zephyr 持有直通设备及 IVC 端点，宿主会拒绝 `vm stop 3`、`vm reset 3` 和 `vm delete --force 3`；这些命令不能代替板级复位。重新运行整套检查时释放当前会话，再重新启动本例。

## 自动执行（可选）

在构建机的 TGOSKits 仓库根目录执行，直接指定功能检查的子目录：

```bash
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/ivc/functional \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

该入口使用 `board-ballpick-robot-ivc.toml`，由 `axvisor.vm_start=1,2,3` 自动启动客户机，并注入测试命令。运行器自动执行三条通信链路、超时、Ctrl+C 取消和恢复检查，还验证 Zephyr 持有直通设备及 IVC 端点时，停止、复位和强制删除操作被拒绝，客户机仍能执行 `ivc latency`。全部检查完成且 Cargo 返回 0 才通过。

## 配置说明

本目录的配置覆盖宿主构建、板级自动测试和三台客户机，交互配置与自动测试配置分别维护，共用同一组客户机定义：

| 文件                                                                                  | 作用                                         |
| ------------------------------------------------------------------------------------- | -------------------------------------------- |
| [build-aarch64-unknown-none-softfloat.toml](build-aarch64-unknown-none-softfloat.toml) | AxVisor 构建参数及三台客户机列表             |
| [configs/uboot-shell.toml](configs/uboot-shell.toml)                                   | 只启动宿主，客户机启动与检查由测试者手工完成 |
| [board-ballpick-robot-ivc.toml](board-ballpick-robot-ivc.toml)                       | 启动参数、自动输入和通过条件                 |
| [configs/linux-smp1.toml](configs/linux-smp1.toml)                                     | Linux 内核和作为根文件系统的第0份 ext4       |
| [configs/starry-smp1.toml](configs/starry-smp1.toml)                                   | StarryOS 和第1份 ext4                        |
| [configs/zephyr-smp1.toml](configs/zephyr-smp1.toml)                                   | Zephyr、配套 DTB、UART6 和虚拟控制台         |
