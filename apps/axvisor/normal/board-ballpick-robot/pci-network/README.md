# Linux 客户机 PCIe 网卡通信

## 测试用途

将目标板卡的 RTL8125 PCIe 网卡交给 Linux 客户机，由客户机加载驱动、取得地址并 ping 局域网目标。验证物理网卡直通后的实际通信。

直通后 PCIe 控制器和网卡归客户机所有，宿主不再使用；受宿主中断后端限制，本例走 PCI INTx 中断而不是 MSI。客户机根文件系统使用独立 VirtIO 块设备，物理 eMMC/SD 仍由 AxVisor 使用。

## 边界

交互配置不自动注入命令或判定结果。必须检查客户机中的物理 RTL8125 设备、所选网口及该网口发出的 ping；宿主或其他接口连通不能替代客户机物理网卡验收。

## 运行前确认

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

两边 SHA256 应相同；不能用文件名相同代替部署核对。板端文件来源不明时，应重新部署可追溯的配套产物并核对摘要。

Linux 内核和客户机 ext4 应来自同一轮构建。客户机文件系统内需要：

- `ip`、`ping`、`dhclient`、`timeout` 和 `modprobe`。
- 与客户机 `uname -r` 一致的模块目录和模块索引。RTL8125 驱动已内置时，无需加载模块。

tgosimages 将对应内核的模块树安装到客户机 `/usr/lib/modules/`，Jammy 的 `/lib` 别名访问同一目录。

测试板卡接入有 DHCP 的局域网，测试目标 `<reachable-lan-ip>` 可达。两个 RTL8125 网口中至少一个已接线；脚本按真实 PCI ID 和 carrier 选择网口，不依赖固定接口名。

本例客户机使用2 GiB内存、一个 Cortex-A76 vCPU（MPIDR `0x400`）。PCIe 控制器及其依赖设备由 [configs/linux.toml](configs/linux.toml) 分配，宿主构建配置不接管这些网卡。

## 启动

将命令中的 `<板卡类型>`、`<服务地址>`、`<端口>` 替换为实际值。

以下命令从 TGOSKits 仓库根目录执行，配置文件直接使用仓库内路径。

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
cargo xtask board ls --server <服务地址> --port <端口>

cargo xtask axvisor board \
  --config apps/axvisor/normal/board-ballpick-robot/pci-network/ping/build-aarch64-unknown-none-softfloat.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/pci-network/configs/uboot-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

交互配置保留 `axvisor.vm_start=1`，不自动输入客户机命令。看到 `axvisor:/$` 后执行：

```text
vm list
vm console 1
```

等待 `root@ubuntu:~#`，以下网络操作都在这个 Linux 客户机内执行。

交互配置不设置终端总超时，空的 `shell_check_steps` 和 `fail_regex` 将输入与结果判定交给测试者。`Ctrl+X` 后按 `h` 返回 AxVisor；`Ctrl+A` 后按 `x` 释放板卡。外层命令返回 0 仅表示会话正常结束，不替代后续功能判定。

## 场景 1：物理网卡通信

整个验证都在 Linux 客户机内完成，依次挂载伪文件系统、加载驱动、选择接线接口并 ping。

### 挂载伪文件系统

PID 1 是交互 shell，需要手工挂载 proc 和 sysfs：

```bash
mkdir -p /proc /sys
mount -t proc proc /proc 2>/dev/null || grep ' /proc proc ' /proc/mounts
test -d /sys/class/net || mount -t sysfs sysfs /sys
```

### 加载 RTL8125 驱动

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

### 选择已接线的 RTL8125 接口

按 PCI vendor/device ID 找到 RTL8125 接口，再从中选择有载波的端口，不依赖接口编号恰好固定。

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

### 取得地址并 ping

为选定的物理接口取得 IPv4 地址，再显式从该接口 ping 目标，避免流量经由其他网卡而产生错误的通过结论。

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

更换目标时，手动执行修改 `PING_TARGET`，自动测例修改 [板卡配置](ping/board-ballpick-robot-linux.toml) 中的地址；构建机命令的 `--server` 和 `--port` 只用于连接板卡服务。

## 判定

以下条件全部满足才通过：

1. 客户机识别到 RTL8125，且驱动正确绑定。
2. 接线接口 carrier=1，并取得 IPv4 地址。
3. 经该物理网口完成5次 ping，返回0、无丢包。

仅列出 PCI 设备、驱动加载成功或有 IP 地址，都不足以证明通信通过。本配置的通过结果只覆盖 INTx 路径。

## 结束

执行 `sync` 后按 Ctrl+X、h 返回 AxVisor，再按 Ctrl+A、x 退出板卡会话。在构建机执行 `cargo xtask board ls --server <服务地址> --port <端口>`，确认板卡重新可用。

## 自动执行（可选）

在构建机仓库根目录执行：

```bash
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/pci-network/ping \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

自动执行相同的设备识别、驱动绑定、接线口选择、DHCP 和 ping。要求打印 `AXVISOR_PCI_NET_PING_PASSED`，且 Cargo 返回0。

## 配置说明

本目录分为客户机定义和 ping 子测例两层，交互配置与 ping 子测例共用相同启动参数：

| 文件 | 作用 |
| --- | --- |
| [configs/uboot-shell.toml](configs/uboot-shell.toml) | 交互启动，不注入命令或自动判定 |
| [configs/linux.toml](configs/linux.toml) | 客户机内核、独立磁盘、内存及 PCIe 设备分配 |
| [ping/build-aarch64-unknown-none-softfloat.toml](ping/build-aarch64-unknown-none-softfloat.toml) | 构建 AxVisor 并注册 Linux 客户机 |
| [ping/board-ballpick-robot-linux.toml](ping/board-ballpick-robot-linux.toml) | 启动参数、客户机检查和通过条件 |
