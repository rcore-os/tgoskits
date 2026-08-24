# RK3588 Task 3 manual/YOLO 3+3 物理板量化

状态：**PASS（真实 ATK-DLRK3588、真实 Guest 内 ncnn/YOLO、manual 3 次 +
YOLO 3 次）**。

本批次补齐了早先 `1+1` 快速验证缺少的重复统计。六轮按以下顺序交错执行：

```text
manual-1 → yolo-1 → manual-2 → yolo-2 → manual-3 → yolo-3
```

每轮都用 `atk-dlrk3588-ram-boot.sh` 重新从 RAM 加载同一个 FIT，使 RT-Thread
虚拟 plant 从 `300` 的相同初始状态开始。未执行 `flash`、`erase` 或 eMMC 写入。

## 固定实验条件

- 开发板：ATK-DLRK3588（RK3588），串口 1,500,000 baud。
- Hypervisor：AxVisor FP-RR。
- StarryOS 与 RT-Thread 各 1 个 vCPU，共享 `pCPU 0x100`。
- RT-Thread 优先级 90，StarryOS 优先级 89。
- AI：StarryOS Guest 内 AArch64 静态 ncnn + YOLO11n，CPU 推理，不使用 NPU。
- 通信：T2N1/UDP `CONTROL → ACK → STATUS`。
- 被控对象：RT-Thread 确定性非线性虚拟 plant。
- 输入：每轮相同的 5 张冻结真实图片及 SHA256；左/中/右 3 张应接受，
  无目标/小目标 2 张应安全拒绝。
- manual：固定发送 `target=500`，不执行模型推理。
- YOLO：逐图真实推理，将检测中心转换为控制目标，并应用最大步长安全限幅。

FIT、initramfs 和三份 TOML 在六轮 metadata 中具有完全相同的 SHA256。配置与
冻结输入 manifest 沿用 [`../atk-dlrk3588-task123-integrated-ab-20260824/`](../atk-dlrk3588-task123-integrated-ab-20260824/)。

## 六轮结果

| 轮次 | 模式 | 图片 | 完整 CONTROL/STATUS | target MAE | state MAE | 平均 RTT | 最大 RTT | 平均推理 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | manual | 5 | 5/5 | 93.00 | 70.67 | 250.6 ms | 282 ms | N/A |
| 2 | manual | 5 | 5/5 | 93.00 | 70.67 | 230.0 ms | 284 ms | N/A |
| 3 | manual | 5 | 5/5 | 93.00 | 70.67 | 253.2 ms | 291 ms | N/A |
| 1 | YOLO | 5 | 3/3 接受图片 | 26.33 | 89.00 | 217.7 ms | 301 ms | 1.6210 s |
| 2 | YOLO | 5 | 3/3 接受图片 | 26.33 | 89.00 | 245.0 ms | 323 ms | 1.6262 s |
| 3 | YOLO | 5 | 3/3 接受图片 | 26.33 | 89.00 | 221.3 ms | 287 ms | 1.6258 s |

跨三轮汇总：

| 指标 | manual 3 次 | YOLO 3 次 |
|---|---:|---:|
| 总图片数 | 15 | 15 |
| 完整 CONTROL/STATUS | 15/15 | 9/9 接受图片 |
| 原始感知中心 MAE | N/A | **0.00** |
| 安全限幅后 target MAE | 93.00 | **26.33** |
| 单步 STATUS state MAE | **70.67** | 89.00 |
| 平均 RTT | 244.6 ms | **228.0 ms** |
| P95 / 最大 RTT | **291 / 291 ms** | 323 / 323 ms |
| 平均推理 | N/A | 1.6243 s |
| P95 / 最大推理 | N/A | 1.6350 / 1.6350 s |
| 各轮平均推理标准差 | N/A | 2.90 ms |
| 接受/拒绝行为正确率 | N/A | **15/15，100%** |
| 安全拒绝率 | N/A | **6/6，100%** |

YOLO 将发送目标 MAE 从 `93.00` 降至 `26.33`，下降约 **71.7%**，并在三轮中
保持相同结果。15 次真实推理的平均值为 `1.6243 s`，各轮平均值标准差仅
`2.90 ms`。这支持“真实视觉提高目标选择准确性并稳定完成安全闭环”的结论。

平均 RTT 从 `244.6 ms` 降至 `228.0 ms`，但 YOLO 的 P95/最大 RTT 更高；本批次
不把 RTT 差异解释为模型或调度器带来的网络加速。YOLO 的单步 state MAE 仍高于
manual，因为 plant 有惯性且三张接受图片的目标连续改变。该指标证明动作和回传
真实发生，但不代表稳态控制质量；稳态比较需要固定时域、IAE、稳定时间和超调量。

## 测量方法、精度与边界

- `infer_us` 在 StarryOS Guest 同一时钟域内包围一次 ncnn 推理，避免跨 Guest
  时钟同步误差；日志精度为 1 us，但包含 runtime 调用和邻近时间戳开销。
- RTT 在 StarryOS 同一时钟域内从 `CONTROL` 发送到对应 request ID 的 `STATUS`
  收到为止，精度为 1 ms；它不包含 YOLO 推理时间。
- 每次接受请求都以 image ID、SHA256、request ID 串联推理、CONTROL 和 STATUS。
- 六轮 runner 均验证样本顺序、图片哈希、控制/状态数量、拒绝数量、FP-RR 计数、
  VM 运行状态以及 fatal marker 不存在。
- 这是一组重复物理板演示数据，不是硬实时 WCET、通用数据集 mAP、真实电机或 NPU
  性能证明。YOLO RTT 只有 9 个接受样本，P95/P99 为描述性 nearest-rank 统计。

## 证据与复现

- 原始启动、业务日志和 artifact metadata：[`logs/`](logs/)
- 每轮逐样本 CSV、逐轮汇总和跨轮汇总：[`metrics/`](metrics/)
- 全目录 SHA256：[`SHA256SUMS.txt`](SHA256SUMS.txt)
- 聚合器：[`../../scripts/test/net-dual-guest/task3_ab_3x3_metrics.py`](../../scripts/test/net-dual-guest/task3_ab_3x3_metrics.py)
- 单轮 runner：[`../../scripts/board/run-atk-task123-integrated-ab.py`](../../scripts/board/run-atk-task123-integrated-ab.py)

汇总命令：

```bash
python3 scripts/test/net-dual-guest/task3_ab_3x3_metrics.py \
  --manual results/atk-dlrk3588-task123-integrated-3x3-20260824/logs/manual-1-console.log \
  --manual results/atk-dlrk3588-task123-integrated-3x3-20260824/logs/manual-2-console.log \
  --manual results/atk-dlrk3588-task123-integrated-3x3-20260824/logs/manual-3-console.log \
  --yolo results/atk-dlrk3588-task123-integrated-3x3-20260824/logs/yolo-1-console.log \
  --yolo results/atk-dlrk3588-task123-integrated-3x3-20260824/logs/yolo-2-console.log \
  --yolo results/atk-dlrk3588-task123-integrated-3x3-20260824/logs/yolo-3-console.log \
  --out-dir results/atk-dlrk3588-task123-integrated-3x3-20260824/metrics
```
