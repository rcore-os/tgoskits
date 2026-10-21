# AxVisor 客户机任务切换测量

## 测试用途

通过 AxVisor 在 RK3588 上启动一个 ArceOS 客户机，测量两个同优先级 FIFO(80) 任务的 `yield` 往返开销。客户机运行30轮，每轮1,000万次切换，记录纳秒、PMU 周期和 GPIO3_C6 时序。

## 边界

这个场景由 `axvm` 的 `task_switch` 内核测试目标驱动。它用 `VmCreatePlan` 为宿主 vCPU 任务安装 FIFO 策略，并检查客户机汇总结果，因此正式测量使用 `cargo xtask ktest board`。手动启动一个普通 VM 不能替代这份调度策略和判定。

客户机在 EL1 读取 `CNTVCT_EL0`、`CNTFRQ_EL0`、`PMCCNTR_EL0`。固件须允许对应 PMU 访问，`configs/task-switch.toml` 须正确分配 GPIO3_C6 的物理窗口。GPIO 或计时器初始化失败时，应停止测量，不能接受零值结果。

## 运行前确认

从 TGOSKits 仓库根目录执行，使用仓库固定工具链及 AArch64 musl 交叉工具链。客户机由本次源码构建，随宿主打包，不需要预装板端 task-switch BIN。

### 构建输入

[configs/guest-build.toml](configs/guest-build.toml) 选择 `task-switch` 软件包、`bench-fifo-policy` 和单 CPU；[guest-builds.toml](guest-builds.toml) 让正式入口在宿主之前构建客户机。需要单独检查构建时执行：

首次执行下面这组命令前，在本文档所在目录打开构建机终端；代码会记录应用路径并返回仓库根目录。后续构建机命令沿用 `APP_DIR` 和 `APP_CASE`。

```bash
APP_DIR="$PWD"
cd "$(git rev-parse --show-toplevel)"
APP_CASE=$(realpath --relative-to=apps/axvisor/normal "$APP_DIR")

app="$APP_DIR"
cargo xtask arceos build --config "$app/configs/guest-build.toml"
```

[build-aarch64-unknown-none-softfloat.toml](build-aarch64-unknown-none-softfloat.toml) 包含 `host-fs` 和存储驱动，宿主仍依赖板卡启动配置中的根分区。客户机固定单 vCPU、MPIDR `0x400`，两个测量任务在同一物理核上切换。

### 服务与设备

服务地址按当前环境填写。运行配置的 `board_type` 选择实体板卡，ktest 的 `-b orangepi-5-plus` 选择逻辑板卡配置；需要外部时序旁证时，按 GPIO3_C6 的设备连接准备示波器。

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
ostool board ls --server "$BOARD_SERVER"
```

## 启动

正式入口自动准备客户机并创建 VM，不需要向管理控制台输入 `vm start` 或 `vm console`。在构建机执行：

```bash
app="$APP_DIR"
cargo xtask ktest board -p axvm --test task_switch -b orangepi-5-plus \
  --config "$app/build-aarch64-unknown-none-softfloat.toml" \
  --board-config "$app/board-ballpick-robot-task-switch.toml" \
  --server "$BOARD_SERVER"
```

## 场景：观察完整测量

客户机先测 `rdtsc` 和任务创建，再进行30轮切换，保持完整工作量，不提前截取部分结果作为最终值。

### 等待任务创建阶段

`bench_spawn()` 可能长时间不打印输出；此前实测约16分钟，完整运行约半小时。`rdtsc` 结果之后静默不一定是挂起。整次执行仍受板卡配置的3600秒超时约束，超过上限应保留日志并判定本轮未完成。

### 核对逐轮结果

每轮应有 `AXVISOR_TASK_SWITCH_GROUP_SUMMARY`，index 从0到29。`samples_per_direction=5000000` 表示单方向采样数，每轮合计1,000万次切换；`avg_cycles` 是每次切换的平均周期数。最后的 `TASK_SWITCH_SUMMARY` 聚合全部轮次。

```text
AXVISOR_TASK_SWITCH_GROUP_SUMMARY index=0 samples_per_direction=5000000 avg_cycles=... min_cycles=... max_cycles=...
...
TASK_SWITCH_SUMMARY rounds=30 samples_per_round=10000000 avg_ns=... avg_cycles=...
AXTEST_SUITE_OK
```

这里的省略号是输出格式示意，实际日志必须包含非零数值。`convert::ticks_to_nanos()` 等函数负责单位换算，往返包含两次上下文切换，不能把每次与每往返的开销混为一谈。

## 判定

30轮全部完成、汇总值非零、出现 `AXTEST_SUITE_OK` 且外层命令返回0才通过。`BENCHER_GPIO_FAILED`、`BENCHER_MEASUREMENT_FAILED`、panic、提前退出或超时均不通过。某次实测均值不是本例的固定性能门槛。

纯算术换算可以另用 `cargo xtask cross-test --arch aarch64 -p task-switch --no-default-features` 检查，但不能代替板卡上的调度、PMU 和 GPIO 测量。

## 结束

正常完成后运行器释放会话。若提前中断，记录已完成轮次、最后输出和退出码，确认租约结束再重新启动。保存源码版本、镜像摘要、CPU 频率、每轮采样数和最终均值，作为本次结果的依据。

## 配置说明

本目录与同板卡的应用集中维护，CI 仍在 benchmark 阶段使用同一份 ktest 入口和性能报告字段。

原 [benchmarks 副本](../../../../../benchmarks/axvisor/board-orangepi-5-plus/task-switch/README.md) 保留现有 CI 性能入口；本目录提供当前应用板卡的运行入口。修改任务切换工作量、客户机构建参数或汇总判定时，修改者须同步核对两份配置；板卡类型和相对资源路径分别按各自场景维护。

| 文件 | 作用 |
| --- | --- |
| [configs/guest-build.toml](configs/guest-build.toml) | 客户机软件包、目标和 FIFO 功能 |
| [guest-builds.toml](guest-builds.toml) | 宿主之前的客户机构建声明 |
| [build-aarch64-unknown-none-softfloat.toml](build-aarch64-unknown-none-softfloat.toml) | AxVisor 宿主构建和打包输入 |
| [configs/task-switch.toml](configs/task-switch.toml) | 单 vCPU、镜像、内存和直通资源 |
| [board-ballpick-robot-task-switch.toml](board-ballpick-robot-task-switch.toml) | 启动环境、超时及成功与失败匹配 |
