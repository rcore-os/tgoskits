# Clippy完整矩阵与独立作业

需要Clippy时，CI检查当前支持的全部workspace包及既有feature/target/configuration矩阵，不按变动包缩小范围。默认四个独立runner作业，checkout继续清理，不依赖上次任务的target。明确不支持的包继续按原规则输出原因。

## 1. 计划与执行

`expand_clippy_checks`仍是检查列表的单一来源。CI从当前精确提交生成计划，按包、target和编译环境聚合，较大的域按32项切块，再按成员数平衡分片；这是确定性初始策略，不是耗时预测。

### 1.1 本地命令

计划包含工具链、Git身份、构建配置摘要与每个分片的成员。执行时重新核对完整本地目录和有效输入，不能混用不同提交或机器路径。

```sh
cargo xtask clippy --all --emit-plan target/ci/clippy-plan.json --shards 4
cargo xtask clippy --execute-plan target/ci/clippy-plan.json \
  --shard 1 --result-file target/ci/results/shard-1.json
cargo xtask clippy --verify-results target/ci/clippy-plan.json \
  --results-dir target/ci/results
```

分片编号从1开始，逐个运行其余分片后才可汇总。没有足够非空小块时减少分片数，空矩阵不能自动通过。普通`--package`与本地`--since`保持。

### 1.2 失败判定

每个检查记录成功、失败或未执行，先失败则本分片后项不执行。取消或超时可能无法上传结果；汇总将缺失、外来、重复或未完成结果视为失败。真实Clippy告警不会通过重试、放宽warnings或换缓存隐藏。

## 2. GitHub调度

`.github/workflows/clippy-checks.yml`负责规划、独立runner矩阵与严格汇总，普通CI从`ci.yml`调用。每个job目录独立，复用原Cargo执行逻辑；作业内默认串行，避免多个并发层叠加。

### 2.1 单独实验

手动运行既有CI workflow时设置`clippy_only=true`，再选择`clippy_shards=1`或`4`。这只运行Clippy链及工具准备，不执行QEMU、板卡或std矩阵。普通push/PR不会因该实验参数关闭其他检查。

四组和一组对照必须使用同一精确SHA、同样完整矩阵，记录排队、准备、执行和整个工作流时间；理论除以4不是实际提速。默认不清理共享工具链或Cargo缓存。

### 2.2 改动分类

只有已确认的`docs/docs/`、`docs/blog/`静态说明树可跳过Clippy。README、Rust测试、CI、脚本、依赖及未知输入均保守运行完整矩阵；差异历史不可用时也全量。本轮不采用独立测试目标缩圈或跨任务持久缓存。

目录分类只影响Clippy；其他CI仍由原规划器选择。计划/结果JSON和工具归档放target或Actions制品，不加入Git。制品按run/attempt命名，默认整条Clippy链重跑；不会从其他运行“找最新结果”。
