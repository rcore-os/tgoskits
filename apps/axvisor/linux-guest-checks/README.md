# AxVisor Linux 直通客户机功能检查

## 测试用途

在捡球机器人板（OrangePi-5-Plus 硬件，板卡类型 `BallPick-Robot`）上通过 AxVisor 启动**单个直通 Linux 客户机**，由测试者进入客户机后手工执行命令，检查客户机启动、RK3588 NPU 驱动绑定、NPU 信息查询、YOLOv8 图片推理，以及 ssh / CPU 信息 / 网络 ping / 文件读写 / 软件包安装等 Linux 基础功能是否可用。

本应用**不含自动化**：没有启动器、没有自动命令注入、没有通过/失败判定。仓库只提供启动配置和本文档，命令与现象由测试者自行执行和判断。

本目录是**直通单 Linux 客户机**这一场景的检查集合，后续检查（如 syslitest 等）继续加在本页的并列章节中；同一份 `configs/` 服务全部章节。

## 边界

- 只验证链路可用：客户机启动、NPU 驱动绑定、信息/频点/负载可查询、NPU 能跑通一次 YOLOv8 图片推理、Linux 基础功能（ssh / cpuinfo / ping / 读写 / apt）可用。
- 不验证识别准确率，不与预设答案比对；未检测到目标时输出 `Detections: 0` 也算推理正常完成。
- 不修改 NPU 频率、不复位 NPU。
- 客户机为 `guest_type = "passthrough"` 单客户机，宿主设备在根基线之上**直通**（见 `configs/linux.toml` 中 `passthrough` 列表的语义说明），不是最小设备白名单。
- 功能检查的网络项依赖板卡所连网络：DHCP 可用、网关与外部 apt 镜像（repo.huaweicloud.com）可达。换网络环境时以现场实际为准。

## 运行前确认

本用例的板端输入全部放在板卡 rootfs 的 `/guest/axvisor/` 下，自己维护一份，**不与仓库里其他用例共用**板卡上已有的目录。这些资产仓库不生产也不搬运，**每块板第一次使用前放一次**，之后重复运行不需要再动。

| 路径 | 内容 | 用于 |
| --- | --- | --- |
| `/guest/axvisor/linux/orangepi-5-plus-6.1.99` | 客户机内核 | 启动客户机，必需 |
| `/guest/axvisor/linux/initramfs.cpio` | 客户机初始内存盘 | 启动客户机，必需 |
| `/guest/axvisor/npu/rknn-yolov8-image/` | YOLOv8 推理包 | 场景 2 |

### 首次准备

下面两步的**目标位置都在板卡上**。第 1 步在板卡 Linux 里执行，进入方式：

```bash
cargo xtask board connect -b BallPick-Robot --server <服务地址> --port <端口>
```

第 2 步在工作站编译、再传上去。

#### 1. 客户机内核与初始内存盘（必需）

两块资产都在板卡上现成，就地复制到新路径：

```bash
sudo mkdir -p /guest/axvisor/linux

cd /guest/linux
sudo cp -a orangepi-5-plus-6.1.99 /guest/axvisor/linux/

cd /boot
sudo cp -a initrd.img-6.1.99-rockchip-rk3588 /guest/axvisor/linux/initramfs.cpio

sudo chmod -R a+rX /guest/axvisor
sync
```

放置结果，两个文件都应存在且非空（内核约 41 MB，初始内存盘约 8.7 MB）：

```bash
ls -l /guest/axvisor/linux/
```

两点注意：

- 内核还在被 `pci-network`、`robot-real-linux`、`test-suit/axvisor/` 等用例引用着，所以这里是**复制**，不要挪走原路径。
- 初始内存盘**不在 `/guest/` 下，而在 `/boot/`**，文件名随内核版本变化。上面写的是当前 6.1.99 内核对应的名字；换内核时要先在 `/boot/` 里确认新名字，不要沿用旧的。

