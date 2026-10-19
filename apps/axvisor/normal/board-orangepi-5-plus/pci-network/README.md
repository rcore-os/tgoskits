# Linux 客户机 PCIe 网卡通信

## 1. 用途

将 Orange Pi 5 Plus 的 RTL8125 PCIe 网卡交给 Linux 客户机，由客户机加载驱动、取得地址并 ping 局域网目标。验证物理网卡直通后的实际通信。

直通后 PCIe 控制器和网卡归客户机所有，宿主不再使用；受宿主中断后端限制，本例走 PCI INTx 中断而不是 MSI。客户机根文件系统使用独立 VirtIO 块设备，物理 eMMC/SD 仍由 AxVisor 使用。

## 2. 准备镜像和网络

在 tgosimages 仓库执行：

```bash
./build.sh platform orangepi-5-plus all
```

板卡原生 Linux 中应部署：

```text
/guest/
├── linux/orangepi-5-plus
└── rootfs-aarch64-orangepi-jammy-0.img
```

这里的无版本号文件名不代表任意内核均可使用。本例要求 Rockchip BSP 内核 `6.1.99-rockchip-rk3588`，并配套同一内核的 RTL8125 模块。自动用例会先打印 uname -a 和实际 release；版本不符即输出失败标记，不继续执行网卡检查。版本正确后仍需通过 PCI 设备识别、链路、DHCP 和 ping，版本号本身不替代通信验收。

正式构建入口为 tgosimages 的上述 all 命令，内核产物为 IMAGES/orangepi/linux/orangepi-5-plus，部署到板卡原生 Linux 的 /guest/linux/orangepi-5-plus。部署时记录 tgosimages 提交、构建日志和该产物 SHA256，并对比板端文件：

```bash
# 构建机：构建完成后记录来源与产物摘要
git -C "$HOME/tgosimages" rev-parse HEAD
sha256sum "$HOME/tgosimages/IMAGES/orangepi/linux/orangepi-5-plus"
# 板卡原生 Linux：在启动 AxVisor 前核对已部署文件
sha256sum /guest/linux/orangepi-5-plus
```

两边 SHA256 应相同；不能用文件名相同代替部署核对。本轮实板日志及版本记录见下文，现有板端文件的原始构建提交未确认，不将其归属于新构建批次。

Linux 内核和客户机 ext4 应来自同一轮构建。客户机文件系统内需要：

- `ip`、`ping`、`dhclient`、`timeout` 和 `modprobe`。
- 与客户机 `uname -r` 一致的模块目录和模块索引。RTL8125 驱动已内置时，无需加载模块。

tgosimages 将对应内核的模块树安装到客户机 `/usr/lib/modules/`，Jammy 的 `/lib` 别名访问同一目录。

测试板卡接入有 DHCP 的局域网，测试目标 `<reachable-lan-ip>` 可达。两个 RTL8125 网口中至少一个已接线；脚本按真实 PCI ID 和 carrier 选择网口，不依赖固定接口名。

本例客户机使用2 GiB内存、一个 Cortex-A76 vCPU（MPIDR `0x400`）。PCIe 控制器及其依赖设备由 [linux.toml](linux.toml) 分配，宿主构建配置不接管这些网卡。

## 3. 使用 Cargo 启动 AxVisor

先在构建机设置板卡服务地址，将主机和端口占位符替换为自己的环境值：

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
```

以下命令在构建机 TGOSKits 仓库根目录执行：

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
ostool board ls --server "$BOARD_SERVER"

app=apps/axvisor/normal/board-orangepi-5-plus/pci-network
# 去掉自动输入步骤和自动测试总超时，生成交互版板卡配置
sed '/^\[\[shell_check_steps\]\]/,$d' \
  "$app/ping/board-orangepi-5-plus-linux.toml" \
  | sed '/^timeout[[:space:]]*=/d' > /tmp/pci-network-manual.toml

cargo xtask axvisor board \
  --config "$app/ping/build-aarch64-unknown-none-softfloat.toml" \
  --board-config /tmp/pci-network-manual.toml \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

交互配置保留 `axvisor.vm_start=1`，不自动输入客户机命令。看到 `axvisor:/$` 后执行：

```text
vm list
vm console 1
```

等待 `root@ubuntu:~#`，以下网络操作都在这个 Linux 客户机内执行。

## 4. 在 Linux 客户机中验证

整个验证都在 Linux 客户机内完成，依次挂载伪文件系统、加载驱动、选择接线接口并 ping。

### 4.1 挂载伪文件系统

PID 1 是交互 shell，需要手工挂载 proc 和 sysfs：

```bash
mkdir -p /proc /sys
mount -t proc proc /proc 2>/dev/null || grep ' /proc proc ' /proc/mounts
test -d /sys/class/net || mount -t sysfs sysfs /sys
```

### 4.2 加载 RTL8125 驱动

先确认模块树与当前内核版本一致，再不依赖 udev、按 PCI ID 找出未绑定的 RTL8125，用内核 modalias 加载驱动：

```bash
uname -r
ls -ld /lib/modules/$(uname -r)
for p in /sys/bus/pci/devices/*; do
  if [ "$(cat "$p/vendor" 2>/dev/null)" = 0x10ec ] &&
     [ "$(cat "$p/device" 2>/dev/null)" = 0x8125 ] &&
     [ ! -e "$p/driver" ]; then
    modprobe "$(cat "$p/modalias")"
    echo MODPROBE_EXIT=$?
  fi
done
```

正常绑定的 PCI 设备具有 `driver` 链接。若模块加载返回非零，先检查模块版本和依赖；不要强制加载其他内核版本的模块。

### 4.3 选择已接线的 RTL8125 接口

