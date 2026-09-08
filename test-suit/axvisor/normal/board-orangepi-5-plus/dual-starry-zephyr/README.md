# Orange Pi 5 Plus StarryOS + Zephyr 双客户机机器人测试

该场景通过 AxVisor 同时启动 StarryOS 感知客户机和 Zephyr 控制客户机：

- StarryOS 使用最新 `ivc-sdk` 的 `axivc_publish/send/close` 发布48字节
  `PerceptionResultV2`；
- Zephyr 将 `ivc-sdk` 作为 module 链入镜像，使用
  `axivc_subscribe/recv/close`；
- IVC key 为 `0x49564301`，channel 大小为64 KiB；
- Zephyr 保留1 ms非阻塞轮询、最新帧策略、50 ms机械臂插值和350 ms输入看门狗；
- UART6 只分配给 Zephyr，摄像头、NPU 和 SD 根存储由 StarryOS 使用。
- RTL8125 及其 PCIe、ITS/LPI 资源由 AxVisor 独占，用于 `8080` 网页 Shell。
- 未使用的 USB0 控制器及其 Combo PHY 也保留给 AxVisor，避免 StarryOS 重置宿主管理资源。

本场景不实现 SDK timeout 语义统一、事件驱动通知、断开检测或自动重连。

## 仓库职责

- `tgoskits` 维护 AxVisor、双客户机 VM 配置和板卡测试场景；
- `tgosimages` 维护 StarryOS 与 Zephyr 的系统构建环境和发布产物；
- `aka-rk3588` 维护感知、公共消息协议和 Zephyr 机器人控制应用；
- `ivc-sdk` 维护 StarryOS/Linux 与 Zephyr 共用的 AXIVC 实现。

这些是独立 Git 仓库，不要求使用某个固定的父目录名称。下文命令分别从对应仓库根目录
执行；`tgoskits` 表示标准仓库名，不使用开发者本机 checkout 的别名。

## 构建客户机镜像

StarryOS 与 Zephyr 客户机均通过 `tgosimages` 的标准入口构建。为确保与 AxVisor 主线
对齐，StarryOS 构建应显式记录所使用的 `tgoskits` 提交：

```bash
# 在 tgosimages 仓库根目录执行
./build.sh platform orangepi-5-plus starry --ref <tgoskits-commit>

./scripts/apps/aka-rk3588-zephyr.sh \
  --image-name orangepi-robot-control-sdk
```

Zephyr 专用入口会查找或下载 `aka-rk3588` 和 `ivc-sdk`，再调用 `tgosimages` 管理的
Zephyr 源码、补丁、工具链和 Orange Pi 5 Plus board 完成构建。复现固定版本时，应使用
干净 checkout，并记录 `tgosimages`、`aka-rk3588`、`ivc-sdk` 和 `tgoskits` 的完整提交号；
已有 checkout 不会被构建脚本自动更新或清理。

构建产物位于 `IMAGES/orangepi/starry/orangepi-5-plus`，部署到板卡宿主文件系统的
`/guest/starry/orangepi-5-plus`。TGOSKits 双客户机场景只维护 VM 配置，不再维护或调用
独立的 StarryOS 构建配置。

两份客户机配置都使用标准宿主文件系统路径，并由 AxVisor 根据各自准备后的内存布局自动
选择 DTB GPA。构建产物与安装目标如下：

| `tgosimages` 构建产物 | 板卡宿主文件系统目标 |
| --- | --- |
| `IMAGES/orangepi/starry/orangepi-5-plus` | `/guest/starry/orangepi-5-plus` |
| `IMAGES/orangepi/zephyr/orangepi-robot-control-sdk` | `/guest/zephyr/orangepi-robot-control-sdk` |
| `IMAGES/orangepi/zephyr/orangepi-robot-control-sdk.dtb` | `/guest/zephyr/orangepi-robot-control-sdk.dtb` |

板卡测试只构建并上传 AxVisor，不会自动安装这些客户机资产。部署流程应校验文件非空并在
启动前完成 `sync`。StarryOS 感知程序、模型和运行脚本属于机器人工作负载发布包，不在
本场景 README 中固化某台开发板的安装目录。

## 构建与启动 AxVisor

在标准 `tgoskits` 仓库根目录构建该场景：

```bash
cargo xtask axvisor build \
  --arch aarch64 \
  --config test-suit/axvisor/normal/board-orangepi-5-plus/dual-starry-zephyr/build-aarch64-unknown-none-softfloat.toml
```

构建配置同时启用 `rockchip-sdhci` 和 `rockchip-dwmmc`，但当前只发布并验证 SD 板卡
场景。板卡执行使用本目录的 `board-orangepi-5-plus-dualguest-robot.toml`。

实际上传和启动由项目 CI 或部署环境的标准 board runner 完成。ostool 服务地址、端口和
板卡租约属于运行环境配置，不写入仓库场景说明。启动前必须确认车轮架起且机械臂活动范围
安全。