#### 2. YOLOv8 推理包（场景 2）

在工作站仓库根目录编译，再把产物整个目录放到板卡的 `/guest/axvisor/npu/` 下。

```bash
cd apps/axvisor/linux-guest-checks/rknn-yolov8-image
./build.sh
```

编译成功时脚本最后一行打印 `Runtime package: .../install/rknn-yolov8-image`。确认产物内容：

```bash
ls install/rknn-yolov8-image/
# 期望：README.md  images  lib  licenses  model  rknn_yolov8_images  yolov8_demo.sh
```

传到板卡。板卡 IP 通常**只在 ostool 会话持有期间可达**（会话一释放，板卡重启、IP 消失），先另开一个 `cargo xtask board connect` 会话把板卡拿住，传完释放；会话是排他的，别和 `axvisor board` 同时跑。

`/guest/` 归 root 所有，`orangepi` 用户写不进去，所以先落到 `/tmp` 再由 root 移动：

```bash
tar czf /tmp/rknn-yolov8-image.tar.gz -C apps/axvisor/linux-guest-checks/rknn-yolov8-image/install rknn-yolov8-image

BOARD_IP=<板卡 IP>
cat /tmp/rknn-yolov8-image.tar.gz | ssh orangepi@${BOARD_IP} 'cat > /tmp/rknn-yolov8-image.tar.gz'
ssh orangepi@${BOARD_IP} '
  printf "%s\n" orangepi | sudo -S -p "" sh -c "
    mkdir -p /guest/axvisor/npu
    rm -rf /guest/axvisor/npu/rknn-yolov8-image
    tar xzf /tmp/rknn-yolov8-image.tar.gz -C /guest/axvisor/npu
    chmod -R a+rX /guest/axvisor/npu
    rm -f /tmp/rknn-yolov8-image.tar.gz
    sync
  "
'
```

放置结果：

```bash
ssh orangepi@${BOARD_IP} 'ls /guest/axvisor/npu/rknn-yolov8-image/'
# 期望与上面 install/ 的清单一致
```

（`orangepi` 是该板 Linux 的公开开发密码，与 [benchmarks 部署做法](../../../benchmarks/starry/orangepi-5-plus-uvc-rknn/README.md)一致；换板就换成该板密码。）

### 启动前核对

资产放好后，每次启动 AxVisor 前用板卡 Linux 确认它们在位：

```bash
cargo xtask board connect -b BallPick-Robot --server <服务地址> --port <端口>
```

进入板卡 Linux 后执行：

```bash
ls -l /guest/axvisor/linux/orangepi-5-plus-6.1.99
ls -l /guest/axvisor/linux/initramfs.cpio
ls /guest/axvisor/npu/rknn-yolov8-image/   # 场景 2 用
lsblk -o NAME,FSTYPE,SIZE                  # 确认 Linux 根分区设备名
cat /proc/cmdline                          # 确认 root= 与本配置一致
```

### 配置说明

`configs/linux.toml` 默认 `root=/dev/mmcblk0p2`，与 [os/axvisor/configs/vms/orangepi-5-plus/linux-smp1.toml](../../../os/axvisor/configs/vms/orangepi-5-plus/linux-smp1.toml) 一致。若本板 Linux 根在 `/dev/mmcblk1p2`（单客户端 USB 机器人板），按 [robot-real-linux/linux-smp1-emmc.toml](../normal/board-orangepi-5-plus/robot-real-linux/linux-smp1-emmc.toml) 改 `cmdline`，并把 `kernel_path` 换成该板实际使用的镜像名。

`configs/linux.toml` 的 `devices.passthrough` 决定直通范围，改之前先分清 `guest_type` 与选择器的关系。`guest_type = "passthrough"` 时，空 `passthrough` 列表与写 `/` 等价，都启用“宿主可直通设备减去基线始终剥掉的 PCI 主桥”这根基线，写在 `/` 旁边的具体路径叠加在基线上；列表只有具体路径、不含 `/` 时退化为精确选择、不含基线；`guest_type = "virtualized"` 则从无物理设备起步，`passthrough` 只接受具体路径。

