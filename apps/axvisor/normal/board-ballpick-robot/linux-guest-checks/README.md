# AxVisor Linux 直通客户机功能检查

## 测试用途

在目标板卡上通过 AxVisor 启动**单个直通 Linux 客户机**，由测试者进入客户机后手工执行命令，检查客户机启动、RK3588 NPU 驱动绑定、NPU 信息查询、YOLOv8 图片推理，以及 ssh / CPU 信息 / 网络 ping / 文件读写 / 软件包安装等 Linux 基础功能是否可用。

本应用**不含自动化**：没有启动器、没有自动命令注入、没有通过/失败判定。仓库只提供启动配置和本文档，命令与现象由测试者自行执行和判断。

本目录是**直通单 Linux 客户机**这一场景的检查集合，后续检查（如 syslitest 等）继续加在本页的并列章节中；同一份 `configs/` 服务全部章节。

## 边界

本目录是手工应用，通过下文的 `cargo xtask axvisor board` 显式加载 `configs/uboot-shell.toml`。`discover_board_runtime_configs()` 会递归匹配 `board-*.toml`，因此手工配置特意避开这个命名前缀，防止进入自动测试发现流程。通用组名枚举仍可能包含 `linux-guest-checks`，但 `cargo xtask axvisor test board --list` 不会列出本目录的用例；显式指定 `-g linux-guest-checks` 会报告没有板卡测试用例，不能据此判定手工检查通过。

- 只验证链路可用：客户机启动、NPU 驱动绑定、信息/频点/负载可查询、NPU 能跑通一次 YOLOv8 图片推理、Linux 基础功能（ssh / cpuinfo / ping / 读写 / apt）可用。
- 不验证识别准确率，不与预设答案比对；未检测到目标时输出 `Detections: 0` 也算推理正常完成。
- 不修改 NPU 频率、不复位 NPU。
- 客户机为 `guest_type = "passthrough"` 单客户机，宿主设备在根基线之上**直通**（见 `configs/linux.toml` 中 `passthrough` 列表的语义说明），不是最小设备白名单。
- 功能检查的网络项依赖板卡所连网络：DHCP 可用、网关与外部 apt 镜像（repo.huaweicloud.com）可达。换网络环境时以现场实际为准。

## 运行前确认

板卡根文件系统使用标准镜像 `orangepi-5-plus.img.tar.xz`，镜像中已提供
`/guest/linux/orangepi-5-plus`，直接复用，不复制、不改名。
本用例直接挂载板卡 Ubuntu 根分区，不加载初始内存盘；标准 `initramfs.cpio` 会进入独立的 BusyBox Shell。
Linux 版本为 `6.1.99-rockchip-rk3588`，支持本用例所需的 NPU 驱动与直通设备。
YOLOv8 推理包不在标准镜像中，执行场景 2 前需要按下面步骤编译和放置。

### 首次准备

以下步骤只准备额外的 YOLOv8 软件，不重新准备客户机内核。

#### YOLOv8 推理包

在工作站仓库根目录执行以下命令编译，再把产物整个目录放到板卡的 `/guest/axvisor/npu/` 下。

```bash
cd apps/axvisor/normal/board-ballpick-robot/linux-guest-checks/rknn-yolov8-image
./build.sh
```

编译成功时脚本最后一行打印 `Runtime package: .../install/rknn-yolov8-image`。确认产物内容：

```bash
ls install/rknn-yolov8-image/
# 期望：README.md  images  lib  licenses  model  rknn_yolov8_images  yolov8_demo.sh
```

传输时先在另一个工作站终端连接板卡原生 Linux，并保持会话：

```bash
cargo xtask board connect -b <板卡类型> --server <服务地址> --port <端口>
```

在板卡 Shell 执行 `ip -br addr` 查看 IP。会话释放会给板卡断电，传输期间不要退出，
也不要同时运行 `axvisor board` 申请同一块板卡。

`/guest/` 归 root 所有，`orangepi` 用户写不进去，所以先落到 `/tmp` 再由 root 移动：

在刚才编译推理包的工作站终端中，先返回仓库根目录，再打包和传输：

```bash
cd "$(git rev-parse --show-toplevel)"
tar czf /tmp/rknn-yolov8-image.tar.gz -C apps/axvisor/normal/board-ballpick-robot/linux-guest-checks/rknn-yolov8-image/install rknn-yolov8-image

BOARD_IP=<板卡 IP>
cat /tmp/rknn-yolov8-image.tar.gz | ssh orangepi@${BOARD_IP} 'cat > /tmp/rknn-yolov8-image.tar.gz'
```

