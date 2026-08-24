# ATK-DLRK3588 Zephyr Guest 快速物理板验证

> **历史证据，已被统一 Zephyr 4.4.2 主链取代。**本文件保留早期 Zephyr 3.7.0
> 周期实验的来源和哈希，不能再用作当前 Task 1/2/3 完成度结论。当前对比见同目录
> `MULTI_RTOS_COMPARISON.md`；正式统一报告见
> `/home/huhu/atk-bringup/zephyr-task123-unified-20260824/UNIFIED_TASK123_REPORT.md`。

日期：2026-08-24  
硬件：同一块 ATK-DLRK3588（RK3588）  
启动：U-Boot → AxVisor → Zephyr Guest，全部 RAM-only，未写 eMMC  
范围：最小启动 smoke 1 次；真实 YOLO 压力下完成 2 个交错配对，即 RR 2 次 +
FP-RR 2 次。每次 300 样本；18,000 样本长实验留到无人值守窗口。

> 本结果证明的是 Zephyr **Guest** 在真实 RK3588 上运行，不是 Zephyr 原生
> RK3588 BSP，也不能替代评分项要求的裸机/原生 RTOS 基线。

## 实验拓扑和唯一变量

```text
StarryOS vCPU0（真实 ncnn/YOLO11n，priority 89） ─┐
                                                  ├─ RK3588 pCPU1 / MPIDR 0x100
Zephyr vCPU0（10 ms 周期探针，priority 90） ──────┘
```

RR 与 FP-RR 两臂的 StarryOS、Zephyr、模型、输入、initramfs、vCPU/pCPU、内存和
设备配置相同；唯一实验变量是 AxVisor Host scheduler feature：
`rr-scheduler` 或 `fp-rr-scheduler`。

Zephyr 3.7.0 使用现有 `qemu_cortex_a53` 虚拟平台，不需要原生 RK3588 BSP。为匹配
板上 AxVisor Guest 契约，构建 overlay 将 GICD/GICR 改为 `0xfe600000/0xfe680000`；
PL011 保持 `0x09000000`，Guest RAM 为 `0xA0000000`，入口为 `0xA0001044`。

## 启动 smoke

最小 Zephyr-only FIT 在真实板上成功输出：

```text
VM[2] boot success
VM[2] VCpu[0] running...
*** Booting Zephyr OS build 36940db938a8 ***
sequence 0..299
PERIODIC LATENCY COMPLETE samples=300
```

完整日志没有 `ESR_EL2`、panic 或 fatal IRQ。

## 真实 YOLO 压力下的两轮短 A/B

两臂均在 `TASK3_MODEL_READY` 和 `TASK3_INFER_STARTED` 后才向 Zephyr 发送 `g`。
Zephyr 完成 300 个 10 ms 样本后先停在导出屏障；runner 验证 YOLO 仍有新推理进展、
停止 AI，再发送 `d` 导出 CSV，避免串口导出进入测量窗口。

| 指标 | RR | FP-RR | 快速变化 |
|---|---:|---:|---:|
| 有效运行 | 2 | 2 | 4/4 完成 |
| 每次样本 | 300 | 300 | 序号均为 0..299 |
| 测量窗口中位数 | 3.002 s | 3.000 s | — |
| mean jitter 中位数 | 2.485 ms | 0.095 ms | 96.19% 下降 |
| P50 jitter 中位数 | 1.998 ms | 0.088 ms | 95.58% 下降 |
| P99 jitter 中位数 | 5.292 ms | 0.117 ms | 97.79% 下降，约 45.15× |
| P99.9 / max 中位数 | 6.499 ms | 1.747 ms | 73.12% 下降 |
| > 1 ms 中位数 | 229/300 | 1/300 | — |
| > 10 ms / deadline miss | 0 / 0 | 0 / 0 | — |
| YOLO inference mean 中位数 | 1530.173 ms | 1560.967 ms | 约 2.01% 增加 |
| YOLO inference samples | 5 | 5 | 两臂均持续前进 |

这是两轮短验证，只支持“Zephyr Guest 上也观察到相同优化方向”，不能把 45× 当作
长时间稳定统计结论。18,000 × 4 留到无人值守窗口执行。

## 观测污染及修正

第一版 Zephyr 在采样结束后立即打印 300 行 CSV。FP-RR 中高优先级 Zephyr 填满
虚拟控制台队列后停在第 80 行，低优先级 Host 控制台任务无法及时排空。这是导出路径
的优先级反转，不是周期采样卡死。

