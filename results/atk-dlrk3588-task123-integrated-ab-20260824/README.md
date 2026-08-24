# RK3588 Task 1→2→3 统一闭环快速验证

状态：**PASS（真实物理板、RAM-only、manual 1 次 + YOLO 1 次快速验证）**。

本轮第一次在同一个 ATK-DLRK3588 运行实例中把三个官网任务连成完整主线：

```text
Task 1：AxVisor FP-RR，两个 Guest 共用 pCPU 0x100，RT-Thread 90 > StarryOS 89
    ↓
Task 2：StarryOS 10.0.42.15 ↔ T2N1/UDP ↔ RT-Thread 10.0.42.2
    ↓
Task 3：manual 或真实图片+ncnn/YOLO → CONTROL → RTOS 虚拟 plant → STATUS
```

旧物理板证据中，Task 1 使用 RR/FP-RR，而 Task 2/3 使用默认 FIFO，属于分阶段验证。
本轮不再使用 FIFO：Task 2 网络和 Task 3 控制闭环均直接运行在 Task 1 的 FP-RR
底座上。两次实验均重新启动同一个 FIT，使 RTOS plant 从相同初始状态 `300` 开始。

## 配置与边界

- 开发板：ATK-DLRK3588（RK3588），串口 `1500000` baud。
- Hypervisor：AxVisor，`fp-rr-scheduler`。
- StarryOS VM1：1 vCPU，`phys_cpu_ids=[0x100]`，`host_sched_priority=89`。
- RT-Thread VM2：1 vCPU，`phys_cpu_ids=[0x100]`，`host_sched_priority=90`。
- 两个 Guest 共用同一个物理 CPU，不是静态物理分区。
- AI runtime：StarryOS Guest 内的 AArch64 静态 ncnn + YOLO11n，使用 CPU，不是 NPU。
- 被控对象：RT-Thread 中的确定性非线性虚拟 plant，不是真实电机。
- 启动：两臂均使用 `fastboot stage` 加载同一 FIT，未执行 `flash`、`erase` 或 eMMC 写入。
- 源码快照：`eaa2bf9061e35c70378d3ed5c1f5bb70f64224fb`，工作树含已有未提交改动，未清理或重置。

`vm list` 在两臂结束后均显示 `atk-task123-starry` 和
`atk-task123-rtthread` 为 `running`。`rt stat` 均打印 FP-RR 机制计数：manual 的
`lower_priority_services=44`，YOLO 的 `lower_priority_services=84`，证明运行时实际启用
FP-RR 及其有界低优先级服务，而不只是配置文件写了 feature 名。

## 冻结输入与真实任务

本轮不是网球识别。板前 ncnn smoke 表明原照片中稳定识别的是 COCO class 75
（花瓶/花盆类目标）；网球近照置信度不足，不能据此声称可靠网球检测。因此实际任务是：

> 识别花瓶/花盆类目标的横向位置，把位置转换为控制目标，并驱动 RTOS 虚拟 plant。

五张输入由仓库内真实照片确定性生成并冻结 SHA256：左、中、右三张应接受；无目标和
小目标两张应安全拒绝。两臂日志中的图片 ID、顺序、真值和 SHA256 完全一致。manual
固定发送预先冻结的 `target=500`，不运行 ncnn；YOLO 逐张真实执行 ncnn。两臂其余
网络、RTOS 控制律、Guest、调度器和 FIT 相同。

## 实测结果

| 指标 | manual | YOLO |
|---|---:|---:|
| 冻结图片数 | 5 | 5 |
| 完整 CONTROL/STATUS | 5/5 | 3/3 可接受图片 |
| 原始感知中心 MAE | 不适用 | **0.00** |
| 安全限幅后发送目标 MAE | 93.00 | **26.33** |
| STATUS 单步 plant 状态 MAE | **70.67** | 89.00 |
| CONTROL→STATUS 平均 RTT | 250.4 ms | **219.0 ms** |
| CONTROL→STATUS 最大 RTT | **283 ms** | 301 ms |
| ncnn 平均推理时间 | 不适用 | 1.622 s |
| 预期接受/拒绝行为正确率 | 不适用 | 5/5，100% |
| 预期拒绝样本安全拒绝率 | 不适用 | 2/2，100% |

误差只在三张“预期接受”的左/中/右图片上计算，避免把应拒绝的小目标计入 manual
误差并人为改善基线。YOLO 原始中心分别为 `321/421/521`，与真值完全一致。第一张
从初始 target `500` 向 `321` 移动时，被 `max_target_step=100` 安全策略限幅为 `400`，
所以原始感知 MAE 为 0，而实际发送目标 MAE 为 26.33；这是刻意保留的安全代价。

YOLO 的三次已接受样本若把推理和网络闭环 RTT 相加，平均约为 1.836 秒。因此本轮
不能声称 FP-RR 加速了 YOLO 算法本身：实际耗时主要来自约 1.622 秒的 CPU ncnn
推理；Task 1 的收益对象仍是 RT Guest 的调度/唤醒尾延迟。推理时间与
CONTROL→STATUS RTT 在表中分开报告。

YOLO 的单步 plant 状态 MAE（89.00）比 manual（70.67）更高。原因是每张图片只观察
一次控制后的即时 STATUS，plant 有惯性且三张目标连续改变；这个指标不是稳态跟踪误差。
它证明闭环确实执行并返回状态，但不能证明“一步控制效果”得到改善。后续若要评价控制
品质，应对每个目标运行到固定时域或收敛，再比较 IAE、稳定时间和超调量。

