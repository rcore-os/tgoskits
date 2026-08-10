# Task 3 实施进度记录

> 智能化工控虚拟化擂台赛 · 任务三：AI 模型与控制联动  
> 主文档：[os/axvisor/doc/task3-ai-control.md](../os/axvisor/doc/task3-ai-control.md)  
> 交卷状态：[plans/task3-reports/SUBMISSION-STATUS.md](task3-reports/SUBMISSION-STATUS.md)

---

## 进度总览

| 里程碑 | 状态 | 说明 |
|---|---|---|
| M3.0 方案与依赖 | ✅ | 复用 Task 2 icpc + 双 Guest 拓扑 |
| M3.1 RTOS 快环 PID 仿真 | ✅ | Guest B 每 CTRL 步进 100×1ms |
| M3.2 Linux 慢环 AI 调参 | ✅ | MLP 慢环（离线权重） |
| M3.3 端到端闭环测试 | ✅ | `task3-pid-loop` PASS |
| M3.4 T1/T2 延迟打点 | ✅ | icpc header + `oneway_us` |
| M3.5 固定 vs AI 对比报告 | ✅ | `run-task3-compare.sh` |
| M3.6 ONNX Runtime | ⬜ | 离线 MLP 替代 ORT |
| M3.7 交卷材料 | 🚧 | 对比报告已生成 |

---

## 2026-07-31 阶段一（PID + 慢环闭环骨架）

### 交付

| 组件 | 路径 | 行为 |
|---|---|---|
| PID 被控对象 | `scripts/task2/icpc-pid-plant.{h,c}` | 一阶惯性 + PID，1ms 离散步进 |
| Guest B 集成 | `icpc-peer-server.c` | 快环 fork；CTRL 含 `setpoint=` 时回 plant 状态 |
| Linux 慢环 | `scripts/task2/task3-ai-loop-client.c` | 100ms 规则调 Kp，读 STATE 闭环 |
| 测试 | `test-suit/axvisor/normal/task3-pid-loop/` | 误差收敛判定 |

### 协议载荷（Task 3 扩展）

- **CTRL_CMD**：`kp=..,ki=..,kd=..,setpoint=..`（无 `setpoint=` 时保持 Task 2 兼容 `state=ok`）
- **STATE_REPORT**：`y=..,err=..,kp=..,ki=..,kd=..,tick=..`

### 验证

```text
task3-pid-loop PASS — first_err≈74 final_err≈5.2 final_y≈95
icpc-smoke PASS（Task 2 回归）
```

---

## 2026-08-02 阶段二（T1/T2 + 固定 vs AI 对比）

### 交付

| 组件 | 行为 |
|---|---|
| `task3-ai-loop-client.c` | 支持 `ai` / `fixed` / `compare`；T1/T2/oneway 打点；RMSE 指标 |
| `icpc-peer-server.c` | `reset=1` 重置 plant |
| `run-task3-compare.sh` | 一键对比 + 报告 |
| `task3-pid-compare` | axvisor 测试用例 |

### 对比结果（20260802T014310Z）

- RMSE：fixed 53.5 → AI 18.5
- final err：41.3 → 9.4
- p99 oneway：3667µs → 2872µs

---

## 下一步

1. （可选）接入 ONNX Runtime 替换离线 MLP
2. 延长慢环轮次或放宽 settling 采样以命中 ±2% 带
