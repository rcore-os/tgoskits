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

本场景不增加 SDK 级统一 timeout、事件驱动通知或通用断线恢复能力。AKA 控制应用
在允许重新配置的状态下有轮询式重订阅；FAULT 状态不提供通用自动恢复。

## 仓库职责

- `tgoskits` 维护 AxVisor、双客户机 VM 配置和板卡测试场景；
- `tgosimages` 维护 StarryOS 与 Zephyr 的系统构建环境和发布产物；
- `aka-rk3588` 维护感知、公共消息协议和 Zephyr 机器人控制应用；
- `ivc-sdk` 维护 StarryOS/Linux 与 Zephyr 共用的 AXIVC 实现。

这些是独立 Git 仓库，不要求使用某个固定的父目录名称。下文命令分别从对应仓库根目录
执行；`tgoskits` 表示标准仓库名，不使用开发者本机 checkout 的别名。

## 构建客户机镜像

StarryOS 由当前 TGOSKits checkout 构建并嵌入 AxVisor，确保 CI 使用本次源码生成的
客户机。单 vCPU 配置保留原有核心分配，日志级别为 Warn，避免逐帧内核日志进入性能测量。

```bash
# 在 tgoskits 仓库根目录执行，顺序与 CI 一致
cargo xtask starry build \
  --config test-suit/axvisor/normal/board-orangepi-5-plus/dual-starry-zephyr/starry-guest-build.toml \
  --smp 1
cargo xtask axvisor build \
  --arch aarch64 \
  --config test-suit/axvisor/normal/board-orangepi-5-plus/dual-starry-zephyr/build-aarch64-unknown-none-softfloat.toml
```

StarryOS 产物为 `target/aarch64-unknown-none-softfloat/release/starryos.bin`；修改客户机
源码后需依次重新构建 StarryOS 和 AxVisor。DTB GPA 由准备后的客户机内存布局自动选择。

Zephyr 继续使用 TGOSImages 的标准构建入口：

```bash
# 在 tgosimages 仓库根目录执行
./scripts/apps/aka-rk3588-zephyr.sh --image-name orangepi-robot-control-sdk
```

该入口使用 TGOSImages 管理的 Zephyr、补丁和工具链，并调用 AKA 控制应用及 IVC SDK。
部署 `IMAGES/orangepi/zephyr/orangepi-robot-control-sdk` 及同名 `.dtb` 到板卡
`/guest/zephyr/`，并与感知程序、模型、脚本和机器人标定保持匹配。记录各仓库版本和
未提交补丁；已有 checkout 不会被构建脚本自动更新或清理。部署后完成 `sync`。
板卡测试上传 AxVisor 及内嵌 StarryOS，Zephyr 和用户态资产仍需预先安装。

## 构建与启动 AxVisor

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

普通持续运行时，进入 StarryOS 中已安装的机器人工作负载目录，执行：

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
作为 PID 1，直接执行 `poweroff` 会失败，不能代替上述流程。

## SD 双客户机 CI

这是 StarryOS + Zephyr 的实体板集成回归测试：检查内核或驱动更新后，是否出现
“能够启动，但摄像头/NPU 变慢、两客户机失联或执行器没完成动作”的问题。
这些真实设备与时序无法仅由主机单元测试证明。AxVisor 提供 IVC 和设备隔离，StarryOS
负责感知，Zephyr 独占 UART6，负责决策、底盘及机械臂。

StarryOS 登录 Shell 出现 `root@starry:/root #` 提示符后，由本目录的 board 配置运行：

```sh
/home/orangepi/robot/aka-rk3588/run_dual_pick_ci_once.sh --min-fps 28
```

消息和失败处理按以下顺序进行；IVC 通道 key 为 `0x49564301`：

1. **配置。** StarryOS 读取本板标定与控制参数，发送带 session、配置 CRC 的 `BEGIN`、
   9 条 `CHUNK`、`COMMIT`。Zephyr 逐条返回 `ACK`，校验并应用配置后返回 `APPLIED`。
   回复错误、拒绝或超时均失败；BEGIN 最多等 90 秒。配置完成后才开始性能计时。