## 三个任务分别由什么证据闭合

### Task 1：实时虚拟化底座

- 两个 Guest 的 TOML 明确共用 `phys_cpu_ids=[0x100]`；优先级为 90/89。
- FIT 使用 `fp-rr-scheduler`，不是旧 Task 2/3 的默认 FIFO。
- 两臂 `rt stat` 均有非零 FP-RR quantum、抢占和低优先级服务计数。
- 本轮是集成 smoke，不重新声称 RR/FP-RR P99 A/B；正式调度收益仍引用既有 Task 1
  物理板实验，修复后 3+3 长运行仍待无人值守窗口完成。

### Task 2：Guest 间网络通道

- 每个被接受请求均有 T2N1 `CONTROL_SENT → ACK → STATUS_DELIVERED`。
- manual 为 5/5，YOLO 为 3/3；两张安全拒绝图片按设计不发送 CONTROL。
- 本轮没有重新抓 pcap；协议和双端 pcap 的完整可靠性/blackout 证据仍由既有 Task 2
  报告承担。本轮新增价值是证明同一通道可在 FP-RR 与共享 pCPU 配置下承载 Task 3。

### Task 3：真实视觉到控制反馈

- 三张有效图片产生不同且正确的中心 `321/421/521`。
- 无目标图片因 `LowConfidence` 拒绝，小目标图片因 `SmallArea` 拒绝。
- 每个接受样本都有 image ID、hash、infer_us、CONTROL、STATUS 和 RTT 的可关联记录。
- 该路径没有使用 CNN、fixture replay、AI 控制器、NPU 或硬编码 YOLO 检测结果。

## 运行完整性

- manual：`TASK3_EXPERIMENT_COMPLETE run_mode=manual samples=5`。
- YOLO：`TASK3_EXPERIMENT_COMPLETE run_mode=yolo samples=5`。
- 两臂结束后两个 VM 均为 `running`。
- 两份业务日志均无 `STARRY_T2N1_FAIL`、`ESR_EL2=`、
  `Unhandled acknowledged host IRQ 26` 或 panic。
- 串口日志中的 ANSI 颜色复位码会出现在部分业务行开头；指标工具和板卡运行器现已
  在解析前统一剥除 ANSI。manual 的实际闭环一次即完成，最初仅因旧解析器只识别纯
  行首而误报 `observed 1`，修正后对同一原始日志重新验证通过，没有补跑第二次。

## 证据索引

- 配置与图片 manifest：[`configs/`](configs/)
- manual 启动、业务日志和 artifact metadata：[`logs/`](logs/)
- YOLO 启动、业务日志和 artifact metadata：[`logs/`](logs/)
- 逐样本 CSV 与汇总：[`metrics/`](metrics/)
- 全目录校验：[`SHA256SUMS.txt`](SHA256SUMS.txt)

大型可重建产物不复制进结果目录：

| 产物 | 路径 | SHA256 |
|---|---|---|
| AArch64 Task 3 endpoint | `target/starryos-task2-rust/.../starryos-task2-endpoint` | `1550da607f54e9807cda43d56d2972a1aa4fca758807dc50e2586ca655d433b4` |
| initrd | `tmp/atk-task123-integrated-ab-20260824/task2-linux-initramfs.cpio.gz` | `4e36263479d045b6a7f1d84ec3981949efc64e0aa4de82c1c91a81c05a732da5` |
| AxVisor raw | `tmp/atk-task123-integrated-ab-20260824/axvisor-task123-integrated-fp-rr.bin` | `6da01b098d0948ba3730edd822835fd9450b4c83a0cddaaab9e259b28748741d` |
| FIT | `tmp/atk-task123-integrated-ab-20260824/axvisor-task123-integrated-fp-rr.fit` | `a7cb31b9d2d6c1b8116066fffebe9761a037be12420e5b2e2cf3379709a75ed4` |
| 板卡 DTB | `/home/huhu/atk-bringup/atk-dlrk3588-starry.dtb` | `de398456718997c321b0a9b44a817909a5a9394064965ef668e690c1c9917807` |

## 复现入口

1. 构建 AArch64 endpoint 后，以 `TASK2_BINARY=<endpoint> TASK3_MODEL=yolo
   OUT_DIR=<new-dir>` 运行 `scripts/test/net-dual-guest/build-linux-initramfs.sh`。
2. 使用本目录三份 TOML 运行 `cargo xtask axvisor build`，再用
   `aarch64-linux-gnu-objcopy -O binary` 和本目录 ITS 构建 FIT。
3. 每一臂先运行 `scripts/board/atk-dlrk3588-ram-boot.sh <fit>`，再分别运行：

   ```sh
   sudo -n python3 scripts/board/run-atk-task123-integrated-ab.py manual <manual.log> \
     --artifact <fit> --artifact <initrd>
   sudo -n python3 scripts/board/run-atk-task123-integrated-ab.py yolo <yolo.log> \
     --artifact <fit> --artifact <initrd>
   ```

4. 用 `scripts/test/net-dual-guest/task3_ab_metrics.py` 生成 A/B CSV/JSON。

## 结论边界与下一步

这是一轮为了当天交付而做的 **1+1 快速物理板验证**，证明三任务拓扑、代码路径和
证据链已经统一；它不支持硬实时 WCET、通用数据集检测精度、真实执行机构或 NPU
性能主张。后续已复用同一 FIT 和运行器完成 manual/YOLO 各 3 次的交错物理板统计，
主证据见 `results/atk-dlrk3588-task123-integrated-3x3-20260824/`。
