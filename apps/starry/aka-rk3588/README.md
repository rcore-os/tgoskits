# aka-rk3588 板卡检查

本目录描述 Orange Pi 5 Plus 上运行的两套 `aka-rk3588` 程序和本仓库为它们注册的
板卡检查。应用源码在外部仓库 `bullhh/aka-rk3588` 中维护；本仓库只保存打包脚本，
以及真实机器人程序使用的预编译 `prebuilt/aarch64/build/tennis`。

应用和客户机内核是两件事：客户机内核由板卡检查的 VM 配置决定，`tennis` 程序始终
来自板上共享根文件系统中的固定部署目录。CI 不下载、编译或部署应用。

## 1. 两套运行链路

| 链路 | 固定入口 | 固定部署目录 | 用途 |
| --- | --- | --- | --- |
| virtual | `run_vision_usb_ci_once.sh` | `/home/orangepi/robot-ci/aka-rk3588-virtual` | 真实 UVC 摄像头、RKNN YOLO 推理、FT232 `0403:6001` TX/RX 回环 |
| real | `run_robot_ci_once.sh` | `/home/orangepi/robot-ci/aka-rk3588` | 摄像头、RKNPU、USB 机器人控制器与车轮/机械臂完整控制 |

virtual 链路没有机械控制代码，也不打开 SoC UART6 `/dev/ttyS6`。它只证明摄像头到
NPU 的推理链路和 FT232 物理回环；回环帧原样返回不能证明控制器、伺服器或机械动作
正确。

real 链路使用原 USB 摄像头 `0ac8:0346` 和 USB 控制器 `1a86:55d3`。原生 Starry 直接
运行 `FEETECH_DEV=auto ./run_robot_ci_once.sh 28.0`；AxVisor Linux guest 通过
`sudo -S env FEETECH_DEV=auto` 运行同一入口。两者都不使用 SoC UART6 `/dev/ttyS6`；
USB 控制器仍可能在系统中呈现 USB 串口节点。VM 配置也不注入额外的 UART6 设备选择。

## 2. CI 矩阵

本目录相关的最终矩阵包含普通 CI 的四条检查、AxVisor Nightly 的两条检查：

| check id | 调度 | board selector | board_type | 入口 |
| --- | --- | --- | --- | --- |
| `test-orangepi-5-plus-robot-native-starryos` | 普通 CI | `orangepi-5-plus-robot` | `OrangePi-5-Plus` | 在 `/home/orangepi/robot-ci/aka-rk3588-virtual` 运行 `./run_vision_usb_ci_once.sh 28.0` |
| `test-orangepi-5-plus-robot-axvisor-starryos-guest` | 普通 CI | `orangepi-5-plus-robot-starry` | `OrangePi-5-Plus` | 同上，AxVisor 运行当前 checkout 构建的 StarryOS guest |
| `test-orangepi-5-plus-robot-axvisor-linux-guest` | 普通 CI | `orangepi-5-plus-robot-linux` | `OrangePi-5-Plus` | 同上，AxVisor 运行 Linux guest |
| `test-orangepi-5-plus-robot-real-native-starryos` | 普通 CI | `orangepi-5-plus-robot-real` | `OrangePi-5-Plus-robot` | 在 `/home/orangepi/robot-ci/aka-rk3588` 运行 `FEETECH_DEV=auto ./run_robot_ci_once.sh 28.0` |
| `test-orangepi-5-plus-robot-real-axvisor-starryos-guest` | AxVisor Nightly | `orangepi-5-plus-robot-real-starry` | `OrangePi-5-Plus-robot` | 同上，AxVisor 运行当前 checkout 构建的 StarryOS guest |
| `test-orangepi-5-plus-robot-real-axvisor-linux-guest` | AxVisor Nightly | `orangepi-5-plus-robot-real-linux` | `OrangePi-5-Plus-robot` | 同上，AxVisor 运行 Linux guest |

三条 virtual 检查使用普通 `OrangePi-5-Plus` 板卡类型。部署前需确认候选板带 UVC
摄像头和 `0403:6001` FT232 回环接线。

三条 real 检查统一使用板服务已注册的
`board_type = "OrangePi-5-Plus-robot"`，对应物理板 ID
`OrangePi-5-Plus-robot-1`。三条 TOML 都使用该类型。

资源组按物理板类型分开：三条 virtual 检查使用 `resource_group = "orangepi-5-plus"`，
三条 real 检查使用 `resource_group = "orangepi-5-plus-robot"`。

## 3. 客户机内核与根文件系统

virtual 和 real 的 AxVisor StarryOS guest 都使用 `image_location = "memory"`，内核为
当前 checkout 构建的
`target/aarch64-unknown-none-softfloat/release/starryos.bin`。real StarryOS guest 不
覆盖 `cmdline`，沿用宿主 bootargs。

AxVisor Linux guest 使用 `image_location = "fs"`、内核
`/guest/linux/orangepi-5-plus-6.1.99` 和 `console=ttyS2`。virtual 普通板使用
`root=/dev/mmcblk0p2`；单客户机 USB 机器人板实测 guest 根为
`/dev/mmcblk1p2`，real Linux 检查使用该根。宿主的 `/guest/linux/` 由人工维护，
仓库只引用路径。

