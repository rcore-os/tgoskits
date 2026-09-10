# Orange Pi vCPU 性能回归测试

## 1. 测量场景

本用例在 Orange Pi 5 Plus 上新增一次独立的 AxVisor 启动，运行一个 ArceOS 单 vCPU 客户机。它检查调度竞争与周期唤醒同时存在时，实际计算吞吐是否明显下降，不以 kick 或 yield 次数判定性能。本项 CI 不使用 QEMU，仓库原有的其他 QEMU 测试保持不变。

### 1.1 竞争负载

AxVisor 的测试专用 `test-vcpu-perf` 功能启动 `perf_load::start()`：后台线程在物理 CPU 0 上持续执行有限批次计算并主动让出 CPU；`vm.toml` 将客户机 vCPU 放在同一 A55 CPU 0。后台线程有 60 秒期限，正常生产配置不启用该功能。构建使用既有 `ax-driver/rk3588-cpufreq` 驱动，不另改全局调度器或调频策略。

测试不启用 HTTP、WebSocket、浏览器控制台或网络协议栈，也不使用它们制造压力。板卡服务器只负责租用、加载和串口采集；串口每个测量窗口输出一条结果。客户机镜像内嵌于 AxVisor，采用内存分配，无磁盘、摄像头、NPU、轮子或机械臂设备直通，不依赖机器人根文件系统。

### 1.2 计算与判定

`apps/arceos/vcpu-perf/src/main.rs` 的 `work()` 每块执行 256 轮固定整数运算，以 `black_box()` 保留实际计算，启动时执行已知答案检查。另一个客户机线程循环睡眠 1 毫秒并记录实际唤醒，形成周期定时器和调度活动；实际唤醒率不要求达到 1000 Hz。

每次启动预热 3 秒，再测量五个 3 秒窗口。`check.py::evaluate()` 检查完整、有序的六条采样、唯一完成标志、后台负载、非零工作量、时长和至少每秒 50 次实际唤醒，再以五个正式窗口每秒计算块数的中位数比较冻结基准。正常判据为不低于基准的 90%，门槛保存在 `baseline.toml`，不会随当前待测版本自动降低。

## 2. 板卡运行

`run.sh` 构建客户机及 AxVisor，通过既有板卡测试入口租用并启动开发板，随后检查串口日志中的性能结果。需要仓库固定 Rust 工具链、LLVM 工具、Python 3.11 或更新版本，以及已配置且可租用 `OrangePi-5-Plus` 的板卡服务器；不需要 QEMU。

### 2.1 CI 入口

从仓库根目录执行同一入口，完整性和吞吐均通过才输出 `VCPU_PERF_PASS` 并返回 0。构建、加载、超时或性能失败都会使最外层脚本非零退出。

```bash
bash test-suit/axvisor/normal/board-orangepi-5-plus/vcpu-perf/run.sh
```

`.github/ci/checks/axvisor.toml` 单独注册 `test-axvisor-self-hosted-board-orangepi-5-plus-vcpu-perf`，使用 `runner = "board"`。配置名 `orangepi-5-plus-vcpu-perf` 是测试选择名，实际租用类型为 `OrangePi-5-Plus`。只改客户机源码也会由 `scripts/test/ci_impact.py` 路由到该板卡检查，不再路由到 QEMU。

检查显式设置 `suite_preserve_command = true`：只修改测试配置、脚本或门槛文件时，精确路由仍保留完整的构建和性能判定命令，不能退化成只运行底层启动探针。这个选项默认为 false，其他检查继续使用原来的精确子用例命令。

### 2.2 手工运行

其他同型号板卡可通过运行器的 `--board-type`、`--server` 和 `--port` 覆盖连接参数。覆盖的是实验室板卡注册名称，不改变测试配置或启动磁盘；应在无其他任务占用的测试板上运行。

```bash
bash test-suit/axvisor/normal/board-orangepi-5-plus/vcpu-perf/run.sh \
  --board-type <注册的板卡类型> --server <板卡服务器> --port <端口>
```

`cargo xtask axvisor test board --board orangepi-5-plus-vcpu-perf --list` 可检查用例发现。直接去掉 `--list` 运行只验证启动和采样完成，不能代替上面的性能判定脚本。原始运行日志保存在 `tmp/vcpu-perf/run.log`；自动测试结束后由板卡运行器释放租约，无需手工卸载程序。

## 3. 基准维护

`baseline.toml` 已记录 Orange Pi 实机三次独立启动的结果：369906.24、369302.83、369934.27 块/秒。冻结中位数向下取整为 369906，10% 回退预算对应最低 332915.40 块/秒；同时记录参考源码、修复内容和客户机镜像哈希。参考基准来自相同型号与 CPU 亲和性的实机，不是 QEMU 数值或机器人帧率。

### 3.1 采集模式

在正常门禁命令后添加 `--measure` 只采集有效数据，不输出性能 PASS。此模式不得放到 CI 正常命令中，也不能用重跑掩盖首次失败。

```bash
bash test-suit/axvisor/normal/board-orangepi-5-plus/vcpu-perf/run.sh --measure
```

`VCPU_PERF_BASELINE` 可指定另一个 TOML 文件，相对路径以仓库根目录为准。基准为 0、非法参数或无有效定时负载均不能通过正常门禁。更换工作负载、核心类型或固件后需要重新评审基准，不应直接放宽 10% 回退预算。

### 3.2 证据边界

`test_check.py` 覆盖判定阈值、无效负载和失败传播规则；真实板卡对照用于证明测试能识别性能下降。这个用例不证明机器人 FPS、双客户机 IVC、所有跨核中断路径或两个修复各自的独立收益。实机虽然避开了 TCG 运行机竞争，散热、电源、固件和调频差异仍需保持一致。

### 3.3 修复对照

同一块 Orange Pi、同一客户机镜像和配置下，恢复 `reference_base_commit` 中尚未修复的四个 axvm 文件，再恢复修复重测。下面均使用正常门禁入口，而非只采集模式；实机运行期间采用原先的 15% 门槛。预算收紧到当前 10% 后，使用相同原始日志重新运行判定器，三项结果均保持不变；这次复核不计作额外实机启动。

| 实现 | 中位吞吐（块/秒） | 最外层结果 |
| --- | ---: | --- |
| 未修复，第一次 | 273852.87 | 低于门槛，退出 1 |
| 未修复，第二次 | 272419.62 | 低于门槛，退出 1 |
| 恢复修复 | 369388.65 | 性能 PASS，退出 0 |

未修复版本均完成采样且有有效定时负载，失败原因是吞吐不足，不是启动失败或缺少日志。三次基准标定的运行间极差约 0.17%；该结果支持当前板卡环境下的门槛，不代替不同固件或硬件批次的验证。