选择器边界由 `axvmconfig` 的 `GuestDevices::validate` 与 `axvm` 的 `setup_guest_fdt_from_vmm` 共同约束。在含 `/` 的根基线模式下，只点名某个 PCI 主桥下的端点、却不点名该主桥会被拒绝，因为客户机只能通过主桥的配置空间枚举端点，整桥保留会连带暴露它的其他端点，报错会指出具体选择器和要补点名的主桥；列表不含 `/` 的纯具体路径精确模式保持原有语义，不受这条限制。`virtualized` 客户机在 `passthrough` 写 `/`，以及任何 `guest_type` 在 `disabled` 写 `/`，都会被拒绝；`disabled` 需点名具体设备。要从无物理设备起步，用 `guest_type = "virtualized"` 加空 `passthrough`，而不是在 `passthrough` 里删掉 `/`。

`configs/board-shell.toml` 不含 `server`/`port` —— 板卡服务地址属于运行环境配置，不写进仓库。按 [板卡管理](../../../docs/docs/build/board.md) 用 `cargo xtask board config` 一次性配置，或在命令行用 `--server`/`--port` 传入。

**本页所有板卡命令都显式写上 `--server`/`--port`**，不要依赖本机默认值：同一环境里可能存在多台板卡服务，各自挂的板卡不同，本地预置资产也不同，省掉参数就可能连到另一台上，跑出与你预期不符的结果。缺省时 `--server`/`--port` 会依次回落到 ostool 全局配置和内置默认地址，见 [板卡管理](../../../docs/docs/build/board.md)。

## 启动

在仓库根目录执行：

```bash
cargo xtask axvisor board \
  -c apps/axvisor/linux-guest-checks/configs/build.toml \
  --board-config apps/axvisor/linux-guest-checks/configs/board-shell.toml \
  --server <服务地址> --port <端口>
```

程序完整显示 AxVisor 与 Linux 启动日志。客户机启动完成后，在 AxVisor 控制台执行：

```text
vm console 1
```

回车即出现 Linux 提示符，随后按下面各节手工执行命令。

按键约定：`Ctrl+X` 后再按 `h` 返回 AxVisor 控制台；会话结束时按 `Ctrl+A` 后再按 `x` 释放串口。

## 场景 1：NPU 可用性检查

在客户机里逐条执行，看输出就行。都是只读查询，不改频率、不复位 NPU。

**NPU 认出来了没有：**

```bash
ls /sys/class/devfreq/fdab0000.npu/
```

能列出内容就说明驱动绑上了、devfreq 节点注册成功。正常长这样：

```text
available_frequencies  device  max_freq  power        trans_stat
available_governors    governor  min_freq  subsystem    uevent
cur_freq               load      name      target_freq
```

**绑的是哪个驱动：**

```bash
ls -l /sys/class/devfreq/fdab0000.npu/device/driver
```

`device/driver` 是指向驱动的符号链接，箭头后面就是驱动名：

```text
lrwxrwxrwx 1 root root 0 ... /sys/class/devfreq/fdab0000.npu/device/driver -> ../../../bus/platform/drivers/RKNPU
```

**支持哪些频点：**

```bash
cat /sys/class/devfreq/fdab0000.npu/available_frequencies
```

一行正整数，单位 Hz：

```text
300000000 400000000 500000000 600000000 700000000 800000000 900000000 1000000000
```

数量按实际内核配置来，**不要求固定**。

**当前负载：**

```bash
sudo cat /sys/kernel/debug/rknpu/load
```

三个核各给一个百分比，空闲时都是 `0%`：

```text
NPU load:  Core0:  0%, Core1:  0%, Core2:  0%,
```

某些驱动把零负载显示成空格加 `%`，也是有效的零值。