AxVisor 与 StarryOS 都按发现顺序把 SD 卡
作为 `/dev/mmcblk0p2` 挂载，因此 StarryOS 沿用宿主 bootargs；Linux Guest 的
驱动按 RK3588 设备树别名编号，正式 CI 中才使用 `/dev/mmcblk1p2`。ostool 直接
启动 AxVisor FIT 镜像时不会执行 SD 卡中的 `boot.scr`，所以板配置仍需显式设置
宿主的 SD 根设备。

进入 StarryOS，并切换到已安装的机器人工作负载目录后运行一次有限验收：

```bash
./run_dual_pick_ci_once.sh
```

生产模式使用：

```bash
./run_dual_pick.sh
```

控制台快捷键需要依次按下：按 `Ctrl+X`，松开后再按 `h`、`[` 或 `]`。

## 网页控制台

AxVisor 通过板载 RTL8125 网卡获取 DHCP 地址，并在可信管理网络的
`http://<board-ip>:8080/` 提供 AxVisor、StarryOS 和 Zephyr 三个独立 Shell 窗格。
启动日志中的 `web_console` 字段会给出实际访问地址。网页控制台不启用 TLS 或认证，
不要将端口 `8080` 暴露到非可信网络；物理串口控制台仍可同时使用。

## 安全退出

StarryOS 和 AxVisor 会同时使用机器人根分区。测试结束后不要直接退出板卡会话或
切断电源，应按以下顺序刷新并关闭文件系统：

1. 在 StarryOS Shell 中执行 `sync`；
2. 按 `Ctrl+X`，松开后按 `h` 返回 AxVisor Shell；
3. 依次执行 `vm stop 1`、`vm stop 2` 和 `exit`；
4. 看到 `Goodbye!` 后，按 `Ctrl+A`，松开后按 `x` 释放板卡会话。

AxVisor 的 `exit` 会先调用宿主文件系统关闭流程。StarryOS 用户空间没有以 systemd
作为 PID 1，直接执行 `poweroff` 会失败，不能代替上述流程。SD 测试后应再进行一次
原生 Linux 冷启动，确认启动日志包含 `opi_root: clean`。

## 验收标志

StarryOS：

```text
STARRY_ROBOT_CI_DONE perf=pass
STARRY_PERCEPTION_OK ... ivc_dropped=0
```

Zephyr：

```text
ZEPHYR_IVC_READY ... transport=ivc-sdk
ZEPHYR_CONTROL_STATUS ... invalid=0
ZEPHYR_PICK_CYCLE_PASS cycles=1
ZEPHYR_ROBOT_CI_PASS cycles=1 wheels=verified arm=verified watchdog=verified
ZEPHYR_INPUT_WATCHDOG ... timeout_ms=350
```

2026-09-01 实机验收中，StarryOS 发送1340条结果、丢弃0条；Zephyr 的接收和控制频率约
21～22 FPS，`invalid=0`、`coalesced=0`，完整底盘、抓取、放置和停车流程通过。

## SD 双客户机 CI

`board-orangepi-5-plus-dualguest-robot.toml` 使用与 Linux+Zephyr CI 相同的板卡类型
`OrangePi-5-Plus-DualGuest-robot`。Zephyr 进入稳定 IVC 等待后，测试会在 StarryOS 中运行：

```text
/home/orangepi/robot/aka-rk3588/run_dual_pick_ci_once.sh
```

脚本返回0并完成 `sync` 后才会输出
`DUAL_PICK_CI_PASS guest=starry-zephyr`。成功标志同样由变量组合，不会因 Shell 回显
板卡配置中的命令文本而提前通过。CI 使用同一块 SD 机器人依次运行
Linux+Zephyr 和 StarryOS+Zephyr，两个场景不会并行占用板卡。

## 如需迁移到 eMMC

当前仓库不提供可直接运行的 eMMC board 配置。StarryOS VM 本身无需改变 IVC、
UART6 或内存布局，但迁移时必须：

1. 创建 eMMC 专用 board type 和 board 配置，不与 SD CI 共用板卡池。
2. 根据 U-Boot 和 AxVisor 中的实际枚举结果，删除 SD 专用的显式
   `setenv bootargs`，或将它改为 eMMC 宿主根分区。
3. 将 StarryOS、Zephyr 镜像、用户态运行包和机器人标定部署到 eMMC 根文件系统，
   完成 `sync` 后再冷启动。
4. 保留 `starry-smp1.toml` 中的 UART6、PCIe PHY、USB0 控制器和 Combo PHY 排除项。
5. 重新验证根分区、摄像头、NPU、IVC、UART6、网络 Shell 和完整
   `run_dual_pick_ci_once.sh`；SD 的通过结果不能代替 eMMC 实机验收。
