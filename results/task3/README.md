# Task-3 证据归档

> 当前主证据不在本历史目录，而在
> `results/atk-dlrk3588-task123-integrated-ab-20260824/`。该目录记录了
> RK3588 物理板上的真实图片、Guest 内 AArch64 ncnn/YOLO、T2N1
> `CONTROL`、RTOS 虚拟 plant 动作以及同 request ID `STATUS`。
> 三张有效图片完成 3/3 闭环，两张无效图片完成 2/2 安全拒绝。
>
> 本目录以下内容仅保留早期 QEMU temporal-CNN、fixture replay、
> 协议故障和定量实验的历史证据。它们可用于说明协议、安全
> 路径和方案演进，但不得代替当前的真实 ncnn/YOLO 主链证据。

> 历史分支 `openrace/task3-ai-control`；当前设计入口见
> `docs/design/task3-ai-design.md`。

## 指标与对比（M4）

| 文件 | 内容 |
|---|---|
| `run-{1..3}.csv` | AI 模式原始日志 CSV |
| `run-{4..6}.csv` | baseline 模式原始日志 CSV |
| `summary.csv` | 逐组 RMSE/调节时间/超调/RTT 汇总 |
| `comparison.png` | AI vs baseline 响应曲线对比图 |

核心结论：整体 RMSE AI 29.2–29.3 vs baseline 190.6–191.3；t500 稳态误差
~2 vs ~192；Guest 推理 11.3 ms 均值 / 14.6 ms p95。

## 故障闭环（M5）

| 文件 | 内容 |
|---|---|
| `fault-final3-*.log` / `fault-final4-*.log` | guest + proxy 日志（黑障→Safe→恢复→续跑） |
| `fault-final3-*pcap` / `fault-final4-*pcap` | 双端数据面抓包 |
| `fault-final3.sha256` / `fault-final4.sha256` | 上述证据哈希 |

关键事件：黑障 25s→35s（代理丢 102 帧）→ 双方 Safe → 恢复 → 控制环续跑
（final4 恢复后 82 个 STATUS 周期、0 错误）。

## 模型（M3）

模型结构/权重哈希/训练器哈希见
`components/task3-model/model/model.json`（13,089 参数，~0.7M MACs，
`weights.bin` SHA-256 内联记录）；golden-vector / golden-window 测试随
`cargo test -p task3-model` 可复算。

## YOLO 感知补充

`results/task3/yolo/` 保存了 YOLO11n ONNX 的固定图片 fixture 结果。它验证
模型 hash、YOLOv8-style 输出解码、置信度/面积门限、中心位置到控制目标的
有界映射，以及低置信度无检测时的安全拒绝。运行命令和当前结果见该目录
的 README 与 `yolo-fixture-manifest.json`。

当前 AArch64 Guest 通过 `TASK3_MODEL=yolo` 使用一个确定性的 fixture replay
adapter：它重放同一 manifest 中的无检测、正常检测和大步长检测，并通过
`task3_model::perception` 的置信度/面积/坐标/单帧步长边界后再产生 CONTROL。
这验证了 YOLO 感知结果进入 T2N1 控制链路的安全接线；它仍不是 Guest 内的
ONNX runtime，fixture replay 的推理耗时不能当成真实 YOLO 推理性能。

编译时选择模型（默认行为保持兼容）：

```bash
TASK3_CONTROL_LOOP=1 TASK3_MODEL=cnn \
  bash scripts/test/net-dual-guest/build-linux-task2.sh
TASK3_CONTROL_LOOP=1 TASK3_MODEL=yolo \
  TASK3_MODEL_PATH=embedded:fixture-replay \
  TASK3_YOLO_MIN_CONFIDENCE_MILLI=600 \
  TASK3_YOLO_MIN_AREA_MILLI=10 \
  TASK3_YOLO_MAX_TARGET_STEP=100 \
  bash scripts/test/net-dual-guest/build-linux-task2.sh
```

controller 日志会输出 `TASK3_MODEL_READY`、`TASK3_DETECTION`、
`TASK3_MODEL_REJECTED`、`TASK3_INFER` 和带模型名的 `TASK3_CONTROL_SENT`。
当前 `yolo` 适配器只接受 `TASK3_MODEL_PATH=embedded:fixture-replay`；传入
其他路径会显式失败，避免把缺失的 ONNX 文件误报成 Guest 内推理。

