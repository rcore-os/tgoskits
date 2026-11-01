# aka-rk3588 板卡检查

本目录描述 Orange Pi 5 Plus 上运行的两套 `aka-rk3588` 程序和本仓库为它们注册的
板卡检查。应用源码在外部仓库 `bullhh/aka-rk3588` 中维护；本仓库保存打包脚本、
真实机器人启动器 `run_robot_ci_once.sh`，以及预编译 `prebuilt/aarch64/build/tennis`。

应用和客户机内核是两件事：客户机内核由板卡检查的 VM 配置决定，`tennis` 程序始终
来自对应系统根文件系统中的固定部署目录。AxVisor real 客户机使用各自的独立镜像，
程序必须部署在客户机镜像内。CI 不下载、编译或部署应用。

## 1. 两套运行链路

| 链路 | 固定入口 | 固定部署目录 | 用途 |
| --- | --- | --- | --- |
| virtual | `run_vision_usb_ci_once.sh` | `/home/orangepi/robot-ci/aka-rk3588-virtual` | 真实 UVC 摄像头、RKNN YOLO 推理、FT232 `0403:6001` TX/RX 回环 |
| real 原生 StarryOS | `run_robot_ci_once.sh` | `/home/orangepi/robot-ci/aka-rk3588` | 摄像头、RKNPU、USB 机器人控制器与车轮/机械臂完整控制 |
| real AxVisor Linux / StarryOS | `run_single_pick_ci_once.sh` | 客户机镜像内 `/home/orangepi/robot/aka-rk3588-single` | 摄像头、RKNPU、USB 机器人控制器与车轮/机械臂抓放动作流程 |

virtual 链路没有机械控制代码，也不打开 SoC UART6 `/dev/ttyS6`。它只证明摄像头到
NPU 的推理链路和 FT232 物理回环；回环帧原样返回不能证明控制器、伺服器或机械动作
正确。

real 链路使用原 USB 摄像头 `0ac8:0346` 和 USB 控制器 `1a86:55d3`。原生 Starry 直接
运行 `FEETECH_DEV=auto ./run_robot_ci_once.sh 28.0`；AxVisor Linux 和 StarryOS guest
在 root Shell 中运行 `FEETECH_DEV=auto ./run_single_pick_ci_once.sh 28.0`。
三种场景都不使用 SoC UART6 `/dev/ttyS6`；
USB 控制器仍可能在系统中呈现 USB 串口节点。VM 配置也不注入额外的 UART6 设备选择。

## 2. CI 矩阵

本目录相关的最终矩阵把同一块板上的 Starry 原生 board suite 注册到普通 CI；
AxVisor Linux 和 AxVisor Starry guest 由三客户机功能检查、机器人 guest 链路与
AxVisor Nightly 覆盖，普通 CI 的 Starry 检查不再重复运行单客户机 AxVisor smoke：

| check id | 调度 | board selector | board_type | 入口 |
| --- | --- | --- | --- | --- |
| `test-starry-self-hosted-board-orangepi-5-plus` | 普通 CI | `orangepi-5-plus` | `OrangePi-5-Plus` | 运行原生 StarryOS board suites；不再重复执行单客户机 AxVisor Linux 和 Starry smoke |
| `test-orangepi-5-plus-robot-axvisor-guests` | 普通 CI | `orangepi-5-plus-robot`、`orangepi-5-plus-robot-starry`、`orangepi-5-plus-robot-linux` | `OrangePi-5-Plus` | 同一 runner 先运行 virtual Starry suites，再用共享 AxVisor 构建运行 Linux 和 StarryOS guest |
| `test-orangepi-5-plus-robot-real-native-starryos` | AxVisor Nightly | `orangepi-5-plus-robot-real` | `OrangePi-5-Plus-robot` | 原生 StarryOS 完整机器人流程 |
| `test-orangepi-5-plus-robot-real-axvisor-starryos` | AxVisor Nightly | `orangepi-5-plus-robot-real-starry` | `BallPick-Robot` | `board-ballpick-robot/robot-real-starry` 单客户机硬件动作流程 |
| `test-orangepi-5-plus-robot-real-axvisor-linux` | AxVisor Nightly | `orangepi-5-plus-robot-real-linux` | `BallPick-Robot` | `board-ballpick-robot/robot-real-linux` 单客户机硬件动作流程 |

三条 real 夜测分别使用独立 job、超时和日志。原生 StarryOS 使用 `run_robot_ci_once.sh 28.0`，
两种 AxVisor 客户机使用 `run_single_pick_ci_once.sh 28.0`，均设置 `FEETECH_DEV=auto`。
矩阵关闭 fail-fast，单场景失败不取消
其他场景；最终汇总仍要求全部成功。

三条 virtual 检查使用普通 `OrangePi-5-Plus` 板卡类型。部署前需确认候选板带 UVC
摄像头和 `0403:6001` FT232 回环接线。

real 原生 StarryOS 的板卡配置仍申请 `board_type = "OrangePi-5-Plus-robot"`；
两种 AxVisor real 客户机的 smoke 配置申请 `board_type = "BallPick-Robot"`。
`board selector` 用于选择仓库用例，`board_type` 用于申请物理板卡，两者不能混用。

CI 清单中，三条 virtual 检查使用 `resource_group = "orangepi-5-plus"`，
三条 real 检查的 CI 清单仍使用 `resource_group = "orangepi-5-plus-robot"`。
该字段用于调度互斥，实际申请的物理板型由各板卡配置的 `board_type` 决定。

## 3. 客户机内核与根文件系统