在持有板卡原生 Linux 连接的终端中执行，`sudo` 按提示输入板卡密码：

```sh
sudo mkdir -p /guest/axvisor/npu
sudo tar xzf /tmp/rknn-yolov8-image.tar.gz -C /guest/axvisor/npu
sudo chmod -R a+rX /guest/axvisor/npu/rknn-yolov8-image
ls /guest/axvisor/npu/rknn-yolov8-image/
rm /tmp/rknn-yolov8-image.tar.gz
sync
```

确认包含 `images`、`lib`、`model`、`rknn_yolov8_images` 和 `yolov8_demo.sh`。
更新推理包前先保存已有包，再解压覆盖；标准客户机文件保持原位置。

### 启动前核对

资产放好后，每次启动 AxVisor 前用板卡 Linux 确认它们在位：

```bash
cargo xtask board connect -b '<板卡类型>' --server <服务地址> --port <端口>
```

进入板卡 Linux 后执行：

```bash
ls -l /guest/linux/orangepi-5-plus
ls /guest/axvisor/npu/rknn-yolov8-image/   # 场景 2 用
lsblk -o NAME,FSTYPE,SIZE                  # 确认 Linux 根分区设备名
cat /proc/cmdline                          # 确认 root= 与本配置一致
```

### 配置说明

`configs/linux.toml` 默认 `root=/dev/mmcblk0p2`，与 [robot-real-linux/linux-smp1-emmc.toml](../robot-real-linux/linux-smp1-emmc.toml) 及实测 BallPick 板一致，内核使用标准路径 `/guest/linux/orangepi-5-plus`。换板时先从原生 Linux 核对根设备，再调整 `cmdline`。

`configs/linux.toml` 的 `devices.passthrough` 决定直通范围，改之前先分清 `guest_type` 与选择器的关系。`guest_type = "passthrough"` 时，空 `passthrough` 列表与写 `/` 等价，都启用“宿主可直通设备减去基线始终剥掉的 PCI 主桥”这根基线，写在 `/` 旁边的具体路径叠加在基线上；列表只有具体路径、不含 `/` 时退化为精确选择、不含基线；`guest_type = "virtualized"` 则从无物理设备起步，`passthrough` 只接受具体路径。

选择器边界由 `axvmconfig` 的 `GuestDevices::validate` 与 `axvm` 的 `setup_guest_fdt_from_vmm` 共同约束。在含 `/` 的根基线模式下，只点名某个 PCI 主桥下的端点、却不点名该主桥会被拒绝，因为客户机只能通过主桥的配置空间枚举端点，整桥保留会连带暴露它的其他端点，报错会指出具体选择器和要补点名的主桥；列表不含 `/` 的纯具体路径精确模式保持原有语义，不受这条限制。`virtualized` 客户机在 `passthrough` 写 `/`，以及任何 `guest_type` 在 `disabled` 写 `/`，都会被拒绝；`disabled` 需点名具体设备。要从无物理设备起步，用 `guest_type = "virtualized"` 加空 `passthrough`，而不是在 `passthrough` 里删掉 `/`。

`configs/uboot-shell.toml` 不含 `server`/`port` —— 板卡服务地址属于运行环境配置，不写进仓库。按 [板卡管理](../../../../../docs/docs/build/board.md) 用 `cargo xtask board config` 一次性配置，或在命令行用 `--server`/`--port` 传入。

**本页所有板卡命令都显式写上 `--server`/`--port`**，不要依赖本机默认值：同一环境里可能存在多台板卡服务，各自挂的板卡不同，本地预置资产也不同，省掉参数就可能连到另一台上，跑出与你预期不符的结果。缺省时 `--server`/`--port` 会依次回落到 ostool 全局配置和内置默认地址，见 [板卡管理](../../../../../docs/docs/build/board.md)。

## 启动

本启动流程依赖仓库 `Cargo.lock` 当前解析的 **`ostool 0.30.3`**，通过 `cargo xtask axvisor board` 调用其 U-Boot 板卡运行器。在工作站的交互终端中，从仓库根目录执行：

```bash
cargo xtask axvisor board \
  -c apps/axvisor/normal/board-ballpick-robot/linux-guest-checks/configs/build.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/linux-guest-checks/configs/uboot-shell.toml \
  --server <服务地址> --port <端口>
```

`vm_configs` 负责打包 Linux 配置，不会自动启动客户机。主机启动参数中保持
`axvisor.vm_start` 未设置。看到 `axvisor:/$` 后，在 AxVisor 控制台手工创建并启动
builtin 清单中 ID 为 `1` 的 Linux 客户机，同时接入控制台：