**想看驱动加载过程**可以加一条：

```bash
dmesg | grep -i rknpu
```

### 判定

四项都看得到才算通过：NPU 节点列得出来、驱动名是 `RKNPU`、频点非空、负载读得出三个核。

任一项看不到都**不算通过**，但不一定就是硬件坏了——权限不足、`/sys/kernel/debug` 没挂载都会让输出缺失。负载那条读不出来时，先确认 debugfs：

```bash
mount | grep debugfs
```

没有输出就是没挂载，挂上再重试：

```bash
sudo mount -t debugfs debugfs /sys/kernel/debug
```

## 场景 2：推理检查

在客户机里手动执行本应用自带的 YOLOv8 推理包（编译与放置见「首次准备」第 2 节），确认 RKNN 运行时、模型和驱动能在这一镜像上协同完成一次前向推理。

```bash
cd /guest/axvisor/npu/rknn-yolov8-image
./yolov8_demo.sh
```

不带参数时跑包内自带的 `images/`。指定自己的图片目录（支持空格，不递归子目录），或调整置信度阈值：

```bash
./yolov8_demo.sh /path/to/images
YOLO_MIN_CONFIDENCE=50 ./yolov8_demo.sh "./my images"
```

通过条件：输出末尾出现 `YOLO_IMAGE_LIST_SUCCESS`，紧跟一行 `success`。

预期现象（节选，数值以现场返回为准）：

```text
========== NPU YOLOv8 Image Inference ==========
Model: .../model/yolov8.rknn
Input directory: .../images
Images: 1
Confidence: 25.0%
NPU core: 0

[1/1] .../images/picture1.jpg
Image size: 640 x 640
Detections: 1
ID  Class ID  Class            Confidence  Box (left, top, right, bottom)
1   0         person            75.00%    (1, 2, 30, 40)
Status: PASS

========== Test Summary ==========
Processed: 1
Passed: 1
Failed: 0
YOLO_IMAGE_LIST_SUCCESS
success
0
```

空目录、模型初始化失败、图片解码或推理失败都返回非零且不打印 `success`；单张图片失败后继续处理其余图片并计入失败数。替换图片不需要重新编译，改程序则需要重新编译并重新拷贝。

包内细节（编译选项、sysroot、产物清单、源码构成）见 [rknn-yolov8-image/README.md](rknn-yolov8-image/README.md)。

## 场景 3：Linux 功能检查

进入客户机（`vm console 1`）后逐项执行。**网络三项依赖 `configs/linux.toml` cmdline 里的 `pci=nomsi`**：AxVM 宿主后端未接入物理 PCI MSI/ITS 分配，直通网卡必须走 PCIe 控制器的 INTx 级联；缺该参数时 r8125 收不到链路变化中断，接口永远是 `NO-CARRIER`，表现为 ping 报 `Network is unreachable`（实为无地址）。

网络在客户机里由 NetworkManager 自动配置（和板卡原生 Linux 相同）：启动约 1 分钟内 `r8125: enP3p49s0: link up`、DHCP 完成。若等了一段时间仍无地址，可手动触发（密码为该板公开开发密码）：

```bash
printf '%s\n' <板卡密码> | sudo -S -p '' nmcli -w 45 device connect enP3p49s0
```

### 功能项 1：ping 测试网络

```bash
ip -br a                 # 期望 enP3p49s0 UP 且有 10.x.x.x/24 地址
cat /sys/class/net/enP3p49s0/carrier   # 期望 1
gw=$(ip r | awk '/default/{print $3}'); ping -c 3 "$gw"
```

通过条件：接口有地址、carrier 为 1、ping 网关 0% 丢包（2026-10-16 实测：10.3.10.254，3 发 3 收，~0.9 ms；另 ping 板卡服务 10.3.10.60 同样 3/3）。

### 功能项 2：CPU 信息

```bash
cat /proc/cpuinfo
```

