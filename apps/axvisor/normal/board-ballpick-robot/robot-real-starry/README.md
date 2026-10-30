# AxVisor StarryOS 单客户机 USB 捡球应用

本应用在 RK3588 板卡上启动单个 StarryOS 客户机，由客户机通过 USB 摄像头采集图片、
使用 NPU 推理，并通过 Feetech USB 控制器驱动车轮和机械臂。宿主启动、手工检查与
自动用例共用本目录的构建和客户机配置。

## 1. 应用用途

本例运行板端已有的完整捡球程序，持续完成网球识别、靠近、抓取、寻找收集桶和放球。USB 摄像头为 `0ac8:0346`，Feetech 控制器为 `1a86:55d3`。

### 1.1 控制链路

使用 `FEETECH_DEV=auto` 让程序选择 USB 控制器，不使用 SoC UART6 `/dev/ttyS6`。
USB 控制器可能呈现 USB 串口节点，实际控制设备由运行包自动识别；只看到枚举结果
不能证明电机响应正常。程序在同一客户机中执行感知和控制，无需 Zephyr 或 IVC 对端。

### 1.2 运行条件

运行前核对本机真实标定，准备待捡网球和收集桶，并留出车轮与机械臂活动空间。
完整程序持续寻找并处理下一颗球，实际动作取决于摄像头识别结果和现场环境。

## 2. 运行前确认

宿主和客户机使用板卡已有的 `/dev/mmcblk0p2` 根文件系统。切换板卡前，在原生 Linux
中核对根设备、内核和程序目录，避免把其他板卡的路径直接套用到本例。

### 2.1 客户机内核

在工作站的 TGOSKits 仓库根目录使用本应用独立的 [configs/starry-guest-build.toml](configs/starry-guest-build.toml)
编译 StarryOS 客户机。该配置保留机器人所需的功能项，设置 `log = "Error"`、`max_cpu_num = 1`：

```bash
cargo xtask starry build \
  --config apps/axvisor/normal/board-ballpick-robot/robot-real-starry/configs/starry-guest-build.toml \
  --smp 1
```

[guest.toml](guest.toml) 直接引用工作站产物
`target/aarch64-unknown-none-softfloat/release/starryos.bin`，宿主构建会将其打包进
initramfs，无需覆盖板端 `/guest/starry/`。修改客户机源码后，重新执行上述构建和宿主启动。

### 2.2 机器人运行包

板端 `/home/orangepi/robot/aka-rk3588-single` 应包含 `bin/tennis`、`lib/`、`models/`、
`config/` 和 `run_single_pick.sh`。`config/` 使用这台机器的标定与控制参数，
运行沿用现有文件；缺少资源时先报告缺项，不通过覆盖标定或替换系统库绕过错误。

在原生 Linux 中只读核对资源：

```bash
findmnt -n -o SOURCE,FSTYPE /
lsusb -d 0ac8:0346
lsusb -d 1a86:55d3
ls -l /home/orangepi/robot/aka-rk3588-single/bin/tennis
ls -l /home/orangepi/robot/aka-rk3588-single/run_single_pick.sh
ls /home/orangepi/robot/aka-rk3588-single/config
```

程序、脚本、运行库、模型与标定均准备好后释放原生 Linux 会话，再运行 AxVisor。
无需向板端复制宿主或客户机内核；运行器通过会话加载本地构建的宿主。

## 3. 启动

工作站命令从 TGOSKits 仓库根目录执行。将 `<板卡类型>`、`<服务地址>`、`<端口>`
替换为实际值；默认机器人板型为 `BallPick-Robot`。PATH 中应有 AArch64 musl 工具链。

### 3.1 启动宿主

交互配置 [configs/uboot-shell.toml](configs/uboot-shell.toml) 不注入客户机检查命令，
不设置终端总超时。直接指定本目录中的配置路径：