修正后 Zephyr 先输出 `PERIODIC LATENCY SAMPLING COMPLETE` 并等待 `d`；AI 停止后
才导出。这与已验证的 RT-Thread 两阶段协议一致。修正后 RR/FP-RR 均完整导出 300 行。
失败预跑保存在外部证据目录 `logs/diagnostics/`，没有从证据中删除。

## 与既有 RT-Thread 结果的边界

同一物理板此前的 RT-Thread + 真实 YOLO 3+3 结果为：P99 中位数 RR
`30.176 ms`、FP-RR `2.805 ms`，下降 `90.70%`。本轮 Zephyr 单次结果也显示下降，
说明优化方向跨 RTOS 复现。

但两组不能用来宣称“Zephyr 天生比 RT-Thread 快”：既有 RT-Thread 运行使用旧的
单次启动推理窗口，本轮 Zephyr 使用持续 model-loop；两种 RTOS 的内核、计时转换和
探针实现也不同。要做 RTOS 间绝对值排名，必须再用完全相同的持续负载和运行协议重跑
RT-Thread。

## 当时尚未完成、现在的状态

- Zephyr 4.4.2 Task 2 的 HEARTBEAT、CONTROL/ACK/STATUS、双端 pcap、blackout、Safe
  与恢复现已在真实板完成；
- Zephyr 4.4.2 Task 3 的 manual/真实多图片 YOLO 闭环现已在真实板完成；
- Zephyr/RT-Thread 完全同协议、同持续负载的严格横向 A/B 尚未执行；
- 原生 RK3588 Zephyr BSP 仍不存在，后续应先做限时 go/no-go 探测；
- 本历史轮 2+2、统一 4.4.2 主链 1+1 均不替代夜间 18,000 × 4 长统计。

RT-Thread/Zephyr 的提交用对比见 `MULTI_RTOS_COMPARISON.md`。

## 复现命令摘要

Zephyr Guest 构建关键参数：

```sh
ZEPHYR_BASE=/home/huhu/toolchains/zephyrproject/zephyr-36940db938a8f4a1e919496793ed439850a221c2 \
ZEPHYR_START_GATED=1 ZEPHYR_DUMP_GATED=1 ZEPHYR_SAMPLE_COUNT=300 \
ZEPHYR_EXTRA_OVERLAY=scripts/test/zephyr-periodic/atk-dlrk3588-axvisor.overlay \
scripts/test/rt-partition/build-zephyr-periodic.sh
```

AxVisor 分别使用 RR/FP-RR board TOML，VM 配置顺序为 StarryOS、Zephyr。FIT 使用：

```sh
scripts/board/atk-dlrk3588-ram-boot.sh <image.fit>
sudo -n python3 scripts/board/run-atk-task1-yolo-arm.py <console.log> \
  --scheduler <rr|fp-rr> --runtime-seconds 3 --expected-inferences 1 \
  --expected-samples 300 --period-ms 10 --periodic-guest zephyr
```

完整镜像、构建日志和原始串口日志保存在：

```text
/home/huhu/atk-bringup/zephyr-guest-smoke-20260824
```

## Artifact 身份

```text
source commit  eaa2bf9061e35c70378d3ed5c1f5bb70f64224fb
Zephyr commit  36940db938a8f4a1e919496793ed439850a221c2
Zephyr bin     6ba6a019a6d32db60fa41c12cdd72fd69cb174f26813a4ae914a693450f173e2
RR FIT         418c02fc75a683c77582ef21399f99440a8eb58b7df23cb636f5a95a3e05f027
FP-RR FIT      5220ca5b9ac3697e36971816deefb769269ce503281274f7fa4a703ca24db5ce
host DTB       de398456718997c321b0a9b44a817909a5a9394064965ef668e690c1c9917807
initramfs      15db5f595a75dd3418d76252013ff6b5d27b7bb4c8c85889567231b9867ee452
round-1 RR     731af90f90ce35c72b0c905484c8bb0355bd75c11dc676bcd723723b02d2abb3
round-1 FP-RR  bc575cce0eff69c9f893fd8d6991815da638500aca6799b5b55d973244fa1fd6
round-2 FP-RR  a38c97b39cd98e1ca27f1191f4dcc2307a5f48eb215a4368d69c7a2198ecf7bd
round-2 RR     8e8a2c37cd438550be5c4acc15f71576eff6b1a83912f95b39859cf559c003d5
smoke log      f4cafedc092d2a6ca3f35517f404ca223b21a8dd183ad60e39650ca0c1b6d7ab
```