```bash
# 按真实 PCI ID 找出全部 RTL8125 接口，逐个拉起
candidates=
for p in /sys/class/net/*; do
  d=$p/device
  if [ "$(cat "$d/vendor" 2>/dev/null)" = 0x10ec ] &&
     [ "$(cat "$d/device" 2>/dev/null)" = 0x8125 ]; then
    candidate=$(basename "$p")
    if ip link set "$candidate" up; then
      candidates="$candidates $candidate"
    fi
  fi
done

# 最多等待10秒，选择第一个 carrier=1 的已接线接口
n=
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  for candidate in $candidates; do
    if [ "$(cat /sys/class/net/$candidate/carrier 2>/dev/null)" = 1 ]; then
      n=$candidate
      break
    fi
  done
  [ -z "$n" ] || break
  sleep 1
done
echo NET_DEVICE=$n
```

`NET_DEVICE` 必须非空。没有候选接口时检查 PCI 设备和驱动；有接口但 carrier 始终为0时检查接线。找到接口后才进行下一步。

### 4.4 取得地址并 ping

```bash
# 复核设备身份：vendor 0x10ec、device 0x8125，驱动已绑定
d=/sys/class/net/$n/device
cat "$d/vendor"
cat "$d/device"
readlink -f "$d"
readlink -f "$d/driver"

# 没有全局 IPv4 地址时向 DHCP 申请
ip link set "$n" up
if ! ip -4 -o addr show dev "$n" scope global | grep -q ' inet '; then
  timeout 45 /sbin/dhclient -1 "$n"
  echo DHCP_EXIT=$?
fi

ip -brief address
ip route
# ping 必须从选中的物理网口发出
PING_TARGET='<reachable-lan-ip>'
ping -n -I "$n" -c 5 -w 10 -W 2 "$PING_TARGET"
echo PING_EXIT=$?
sync
```

预期 PCI ID 为 `10ec:8125`，驱动已绑定，接口具有 IPv4 地址。ping 必须从选中的接口发出，返回0、收到5个回复且丢包率为0%。

更换目标时，手动执行修改 `PING_TARGET`，自动测例修改 [板卡配置](ping/board-orangepi-5-plus-linux.toml) 中的地址；`BOARD_SERVER` 只用于连接板卡服务。

## 5. 查看结果与结束测试

以下条件全部满足才通过：

1. 客户机识别到 RTL8125，且驱动正确绑定。
2. 接线接口 carrier=1，并取得 IPv4 地址。
3. 经该物理网口完成5次 ping，返回0、无丢包。

仅列出 PCI 设备、驱动加载成功或有 IP 地址，都不足以证明通信通过。本配置的通过结果只覆盖 INTx 路径。

执行 `sync` 后按 Ctrl+X、h 返回 AxVisor，再按 Ctrl+A、x 退出板卡会话。在构建机用 `ostool board ls` 确认板卡重新可用。

## 6. 自动执行

在构建机仓库根目录执行：

```bash
cargo xtask axvisor app board -g normal \
  -t board-orangepi-5-plus/pci-network/ping \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

自动执行相同的设备识别、驱动绑定、接线口选择、DHCP 和 ping。要求打印 `AXVISOR_PCI_NET_PING_PASSED`，且 Cargo 返回0。

## 7. 配置文件

本目录分为客户机定义和 ping 子测例两层，手动流程从子测例的板卡配置裁剪而来：

| 文件 | 作用 |
| --- | --- |
| [linux.toml](linux.toml) | 客户机内核、独立磁盘、内存及 PCIe 设备分配 |
| [ping/build-aarch64-unknown-none-softfloat.toml](ping/build-aarch64-unknown-none-softfloat.toml) | 构建 AxVisor 并注册 Linux 客户机 |
| [ping/board-orangepi-5-plus-linux.toml](ping/board-orangepi-5-plus-linux.toml) | 启动参数、客户机检查和通过条件 |

## 8. 实板验证记录

2026-10-16 在本轮使用的板卡服务查询登记信息。服务地址按自己的环境填写：

```bash
ostool board ls --server "$BOARD_SERVER"
```

查询输出摘录（UTC 08:17:16，AVAILABLE=1、TOTAL=1）：

```text
BOARD TYPE             AVAILABLE  TOTAL
BallPick-Robot-Rk3588          1      1
```

BallPick-Robot-Rk3588 是板卡服务登记的物理类型，板卡配置的 board_type、本文命令的 -b 和 nightly 命令的 --board-type 均使用它。orangepi-5-plus-linux 是 --board 选择的测例逻辑名称，两者用途不同。

同轮 PCI ping 复测实际 release 为 6.1.99-rockchip-rk3588。以下为日志摘录，时间戳是构建机接收到串口输出的 UTC 时间：

```text
[2026-10-16T08:18:18.537+00:00] root@ubuntu:~#root@ubuntu:~#PCI_NET_KERNEL_RELEASE=6.1.99-rockchip-rk3588 expected=6.1.99-rockchip-rk3588
[2026-10-16T08:18:18.537+00:00] AXVISOR_PCI_NET_KERNEL_OK
[2026-10-16T08:18:47.903+00:00] 5 packets transmitted, 5 received, 0% packet loss, time 4009ms
[2026-10-16T08:18:47.935+00:00] AXVISOR_PCI_NET_PING_PASSED
```

Cargo 外层退出码为0。本轮证明该版本 BSP 在当前板卡上的设备识别、链路、DHCP 和 ping 均通过；板端文件的原始构建提交未确认，因此不宣称其对应某一轮 tgosimages 构建。后续部署按第2节记录构建提交和 SHA256；用例还会在每次运行时校验实际内核版本。
