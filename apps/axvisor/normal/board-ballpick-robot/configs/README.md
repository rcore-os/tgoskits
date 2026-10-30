# BallPick-Robot UART6 设备树

本目录保存 `apps/axvisor/normal/board-ballpick-robot` 下应用共用的板级配置。`ballpick-robot-uart6.dtb` 在仓库的 RK3588 板卡设备树上启用 UART6_M1，供机器人应用直接控制执行器，当前由 `robot-real-starry` 引用。UART6 位于 `/serial@feb90000`，使用 40 针排针的 8 号 TX 和 10 号 RX，与匹配的舵机总线适配器交叉连接并共地。

## 1. 设备树生成

`ballpick-robot-uart6.dtso` 只将 UART6 的 `status` 改为 `okay`，并明确选择已有的 `uart6m1_xfer` 引脚组。基础设备树中的摄像头、USB、NPU、存储及调试串口配置继续保留。

### 1.1 基础文件

输入为仓库中的 [orangepi-5-plus.dtb](../../../../../os/axvisor/configs/board/orangepi-5-plus.dtb)。其 SHA256 为 `66939117c227f224ee644ed4543b68178ceeb8c18655ff01a10db9da28fc9ad5`，原 UART6 状态为 `disabled`。

### 1.2 编译与核对

安装提供 `dtc`、`fdtoverlay` 和 `fdtget` 的设备树工具后，从 TGOSKits 仓库根目录执行以下命令。生成的 DTB 与叠加层源码一同维护，启动应用时直接引用 DTB。

```bash
dtc -@ -I dts -O dtb \
  -o /tmp/ballpick-robot-uart6.dtbo \
  apps/axvisor/normal/board-ballpick-robot/configs/ballpick-robot-uart6.dtso
fdtoverlay \
  -i os/axvisor/configs/board/orangepi-5-plus.dtb \
  -o apps/axvisor/normal/board-ballpick-robot/configs/ballpick-robot-uart6.dtb \
  /tmp/ballpick-robot-uart6.dtbo
fdtget apps/axvisor/normal/board-ballpick-robot/configs/ballpick-robot-uart6.dtb \
  /serial@feb90000 status
```

最后一条命令应输出 `okay`。当前基础树已使用 UART6_M1 引脚组，叠加后的设备树内容仅有该串口的启用状态变化。

## 2. 单客户机配置

宿主和客户机使用同一份带 UART6 的设备树；启动配置同时准备 UART6 的时钟、复位及引脚复用。应用通过串口访问执行器，摄像头继续使用 USB。

### 2.1 宿主与客户机设备树

`robot-real-starry` 的交互配置与自动配置均通过 `dtb_file` 引用本目录的 `ballpick-robot-uart6.dtb`，并在 U-Boot 阶段准备串口时钟、复位及引脚复用。客户机 [guest.toml](../robot-real-starry/guest.toml) 的 `kernel.dtb_path` 也引用该文件，保留完整硬件描述。

`devices.passthrough` 显式分配 `/serial@feb90000`，以及根盘、USB 摄像头、NPU 和相应的时钟、DMA、电源、引脚依赖，使客户机能接收这些设备的中断。物理调试串口由宿主管理；不要用根路径 `/` 代替这组选择器。`robot-real-linux` 目前仍保留原来的 USB 配置，不使用本设备树。

### 2.2 应用运行入口

在 StarryOS 客户机中进入已部署的单客户机发布包目录，先扫描执行器，再运行有限检查。程序使用包内的 `bin/tennis`、运行库、模型及本机标定。

```bash
cd /home/orangepi/robot/aka-rk3588-single
LD_LIBRARY_PATH="$PWD/lib:${LD_LIBRARY_PATH:-}" \
  ./bin/tennis test-feetech /dev/ttyS6 scan
FEETECH_DEV=/dev/ttyS6 ./run_single_pick_ci_once.sh 28.0
```

扫描应发现 ID 1～9。有限检查会执行车轮和机械臂动作，运行前按测试要求架空车轮并清空机械臂活动范围；只有脚本的最终结果和退出码都满足用例要求，才判定应用通过。
