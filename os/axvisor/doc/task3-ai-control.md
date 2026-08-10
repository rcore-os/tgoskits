# Task 3：AI 模型与控制联动 — 实施指南

> 对应赛题任务三（25 分）与 `plans/技术方案.md` §3.3。  
> 依赖 Task 2 icpc 双 Guest 拓扑。

---

## 1. 双环架构

| 环 | 位置 | 周期 | 职责 |
|---|---|---|---|
| 快环 | Guest B（RTOS 仿真） | 1ms | PID + 一阶被控对象 |
| 慢环 | Guest A（Linux） | 100ms | MLP 输出 ΔKp/ΔKi/ΔKd |
| 下发 | icpc CTRL_CMD | — | 增益与设定值（header 带 T1） |
| 回传 | icpc STATE_REPORT | — | y、误差、当前增益（echo T1） |

## 2. 代码映射

| 交付物 | 路径 |
|---|---|
| PID  plant | `scripts/task2/icpc-pid-plant.{h,c}` |
| RTOS 集成 | `scripts/task2/icpc-peer-server.c`（含 `reset=1`） |
| Linux 慢环 | `scripts/task2/task3-ai-loop-client.c` + `task3-mlp.{h,weights.h}` |
| 闭环测试 | `test-suit/axvisor/normal/task3-pid-loop/` |
| 对比测试 | `test-suit/axvisor/normal/task3-pid-compare/` |

客户端模式：`task3-ai-loop [ai|fixed|compare]`

## 3. 验证

```bash
./scripts/task3/run-task3-pid-loop.sh
./scripts/task3/run-task3-compare.sh
cargo xtask axvisor test qemu --arch aarch64 -c icpc-smoke   # 回归 Task 2
```

## 4. T1/T2 延迟（方法 1）

- Linux 发送 CTRL_CMD 时在 icpc header 写入 `timestamp_ns`（T1）
- Guest B 回 STATE_REPORT 时 echo 同一 T1
- Linux 收到响应时记 T2，单向延迟 ≈ `(T2−T1)/2`
- 日志字段：`t1_ns`、`t2_ns`、`oneway_us`；汇总见 `TASK3_METRICS`

## 5. 待办

- （可选）ONNX Runtime 运行时替换离线 MLP
- 延长慢环轮次以命中 ±2% settling 带
