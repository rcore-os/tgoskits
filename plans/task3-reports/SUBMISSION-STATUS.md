# Task 3 交卷状态摘要

> 最后更新：2026-08-02  
> 实施记录：[`../task3-实施记录.md`](../task3-实施记录.md)  
> 对比报告：[`task3-compare-20260802T014310Z.md`](./task3-compare-20260802T014310Z.md)

## 已完成（可复现）

| 项 | 证据 |
|---|---|
| RTOS PID 快环 | `icpc-pid-plant.*` + `icpc-peer-server.c`（100×1ms 步进） |
| Linux 慢环（MLP/ONNX 权重） | `task3-ai-loop-client.c` + `task3-mlp.{h,weights.h}` |
| 端到端闭环 | `task3-pid-loop` PASS |
| T1/T2 延迟打点 | icpc header `timestamp_ns`，日志 `t1_ns`/`t2_ns`/`oneway_us` |
| 固定 vs AI 对比 | `task3-pid-compare` PASS + 对比报告 |
| Task 2 回归 | `icpc-smoke` / `task3-pid-loop` |

一键复现：

```bash
./scripts/task3/run-task3-pid-loop.sh
./scripts/task3/run-task3-compare.sh
```

## 关键数据（fixed vs AI，同次 compare 运行）

| 指标 | fixed | AI |
|---|---:|---:|
| RMSE | 53.5 | 18.5 |
| final err | 41.3 | 9.4 |
| p99 oneway (us) | 3667 | 2872 |

## 仍缺项

| 项 | 说明 |
|---|---|
| ONNX Runtime 运行时 | 当前为离线 MLP 权重（等价 ONNX 导出） |
| ±2% settling 时间 | 90 轮内未进入 ±2% 带，`settle_loops=-1`；闭环 PASS 用 ±6 判定 |

## 赛题评分对照（自评）

| 评分项 | 自评 | 说明 |
|---|---|---|
| 双环架构 | ✅ | 1ms 快环 + 100ms MLP 慢环 |
| icpc 下发/回传 | ✅ | CTRL setpoint + STATE y/err/kp |
| T1/T2 延迟 | ✅ | 方法 1：header 时间戳 + RTT/2 |
| 固定 vs AI ≥2 项 | ✅ | RMSE、final err、p99 oneway |
| AI 慢环 | 🚧 | 离线 MLP，非 ORT 运行时 |
