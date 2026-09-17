# aka-rk3588 tennis robot

该应用用于在 Orange Pi 5 Plus 的 StarryOS 环境中运行 RK3588 网球机器人视觉程序。
StarryOS 直接使用 Linux 预先部署到共享根文件系统
`/home/orangepi/robot-ci/aka-rk3588` 的程序，不依赖 StarryOS 网络下载。

仓库保留已经在 Orange Pi Jammy 上使用 GCC 11 原生编译并完成实机验证的 AArch64
`tennis` 程序。模型、`librknnrt.so`、默认配置和运行脚本由
[`prepare-package.sh`](prepare-package.sh) 从 `aka-rk3588` 固定提交下载，不在本仓库
重复保存。

固定源码版本和 SHA256 记录在 [`source.env`](source.env) 中。当前程序最高依赖
`GLIBC_2.34`，适用于机器人共享的 Jammy/StarryOS 根文件系统，不适用于仅含 musl
的通用 Alpine rootfs。

## 准备部署包

在开发主机执行：

```bash
cd apps/starry/aka-rk3588
./prepare-package.sh
```

脚本下载固定提交源码归档并校验 SHA256，然后用仓库中的预编译 `tennis` 替换源码
归档中的构建产物。生成文件为：

```text
target/aka-rk3588/aka-rk3588.tar.gz
```

已有归档可通过环境变量复用，进行无网络打包：

```bash
AKA_RK3588_SOURCE_ARCHIVE=/path/to/aka-rk3588-f5d2c731a13692a1e3bc7136188df3f2ffc541c1.tar.gz \
  ./prepare-package.sh
```

## Linux 部署

先启动开发板 Linux，将部署包传入开发板。不要覆盖一台已经完成实机校准的机器人
配置；部署前应备份：

```text
/home/orangepi/robot-ci/aka-rk3588/config
```

然后将部署包解压到：

```text
/home/orangepi/robot-ci/aka-rk3588
```

每台机器人的 `lekiwi_calibration.json` 和 `lekiwi_pick_config.txt` 应继续使用各自的
实机校准与调试结果。

首次迁移时，从本板旧目录 `/home/orangepi/robot/aka-rk3588/config` 复制校准配置到
新目录；保留整个旧目录，供合入前的 CI 使用。先部署并验证新目录，再合入路径修改，
避免新旧程序与启动脚本混用。不要用其他板卡的校准文件覆盖本板配置。

## StarryOS 安全演示

完成 Linux 部署后执行：

```bash
cargo xtask starry app board -t aka-rk3588 \
  -b OrangePi-5-Plus-robot
```

默认演示直接从共享根文件系统运行一次摄像头采集和 RKNN 网球识别，不驱动车轮和
机械臂。成功标志为：

```text
AKA_RK3588_DEMO_PASSED
```

完整捡球流程必须在已校准且周边安全的机器人上手动执行：

```bash
cd /home/orangepi/robot-ci/aka-rk3588
export LD_LIBRARY_PATH="$PWD/lib:${LD_LIBRARY_PATH:-}"
./run_lekiwi_full.sh
```

## 更新版本

更新时应使用新的完整提交号和真实源码归档 SHA256，并在目标兼容的 AArch64 Linux
环境重新生成 `tennis`。替换预编译程序后同步更新 `source.env` 中的二进制 SHA256。
不要使用分支名或 `HEAD` 作为下载和构建输入。

## CI 正确性与性能门槛

固定版本 `f5d2c73` 要求真实推理成功、两个完整性能窗口、控制流程完成和零退出码。
性能窗口前须通过三轮双向速度反馈检查，流程结束时须连续三次读到三轮速度均为零；
应用在上述检查和模型释放均成功后输出唯一 `APPLICATION_PASS`；启动器要求该结果及
零退出码，不再根据中间诊断拼出成功结论。命令错误不会被后续
成功停车清除，推理失败或提前中断也不会输出总 PASS。部署包使用真实 Feetech 执行器。
启动脚本优先从自身 `lib/` 加载 RKNN 运行库，支持独立 CI 目录部署。

robot Starry guest 使用专用
`test-suit/axvisor/normal/board-orangepi-5-plus/robot-starry/guest.toml`，
保持单 CPU 0（MPIDR `0x00`），并嵌入当前工作区构建的 Starry 镜像。
CI 同时响应 AxVisor 和 Starry 的相关修改，先构建 guest 再运行板卡测试。
手动运行也必须按同样顺序准备 guest，避免复用陈旧的 target 产物：

```sh
cargo xtask starry build \
  --config test-suit/starryos/board-orangepi-5-plus/robot-flow/build-aarch64-unknown-none-softfloat.toml \
  --smp 1
cargo xtask axvisor test board --board orangepi-5-plus-robot-starry
```

三个 robot board TOML 的 `shell_check_steps.shell_cmd` 显式传入
`./run_robot_ci_once.sh 28.0`。调整性能门槛应修改对应配置文件中的参数，
不通过宿主环境变量覆盖。程序的 `PERF_BEGIN` 输出实际门槛，
`PERF_WINDOW` 和 `PERF_SUMMARY` 输出处理帧数、实际耗时及 FPS。
性能窗口使用真实摄像头和 RKNN；完整通过还要求执行器控制和最终停车成功。

当前 Direct DMA NPU 提交要求调用进程持有 `CAP_SYS_RAWIO`；
这不提供无特权 IOMMU 隔离。修复原因、引入提交和性能基线见
[`rknpu-privileged-submit.md`](../../../docs/design/rknpu-privileged-submit.md)。
`tests/rknpu-submit-access.c` 验证无效对象、GEM 归属、越界和降权后继承 fd 的拒绝路径；
仅在 Starry root shell 执行，不在 Linux vendor 驱动上执行这些无效对象测试。

## 部署兼容性与基线

运行目录固定为 `/home/orangepi/robot-ci/aka-rk3588`，源码提交号和产物哈希记录在
部署目录的 `SOURCE` 文件中。部署时持有板卡租约，确认没有程序运行，先备份当前
目录，再整体切换包含程序、启动器、运行库、模型和本板校准配置的暂存目录。
不要逐个覆盖正在使用的文件。更早的 `/home/orangepi/robot/aka-rk3588` 保留不变。

程序内部使用新版 `APPLICATION_PASS`，启动器对外仍输出原有 `RESULT=PASS/FAIL`，
因此合入前后使用同一固定路径的 board 配置均可识别结果。新旧程序和启动器不能混用。

基线约 30 FPS，三个正式配置的门槛为 28 FPS；成功以两个约 10 秒窗口的总帧数除以
总耗时判断。最终性能汇总延后到动作、停车和清理完成后输出。该基线验证真实 NPU
和执行器控制，不要求球存在，也不表示验证了真实抓球成功率。