AxVisor StarryOS guest 使用当前 checkout 构建的
`target/aarch64-unknown-none-softfloat/release/starryos.bin`。`vm_configs` 指定归档输入，
axbuild 将内核装入宿主 initramfs 的 `/guest/builtin/images`；有宿主磁盘根时，AxVisor
先安装自带资源再切根，从最终文件路径加载客户机。real StarryOS guest 使用独立的
`configs/starry-guest-build.toml` 构建，并显式设置 `cmdline`，通过 `setsid --fork --wait --ctty`
建立交互控制终端；Linux real guest 也显式设置自己的启动参数，不依赖宿主 bootargs。

AxVisor real Linux guest 使用宿主上的 `/guest/linux/orangepi-5-plus` 内核和
`console=ttyS2`，宿主的内核文件由人工维护，仓库只引用路径。
real 场景的根盘由当前 native board 配置和两份 AxVisor guest 配置分别指定：

| real 场景 | 原生系统或 AxVisor 宿主根盘 | 客户机镜像 | 客户机根参数 |
| --- | --- | --- | --- |
| 原生 StarryOS | `/dev/mmcblk0p2` | 不适用 | 不适用 |
| AxVisor Linux | `/dev/mmcblk0p2` | `/guest/rootfs-aarch64-orangepi-jammy-0.img` | `root=/dev/vda` |
| AxVisor StarryOS | `/dev/mmcblk0p2` | `/guest/rootfs-aarch64-orangepi-jammy-1.img` | `root=/dev/sda` |

两种 AxVisor real 客户机均排除物理 MMC 控制器，eMMC 及宿主文件系统由宿主独占。
客户机只挂载自己的 VirtIO ext4 镜像，不能将宿主的 `root=` 复制到客户机配置。
StarryOS 的 `/dev/sda` 是当前 `RootSpec` 支持的第一块盘选择方式。
原生 StarryOS 根参数见
[robot-flow 板卡配置](../../../test-suit/starryos/board-orangepi-5-plus/robot-flow/board-orangepi-5-plus-robot-real.toml)；
两种客户机的配置、构建及镜像部署步骤分别见
[Linux 机器人说明](../../axvisor/normal/board-ballpick-robot/robot-real-linux/README.md) 和
[StarryOS 机器人说明](../../axvisor/normal/board-ballpick-robot/robot-real-starry/README.md)。
普通 CI 的 virtual 检查仍使用其原有板卡与部署配置，不套用上述 real 客户机根盘设置。

## 4. PASS/FAIL 判定

应用内部判定和启动器对外判定必须配套使用：

- virtual：`run_vision_usb_ci_once.sh 28.0` 只有在应用 `APPLICATION_PASS`、两个完整
  性能窗口、FT232 回环校验和零退出码全部通过后，才输出
  `[VISION_USB_CI] RESULT=PASS attempts=1`。board 配置只接受这一条最终标记，不接受
  中间日志或诊断汇总；任何失败路径返回非零并输出
  `[VISION_USB_CI] RESULT=FAIL`。
- real：原生 StarryOS 的 `run_robot_ci_once.sh 28.0` 和 AxVisor 客户机的
  `run_single_pick_ci_once.sh 28.0` 只有在应用完整判定和零退出码后，才输出
  `[ROBOT_CI] RESULT=PASS attempts=1` 或 `attempts=2`。board 命令不再使用
  `|| echo` 吞掉退出码，因此非零退出直接让步骤失败；成功标记仍由最终启动器输出。

本目录的 `run_robot_ci_once.sh` 以 `source.env` 固定版本的协议为基础维护。它先识别 awk 实现，
仅为 mawk 加 `-W interactive`，使管道输入逐行处理；单独调用 `fflush()` 只能刷新
awk 输出，不能避免输入缓冲造成的集中打印。其他 awk 继续使用通用参数，原有
`APPLICATION_PASS` 字段、性能门槛、进程退出码及失败后恢复/重试判定保持不变。

`prepare-package.sh` 始终使用本目录的启动器覆盖上游归档入口，并在包内 `SOURCE`
记录 `launcher_sha256`。此修正不需要重新构建 `tennis`。板卡 CI 使用预部署目录，
本地改动或重新打包不会自动更新板上脚本；部署修正版后才能验证实际板端日志时序。

各入口的 FPS 门槛都写在对应 board TOML 中，不能靠宿主环境变量覆盖。成功正则只
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
AxVisor 的 `run_single_pick_ci_once.sh` 可使用模拟就绪条件驱动真实车轮和机械臂，
该硬件动作流程通过不能单独证明实际抓取了实体网球。

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
必须一起更新。该包供原生 StarryOS 使用，部署到 `/home/orangepi/robot-ci/aka-rk3588` 由人工完成。
AxVisor 两种 real 客户机使用镜像内的 `/home/orangepi/robot/aka-rk3588-single` 及其
`run_single_pick_ci_once.sh`，按第 3 节链接的各应用说明准备，不能直接套用本目录的
打包脚本和旧部署目录。CI 不自动打包、编译或部署。

## 7. 文件说明

| 文件 | 用途 |
| --- | --- |
| `prepare-package.sh` | 组装 real 机器人包 |
| `run_robot_ci_once.sh` | real 启动器：流式日志、应用结果及进程状态校验、失败恢复与重试 |
| `prepare-vision-usb-source.sh` | 解析 virtual 提交并准备源码 |
| `prepare-vision-usb-package.sh` | 从源码和本次构建输出组装 virtual 包 |
| `board-orangepi-5-plus.toml` | `cargo xtask starry app board -t aka-rk3588` 的 real 板卡入口 |
| `init.sh` | `starry app board` 使用的 real 板上最小视觉冒烟 |
