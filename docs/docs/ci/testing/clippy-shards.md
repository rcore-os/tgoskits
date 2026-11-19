# Clippy完整矩阵与独立作业

`expand_clippy_checks`每次从当前Cargo workspace生成全部支持的检查。CI固定四个独立runner，作业内串行；checkout继续清理，不依赖上次任务的target，不使用Clippy增量包选择。

## 1. 发现与分组

发现、检查展开与任务分配是三步，分组权重不能成为第二份检查名单。新包遵循同一规则，无需登记到固定组。

### 1.1 新增包和配置

包必须属于根`Cargo.toml`的workspace成员或成员通配符。`workspace_packages`从Cargo metadata取全部成员；`expand_clippy_checks`按feature、声明目标和专用配置展开。`validate_package_coverage`拒绝有包未产生检查，除非它在明确的不支持清单中且输出原因。独立workspace不属于这次发现范围。

新增feature按原规则展开，不自动穷举所有feature组合；特殊构建组合在包内`package.metadata.clippy`声明。无法准备的配置直接失败。清单总数动态计算，不固定为某次验收的数量。

### 1.2 数量分配

`partition`将同包/target/env检查成块，较大域按32项切分，先安排较大的任务块，再依次放入当前检查数量最少的组；平局按组编号选择。各组继续按原检查ID顺序执行。一个包可分布在多个组，新增包可能使其他组归属改变，相同清单下分组确定。

CI固定四组，不维护四份包名单，也不依赖历史耗时文件。此前试验的耗时权重、文件和开关已移除，避免维护两套调度策略。axbuild脱离仓库仍直接使用同一算法。

## 2. 计划与判定

计划将检查对象、分组、Git身份、工具链和构建配置绑定在一起；执行和汇总重新核对本地完整检查目录与有效输入。

### 2.1 本地命令

`--emit-plan`必须与`--all`一起使用，默认四组；分组编号从1开始，所有组执行结束后才能汇总。计划仅由当前完整目录和组数生成。

```sh
cargo xtask clippy --all --emit-plan target/ci/clippy-plan.json --shards 4
cargo xtask clippy --execute-plan target/ci/clippy-plan.json \
  --shard 1 --result-file target/ci/results/shard-1.json
cargo xtask clippy --verify-results target/ci/clippy-plan.json \
  --results-dir target/ci/results
```

本地定向检查保留`--package`，全量可省略`--all`；Clippy的`--since`及其专属增量选择已删除。std测试、sync-lint及CI文档门禁仍有各自的有效差异分析。

### 2.2 失败传播

`CheckPlan::validate`和CI matrix生成都确认每个ID恰好分配一次。每项记录成功、失败或未执行，先失败则本分片后项停止。汇总拒绝错误身份、重复、缺组、缺项、失败或未执行；取消和超时不能被后续成功覆盖。

## 3. 工具分发与实验

`.github/workflows/clippy-checks.yml`负责规划、四组执行及汇总。每次构建一个当前SHA的tg-xtask分发副本，各job下载复用，不在随机runner寻找上次残留工具。

### 3.1 小体积工具制品

`scripts/test/ci_clippy_tool.py pack`只复制可执行文件，对副本去除调试信息并以xz低级别压缩；原调试构建保持。归档记录SHA256，接收方先验和，再确认只含唯一普通文件后原子还原可执行程序，拒绝损坏、链接或路径穿越。计划、工具、结果是不同制品，按run/attempt命名；已经压缩的工具归档关闭Actions外层压缩。

剥离副本减少工具自身崩溃的详细调试信息，必要时用同SHA未剥离构建排查。执行器仍使用原Cargo参数与warnings标准，汇总也继续用同一工具核对完整身份和结果，不另写宽松判定。xz解压由Python标准库lzma完成，运行环境必须提供此模块；缺失会明确失败。

### 3.2 同源实际对照

手动CI设置`clippy_only=true`，可选`clippy_shards=1/4`；这仅运行Clippy必要链，不执行QEMU、板卡或std矩阵。普通CI保留其他作业。

同SHA顺序比较完整检查集合、逐项实际命令、准备/传输/检查/汇总和端到端耗时，并记录runner资源。工具制品大小乘下载次数只是对应payload估计，不代表整个CI网络流量；数量平衡不保证耗时相同，不能将检查数直接换算成提速比例。

## 4. 触发与维护

PR的workflow路径规则先包含全部路径，再排除两个静态说明树，确保未知路径与未来新增顶层目录也能进入规划；无需另写源码目录白名单。`scripts/test/ci_clippy.py classify`只允许`docs/docs/`和`docs/blog/`已确认静态说明树跳过Clippy。其他代码、README、配置、脚本、依赖及未知输入均完整检查，历史不可用、merge-base等于HEAD或基准不确定也全量；其他OS任务仍由原规划器选取。

计划和结果写target或Actions制品，不加入Git。新增包不维护Clippy白名单；特殊排除必须给出理由。分组只改变排班，不改变功能覆盖；不得通过合并feature配置、忽略warnings或跳过错误制造提速。