通过条件：能列出处理器信息；`processor` 条目数与配置的 `cpu_num` 一致（本配置为 1）。

### 功能项 3：文件读写

```bash
echo axvisor-guest-rw > /tmp/rw-check.txt
cat /tmp/rw-check.txt
```

通过条件：`cat` 原样返回写入内容。

### 功能项 4：ssh 连接 Ubuntu 客户机

客户机起来后（板卡 IP **仅在 ostool 会话持有期间可达**，见「首次准备」的说明），在工作站另开终端：

```bash
ssh orangepi@<客户机 IP>     # 客户机 IP 用场景 3 功能项 1 的 ip -br a 查
uname -r                     # 期望 6.1.99-rockchip-rk3588
```

通过条件：能登录并在客户机内执行命令（2026-10-16 实测：ssh 10.3.10.225 登录成功，与板卡原生 Linux 同 IP——同网卡同 MAC，DHCP 按现有分配）。

登录后建议先确认身份：客户机和板卡原生 Linux 用同一张网卡、同一个 IP，光看提示符分不出来。客户机是单核（`cpu_num = 1`），`grep -c '^processor' /proc/cpuinfo` 返回 `1`；板卡原生 Linux 是 8 核，返回 `8`。`uname -r` 两者相同（都是 `6.1.99-rockchip-rk3588`），不能用来区分。

### 功能项 5：apt 软件包安装

```bash
sudo apt-get update
sudo apt-get install -y sl && sl
```

通过条件（最小判定）：`apt-get update` 返回 0 并能拉到索引（2026-10-16 实测：repo.huaweicloud.com，34.8 MB / 11 s；原生同网络 4 s）。安装示例包成功并运行可再确认安装链路；不需要时可 `sudo apt-get remove -y sl` 清掉。

### 判定

功能项 1–5 独立判定，任一项失败不影响其他项的结论。网络三项失败时先核对 `pci=nomsi` 在不在 `configs/linux.toml` 的 cmdline 里，再看板卡所连网络（DHCP / 网关 / 镜像源）是否正常——用板卡原生 Linux（`cargo xtask board connect` 进去跑同样命令）做对照，可以把"用例配置问题"和"环境问题"分开。

五项一次跑通的完整记录（2026-10-16，同一会话内）：

| # | 实测 |
| --- | --- |
| 8 | `CPUs=1`，`processor : 0` |
| 9 | `enP3p49s0 UP 10.3.10.225/24`，`carrier=1`，ping 网关 3 发 3 收，avg 0.938 ms |
| 10 | 写入 `axvisor-guest-rw`，`cat` 原样返回 |
| 7 | 从工作站 `ssh orangepi@10.3.10.225` 登录成功 |
| 11 | `apt-get update` 返回 0，拉到 `repo.huaweicloud.com` 索引 |

网络就绪时间也可复现：客户机内核日志 `[60.744276] r8125: enP3p49s0: link up`，即启动约 1 分钟。

## 结束

先在客户机内正常关机：

```bash
sync
sudo poweroff
```

等客户机停止后退出 AxVisor，再按 `Ctrl+A` 后按 `x` 释放串口会话。不要在客户机根分区仍读写时直接断电。

## 判定边界

本页通过只说明**客户机能启动，且 NPU 被 Linux 驱动识别并暴露信息节点**（场景 1）、RKNN 运行时 / 模型与驱动能在这一镜像上协同完成一次前向推理（场景 2），以及**直通环境下 Linux 基础功能可用**（场景 3：ssh 可登录、CPU 信息可查、物理网卡有载波可 ping 通网关、根文件系统可读写、apt 可拉取安装软件包）。

它**不代表**识别准确率达标、DMA 长稳、性能达标，也不代表宿主侧虚拟化路径已被充分覆盖。场景 3 的网络项在 INTx（`pci=nomsi`）模式下验证；AxVM 将来接入物理 PCI MSI/ITS 分配后，可在去掉该参数的配置下另行复测。