```bash
cargo xtask axvisor board \
  --config apps/axvisor/normal/board-ballpick-robot/robot-real-starry/build-aarch64-unknown-none-softfloat.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/robot-real-starry/configs/uboot-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

`vm_configs` 打包 VM1 的配置，`axvisor.vm_start=1` 自动创建并启动它。
客户机资源为1 vCPU，MPIDR `0x00`，256 MiB 低地址内存及 2 GiB 高地址内存，设备树采用宿主基线，不附加 UART6 专用设备树或初始化命令。

### 3.2 进入客户机

看到 `axvisor:/$` 后，确认 VM1 为 `running`，再接入控制台：

```text
vm list
vm console 1
```

等待 `root@starry:~#` 后再输入业务命令。`Ctrl+X` 后按 `h` 返回宿主，`Ctrl+A` 后按 `x`
结束工作站串口会话；外层交互命令返回 0 只表示会话正常结束。

## 4. 运行捡球程序

在客户机中确认运行包可访问，再启动完整捡球程序。运行前确认没有其他程序占用摄像头
或执行器 USB 控制器。

### 4.1 检查运行环境

在客户机 Shell 中执行，确认程序和模型存在，根盘中的真实标定可读：

```sh
test -x /home/orangepi/robot/aka-rk3588-single/bin/tennis
test -x /home/orangepi/robot/aka-rk3588-single/run_single_pick.sh
test -s /home/orangepi/robot/aka-rk3588-single/models/tennis.rknn
ls /home/orangepi/robot/aka-rk3588-single/config
```

任一检查失败时先核对根分区和部署目录。StarryOS 使用 root Shell，脚本从包内 `lib/` 加载运行库。

### 4.2 执行完整捡球程序

在同一客户机 Shell 中进入程序目录并启动：

```sh
cd /home/orangepi/robot/aka-rk3588-single
./run_single_pick.sh auto 0
```

`auto` 自动选择 Feetech USB 控制器，`0` 指定摄像头索引。脚本加载包内运行库和模型，
启动 `bin/tennis`，持续执行识别、靠近、抓球和放球流程，直到人工停止或发生不可恢复错误。

## 5. 运行现象

机器人识别到网球后靠近并抓取，再寻找收集桶并放球，随后继续寻找下一颗球。观察实际
识别结果、车轮运动和机械臂动作；没有检测到目标时，按现场画面与程序日志判断状态。

## 6. 结束

需要结束时在客户机控制台按 `Ctrl+C`，等待程序退出并确认车轮停止，再执行 `sync`。
随后按 `Ctrl+X`、`h` 返回 AxVisor，
按 `Ctrl+A`、`x` 结束串口并等待运行器释放租约。包含物理设备直通的客户机应按板级
复位流程重新运行，不用 `vm stop/reset/delete` 代替 USB、DMA 和设备状态回收。

运行复用现有程序和标定，程序可能在运行目录产生图片等输出。

## 7. 自动测试

CI 使用 `run_single_pick_ci_once.sh 28.0` 执行有限流程，检查两个性能窗口、执行器反馈
和安全停止；`28.0` 是测试帧率门槛。该流程用于测试，可能使用模拟球就绪条件。
需要运行自动测试时，按测试要求架空车轮，并从工作站仓库根目录执行：

```bash
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/robot-real-starry \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

测试要求最终 `[ROBOT_CI] RESULT=PASS attempts=1/2` 且外层退出码为 0；有限流程通过
不代表实际抓球成功。自动测试期间不要同时运行完整捡球程序。

## 8. 配置说明

交互配置与自动配置分别负责人工会话和自动判定，使用同一份宿主构建输入及客户机定义：

| 文件 | 作用 |
| --- | --- |
| [build-aarch64-unknown-none-softfloat.toml](build-aarch64-unknown-none-softfloat.toml) | 构建 AxVisor 并打包 VM1 配置 |
| [guest.toml](guest.toml) | 客户机内核、根分区、CPU、内存和设备分配 |
| [configs/starry-guest-build.toml](configs/starry-guest-build.toml) | 单 CPU StarryOS 客户机构建，Error 日志及机器人功能项 |
| [configs/uboot-shell.toml](configs/uboot-shell.toml) | 保留人工串口会话，不注入业务命令 |
| [smoke/board-orangepi-5-plus-robot-real-starry.toml](smoke/board-orangepi-5-plus-robot-real-starry.toml) | 板型、启动参数、自动业务命令与结果判定 |

AxVisor Nightly 的 `test-orangepi-5-plus-robot-real-suite` 调用本应用，修改控制入口、
板端资源路径或通过条件时，需同步核对自动配置和本文档。
