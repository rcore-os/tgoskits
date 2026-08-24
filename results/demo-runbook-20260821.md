# OpenRace 演示流程与 marker 清单（2026-08-21）

目标是用一条可复核的 5 分钟演示说明“模型 → 协议 → RTOS 控制 → 故障恢复”，
并明确哪些数字是 QEMU SIL。

## 演示顺序

1. 展示 `results/final-submission-scorecard-20260821.md`，说明 Task1 为
   26–27/30 的保守区间，两个缺口的修复边界，以及不宣称物理板 worst-case。
2. 展示 YOLO 模型契约和 hash：`results/task3/yolo/`，说明当前 Guest 采用
   `embedded:fixture-replay`，不是 Guest 内 ONNX runtime。
3. 运行或展示正常闭环目录
   `results/task3/switch/current-head-yolo-capture/`，按下列顺序定位 marker。
4. 展示协议故障证据：ACK-drop、out-of-order、invalid-parameter 三个目录，
   以及各自的专用 verifier PASS。
5. 展示 `results/bonus-path-audit-20260821.md`，诚实说明 StarryOS/NPU 和
   第二 RTOS 的外部资产/闭环证据缺口。

## 正常闭环 marker

```text
TASK2_READY
TASK3_MODEL_READY
TASK3_DETECTION 或 TASK3_MODEL_REJECTED
TASK3_INFER
TASK3_CONTROL_SENT
TASK2_CONTROL_RECEIVED
TASK3_CONTROL_APPLIED
TASK2_STATUS_SENT
TASK3_STATUS_RECEIVED
```

## Blackout 恢复 marker

```text
TASK3_CONTROL_SENT
TASK2_SAFE
TASK2_RECOVERED state=Active
TASK3_CONTROL_SENT       # 恢复后继续
TASK3_STATUS_RECEIVED    # 恢复后继续
```

## 协议故障 marker

```text
# ACK drop
PROXY_DROP ... kind=ack ... remaining=0
TASK2_RETRANSMIT
TASK2_DUPLICATE

# Out-of-order
PROXY_INJECT mode=out-of-order ... sequence=99
TASK2_PROTOCOL_ERROR out_of_order=99
TASK2_REMOTE_ERROR code=OutOfOrder

# Invalid parameter
PROXY_INJECT mode=invalid-parameter ... value=1001
TASK2_PROTOCOL_ERROR invalid_parameter ...  # Zephyr，或 invalid_payload（Rust）
TASK2_REMOTE_ERROR code=InvalidParameter
```

每个 marker 都必须由保存的日志和 pcap 支撑；QMP `quit` 本身不是闭环成功证明。