普通 CI 的 virtual 检查消费共享 eMMC 根 `/dev/mmcblk0p2`；real Linux 使用 USB 机器人板
实测的 `/dev/mmcblk1p2`。三条 real 检查（普通 CI 的原生 Starry 和 AxVisor Nightly 的
两条 guest 检查）消费单客户机 USB 机器人板上的部署目录；它们都不使用 SoC UART6
`/dev/ttyS6`。

## 4. PASS/FAIL 判定

应用内部判定和启动器对外判定必须配套使用：

- virtual：`run_vision_usb_ci_once.sh 28.0` 只有在应用 `APPLICATION_PASS`、两个完整
  性能窗口、FT232 回环校验和零退出码全部通过后，才输出
  `[VISION_USB_CI] RESULT=PASS attempts=1`。board 配置只接受这一条最终标记，不接受
  中间日志或诊断汇总；任何失败路径返回非零并输出
  `[VISION_USB_CI] RESULT=FAIL`。
- real：`run_robot_ci_once.sh 28.0` 只有在应用完整判定和零退出码后，才输出
  `[ROBOT_CI] RESULT=PASS attempts=1` 或 `attempts=2`。board 命令不再使用
  `|| echo` 吞掉退出码，因此非零退出直接让步骤失败；成功标记仍由最终启动器输出。

两个入口的 FPS 门槛都写在对应 board TOML 中，不能靠宿主环境变量覆盖。成功正则只
匹配最终生产者标记，命令回显中没有任何成功标记。

## 5. 覆盖边界

| 能力 | virtual 三条 | real 三条 |
| --- | --- | --- |
| UVC 采集 / JPEG 解码 / RKNN 推理 | 覆盖 | 覆盖 |
| FT232 TX/RX 回环 | 覆盖 | 不适用 |
| USB 相机 `0ac8:0346` 与控制器 `1a86:55d3` | 不声明 | 覆盖 |
| 车轮、机械臂和停车流程 | 不驱动 | 覆盖完整控制流程 |
| 抓球成功率、长期稳定性、急停 | 不覆盖 | 只覆盖单次完整流程 |

real 检查需要现场满足机械臂活动空间、车体架空或安全停靠、标定文件和控制器连接等
前置条件。任何一种检查通过都不能替代物理安全评审或人工验收。

## 6. 人工重编译与打包

重编译和部署始终由人工完成，CI 只消费板上已有包。不要提交 `target/` 产物、部署包或
新的 prebuilt 目录。

### 6.1 virtual 链路

1. 准备源码。脚本在开始时把分支解析成完整提交 SHA，源码只写到 `target/` 下：

   ```bash
   cd apps/starry/aka-rk3588
   ./prepare-vision-usb-source.sh
   # 或使用本地 virtual checkout：
   ./prepare-vision-usb-source.sh --checkout /path/to/aka-rk3588-virtual-work
   ```

   默认输出 `target/aka-rk3588-vision-usb/source/` 和
   `target/aka-rk3588-vision-usb/SOURCE`。`SOURCE` 记录仓库、ref、完整 commit、源码
   归档哈希和源码树哈希。

2. 在兼容 AArch64 的 Jammy 环境中，用准备好的源码完成构建，得到 `build/tennis`，
   并把运行库 `libuvc.so.0`、`libusb-1.0.so.0`、`libturbojpeg.so.0`、
   `libjpeg.so.8`、`libudev.so.1` 放到构建输出的 `lib/` 下。

3. 从本次构建输出生成部署包：

   ```bash
   ./prepare-vision-usb-package.sh \
     --source-dir target/aka-rk3588-vision-usb/source \
     --build-dir /path/to/build-output
   ```

   输出 `target/aka-rk3588-vision-usb/aka-rk3588-vision-usb.tar.gz` 和同目录 `SOURCE`。
   脚本只使用命令参数给出的源码树和本次构建输出，不访问网络，也不部署到板卡；
   `SOURCE` 记录源码树哈希、`tennis`、启动器、模型、`librknnrt.so` 和运行库清单哈希。

4. 持有板卡租约后，人工把同一个包部署到 `/home/orangepi/robot-ci/aka-rk3588-virtual`。
   部署前确认没有检查在运行，整目录切换并保留上一版回滚。

### 6.2 real 链路

`prepare-package.sh` 读取 `source.env`，下载并校验固定提交的源码归档，复制本仓库
跟踪的 `prebuilt/aarch64/build/tennis`，输出
`target/aka-rk3588/aka-rk3588.tar.gz`。包内 `SOURCE` 记录仓库、提交、源码归档哈希和
二进制哈希。

更换 real 程序版本时，`source.env` 中的提交号、源码归档 SHA256 和二进制 SHA256
必须一起更新。部署到 `/home/orangepi/robot-ci/aka-rk3588` 由人工完成；CI 不自动
打包、编译或部署。

## 7. 文件说明

| 文件 | 用途 |
| --- | --- |
| `prepare-package.sh` | 组装 real 机器人包 |
| `prepare-vision-usb-source.sh` | 解析 virtual 提交并准备源码 |
| `prepare-vision-usb-package.sh` | 从源码和本次构建输出组装 virtual 包 |
| `board-orangepi-5-plus.toml` | `cargo xtask starry app board -t aka-rk3588` 的 real 板卡入口 |
| `init.sh` | `starry app board` 使用的 real 板上最小视觉冒烟 |
