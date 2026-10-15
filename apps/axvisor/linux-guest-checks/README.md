# AxVisor Linux 直通客户机功能检查

## 测试用途

在捡球机器人板（OrangePi-5-Plus 硬件，板卡类型 `BallPick-Robot`）上通过 AxVisor 启动**单个直通 Linux 客户机**，由测试者进入客户机后手工执行命令，检查客户机启动、RK3588 NPU 驱动绑定、NPU 信息查询和 YOLOv8 图片推理是否可用。

本应用**不含自动化**：没有启动器、没有自动命令注入、没有通过/失败判定。仓库只提供启动配置和本文档，命令与现象由测试者自行执行和判断。

本目录是**直通单 Linux 客户机**这一场景的检查集合，后续检查（如 syslitest 等）继续加在本页的并列章节中；同一份 `configs/` 服务全部章节。

## 边界

- 只验证链路可用：客户机启动、NPU 驱动绑定、信息/频点/负载可查询、NPU 能跑通一次 YOLOv8 图片推理。
- 不验证识别准确率，不与预设答案比对；未检测到目标时输出 `Detections: 0` 也算推理正常完成。
- 不修改 NPU 频率、不复位 NPU。
- 客户机为 `guest_type = "passthrough"` 单客户机，宿主设备**全量直通**，不是最小设备白名单。

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

## 结束

先在客户机内正常关机：

```bash
sync
sudo poweroff
```

等客户机停止后退出 AxVisor，再按 `Ctrl+A` 后按 `x` 释放串口会话。不要在客户机根分区仍读写时直接断电。

## 判定边界

本页通过只说明**客户机能启动，且 NPU 被 Linux 驱动识别并暴露信息节点**（场景 1），以及 RKNN 运行时、模型与驱动能在这一镜像上协同完成一次前向推理（场景 2）。

它**不代表**识别准确率达标、DMA 长稳、性能达标，也不代表宿主侧虚拟化路径已被充分覆盖。