2. **感知与动作。** StarryOS 每帧真实采集、解码、运行 NPU 和红桶检测，经 IVC 发送
   48 字节 `PerceptionResultV2`：序号、时间戳及球/桶的可见性、位置、大小、置信度。
   Zephyr 取最新有效结果决定底盘动作，以独立的 50 ms 周期推进机械臂；逐帧结果
   **没有 ACK**。前两个约 10 秒窗口各须达到 28 FPS。约第 20 秒起仍真实推理，
   但把发送给 Zephyr 的球/桶结果替换为预设场景，以驱动实际控制流程。
3. **执行反馈。** Zephyr 检查机械臂受检终点，随后对三轮依次执行停止、正转、停止、
   反转、停止。每阶段最多 2 秒，读取实际位置与速度；正反转需方向和位置变化正确，
   停车需四组采样的位置稳定、速度在允许范围内。检查位 `checks=31` 仅在周期、终点、
   停车命令、双向运动和最终停车反馈都完成后才成立。错误会锁定本次 CI 失败。
4. **完成回复。** 感知程序至少运行 62 秒后发送 `CONTROL_FINISH`，等待最多 10 秒；
   期间继续发送末尾场景，避免输入断流。Zephyr 返回 `CONTROL_RESULT`，状态为
   `pending`、`failed` 或 `success`，附本次 session、配置 CRC、完成周期、检查位和错误码。
   旧会话回复不能充当成功；失败回复、错误检查位或等不到成功回复都会使 CI 失败。
5. **最终判定。** StarryOS 关闭摄像头、释放模型并关闭 IVC 后，才生成唯一的
   `DUAL_PICK_APPLICATION_PASS`。脚本核对两个窗口、匹配的控制结果、至少 62 秒时长、
   清理及零退出码；board 入口还须 `sync` 成功，才输出独占一行的
   `DUAL_PICK_CI_PASS guest=starry-zephyr`。中间日志和 Zephyr 串口文字不能代替最终结果。

NPU 调用错误、IVC 发送失败或丢帧、执行器读写/反馈错误都会失败；进入 CI 控制模式后，
未完成时约 350 ms 输入断流也会停车并锁定失败。少量坏帧允许剔除并记录，连续采集/解码
失败达到 10 次则退出，坏帧不计入有效 FPS。这项测试验证真实感知性能、双向通信和
受控执行器反馈；预设场景允许模拟夹持成功，不能证明识别准确率、真实抓球入桶、
地面行驶、长期稳定性或硬件急停。

本 SD 场景使用板卡类型 `OrangePi-5-Plus-DualGuest-robot`，与 Linux + Zephyr 场景顺序
共用同一块板。CI 从当前 checkout 构建 StarryOS 并嵌入 AxVisor，确保源码修改得到测试。
感知程序、Zephyr 镜像和 CI 脚本须配套更新。board 入口以 `shell_check_steps`
逐步注入短命令。

## 如需迁移到 eMMC

当前仓库不提供可直接运行的 eMMC board 配置。StarryOS VM 本身无需改变 IVC、
UART6 或内存布局，但迁移时必须：

1. 创建 eMMC 专用 board type 和 board 配置，不与 SD CI 共用板卡池。
2. 根据 U-Boot 和 AxVisor 中的实际枚举结果，删除 SD 专用的显式
   `setenv bootargs`，或将它改为 eMMC 宿主根分区。
3. 将 Zephyr 镜像、用户态运行包和机器人标定部署到 eMMC 根文件系统，
   完成 `sync` 后再冷启动。
4. 保留 `starry-smp1.toml` 中的 UART6、PCIe PHY、USB0 控制器和 Combo PHY 排除项。
5. 重新验证根分区、摄像头、NPU、IVC、UART6、网络 Shell 和完整
   `run_dual_pick_ci_once.sh`；SD 的通过结果不能代替 eMMC 实机验收。