当前 HEAD 的双 Guest QEMU YOLO 运行证据位于
`results/task3/switch/current-head-yolo-capture/`，对应记录见
`results/task3/yolo/current-head-validation-20260821.md`。该运行包含双端
pcap、QMP 正常退出、`TASK3_MODEL_READY`、检测/无检测拒绝、CONTROL 和
STATUS marker；pcap 必须通过 `verify_pcap.py --require-task2` 才可引用。
`serial_console.py dump-pcap` 现在会实际发送 `virtnet capture dump` 并写出
两侧 classic pcap，而不是只清空内存缓存。

最终 HEAD 的独立重放证据位于
`results/task3/switch/final-head-yolo-replay-v2/`。该运行使用 slot-0
Zephyr 镜像（`zephyr_binary_sha256 =
49ca61bc847835e61a03f94fb619fca01b49f02157d347b0fc0a9806f7fcb433`），双端
各导出 320 帧，其中 315 帧是可验证的 T2N1 UDP 帧；两侧 pcap 均通过
`verify_pcap.py --require-task2`。日志中的 `TASK3_MODEL_READY` 明确标注
`embedded:fixture-replay`，因此这条证据证明的是 YOLO 感知契约接入和双
Guest 控制闭环，不是 Guest 内 ONNX runtime 性能。

YOLO 故障恢复证据位于
`results/task3/switch/fault-current-head-yolo-fault-validated/`：黑障期间进入
`TASK2_SAFE`，恢复后出现 `TASK2_RECOVERED state=Active`，并继续产生
`TASK3_CONTROL_SENT`/`TASK3_STATUS_RECEIVED`。fault runner 在归档前强制检查
marker 顺序、恢复后的续跑、双端非空 pcap、T2N1 ledger 和 SHA256 manifest。
该证据同样是 QEMU SIL，不能解读为物理板硬实时或真实 ONNX 推理耗时。

最终 HEAD 的 YOLO blackout 重放位于
`results/task3/switch/fault-final-head-yolo-blackout-v2/`。它使用显式的
`yolo` initramfs，验证顺序为 `blackout ON → TASK2_SAFE → blackout OFF →
TASK2_RECOVERED`，恢复后继续运行至 `elapsed_ms >= 45000`；双端各有 727
帧、720 个 T2N1 帧，pcap verifier PASS。现在 fault runner 支持
`baseline|cnn|yolo` 参数且默认 `yolo`，避免旧的 `ai` 别名掩盖实际模型：

```bash
bash scripts/task3/run-task3-switch-fault.sh <label> yolo
```

当前 HEAD 的协议故障注入也已形成专门证据：

- `results/task3/fault-current-head-yolo-injection-out-of-order/`：代理在真实
  QEMU 数据面注入 `CONTROL sequence=99`，RTOS 记录
  `TASK2_PROTOCOL_ERROR out_of_order=99`，Linux 收到
  `TASK2_REMOTE_ERROR code=OutOfOrder`；
- `results/task3/fault-current-head-yolo-injection-invalid-parameter-v2/`：代理
  注入越界 `CONTROL value=1001`，RTOS 记录 invalid-parameter 拒绝，Linux
  收到 `TASK2_REMOTE_ERROR code=InvalidParameter`。
- `results/task3/fault-current-head-yolo-injection-out-of-order-v2/`：在最终提交
  HEAD 上重新执行的 out-of-order 证据，manifest 的 `git_head` 与最终提交一致。

两次运行均通过 `verify_protocol_injection.py`，并保存双端 pcap、guest/proxy
日志和 SHA256 manifest。这里验证的是协议错误传播和安全拒绝，不把注入帧当作
正常控制输出。

## 当前 HEAD 三模式量化结果

baseline、CNN、YOLO fixture replay 已在同一固定场景下交错各运行 3 次，原始
日志、双端 pcap、每次 manifest 和汇总脚本输出见
`results/task3/quant-20260821/` 以及
`results/task3/switch/quant-{baseline,cnn,yolo}-{1,2,3}/`。报告明确区分了
模型 target 跟踪误差、冻结场景误差、RTT、settling、超调、检测/拒绝次数和
fixture replay overhead；YOLO 的 `infer_us` 不当作真实 ONNX 推理性能。

## 构建与运行命令

见 `docs/design/task3-ai-design.md` §8（历史复现流程）。