```text
vm start --console 1
```

等待 Linux 启动完成，登录后按下面各节手工执行命令。若客户机已在运行，使用
`vm console 1` 接入；该命令只连接控制台，不创建或启动客户机。

按键约定：`Ctrl+X` 后再按 `h` 返回 AxVisor 控制台；会话结束时按 `Ctrl+A` 后再按 `x` 释放串口。

`uboot-shell.toml` 的 `shell_check_steps = []` 使该版本的 `UbootConfig::shell_check_matcher()` 不创建自动检查器；启动后，`AsyncTerminal` 继续转发串口输入输出。`fail_regex = []` 也不检测失败标记。配置未设置 `timeout`，该版本不会为启动后的终端阶段设置超时；U-Boot 连接和准备阶段仍有独立的超时与错误处理。因此命令停留在终端等待人工操作是预期行为，不会因没有检查步骤而自动报告功能通过。

`Ctrl+X` 后按 `h` 只切回 AxVisor 控制台，会话仍占用板卡。`Ctrl+A` 后按 `x` 结束工作站终端，运行器随后通过 `finalize_session()` 请求释放板卡会话。正常退出且释放成功时命令返回 0；启动、传输、终端或释放报错时返回非零。**退出码 0 只表示运行会话正常结束，不表示任何手工功能项通过**；每项结果仍须依据本页的命令输出记录。升级 `ostool` 时应复核空步骤、终端退出与会话释放行为，再用本板卡入口确认；这些约定不能直接套用到会拒绝空步骤的其他应用入口。

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

在客户机里手动执行本应用自带的 YOLOv8 推理包（编译与放置见「首次准备」），确认 RKNN 运行时、模型和驱动能在这一镜像上协同完成一次前向推理。

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

网络在客户机里由 NetworkManager 自动配置（和板卡原生 Linux 相同）：启动约 1 分钟内 `r8125: enP3p49s0: link up`、DHCP 完成。若等了一段时间仍无地址，可手动触发，`sudo` 按提示输入板卡密码：

```bash
sudo nmcli -w 45 device connect enP3p49s0
```

### 功能项 1：ping 测试网络

```bash
ip -br a                 # 期望 enP3p49s0 UP 且有 10.x.x.x/24 地址
cat /sys/class/net/enP3p49s0/carrier   # 期望 1
gw=$(ip r | awk '/default/{print $3}'); ping -c 3 "$gw"
```

通过条件：接口有地址、carrier 为 1、ping 网关 0% 丢包。

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

通过条件：`cat` 原样返回写入内容。完成后执行 `rm /tmp/rw-check.txt` 清理本次文件。

### 功能项 4：ssh 连接 Ubuntu 客户机

客户机起来后（板卡 IP **仅在 ostool 会话持有期间可达**，见「首次准备」的说明），在工作站另开终端：

```bash
ssh orangepi@<客户机 IP>     # 客户机 IP 用场景 3 功能项 1 的 ip -br a 查
uname -r                     # 期望 6.1.99-rockchip-rk3588
```

通过条件：能登录并在客户机内执行命令。

登录后建议先确认身份：客户机和板卡原生 Linux 用同一张网卡、同一个 IP，光看提示符分不出来。客户机是单核（`cpu_num = 1`），`grep -c '^processor' /proc/cpuinfo` 返回 `1`；板卡原生 Linux 是 8 核，返回 `8`。`uname -r` 两者相同（都是 `6.1.99-rockchip-rk3588`），不能用来区分。

### 功能项 5：apt 软件包安装

先查看 `hello` 是否已安装并记录结果，便于测试后恢复原状态：

```sh
dpkg-query -W -f='${Status}\n' hello
```

更新在线索引，重新安装并运行示例包，逐条等待命令完成：

```sh
sudo apt-get update
sudo apt-get install -y --reinstall hello
hello
```

通过条件：索引更新和安装均成功，`hello` 输出 `Hello, world!`。
测试前未安装时，执行 `sudo apt-get purge -y hello` 清理本次安装；原本已安装时保留。

### 判定

功能项 1–5 独立判定，任一项失败不影响其他项的结论。网络三项失败时先核对 `pci=nomsi` 在不在 `configs/linux.toml` 的 cmdline 里，再看板卡所连网络（DHCP / 网关 / 镜像源）是否正常——用板卡原生 Linux（`cargo xtask board connect` 进去跑同样命令）做对照，可以把"用例配置问题"和"环境问题"分开。

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
