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

### 2.3 自动回归保护

`plan.rs`、`partition.rs`、`selection.rs`中的源码内单元测试由`cargo xtask test`标准入口执行，保护计划完整性、分组唯一性、结果身份及全部检查成功的规则。结果负例先建立完整有效的分组结果，再只修改一个条件；修改计划分配时重新计算摘要，确保漏项或重复确实由覆盖校验拒绝。临时Cargo workspace验证新增成员会被发现，不能在检查展开后整包消失。

这些持续回归与一次性完整矩阵验收分别保留：正常全量通过证明当前检查实际执行，负例测试防止以后误删拒绝逻辑后仍被正常CI放行。

## 3. 工具分发

`.github/workflows/clippy-checks.yml`负责规划、四组执行及汇总。每次构建一个当前SHA的tg-xtask分发副本，各job下载复用，不在随机runner寻找上次残留工具。

### 3.1 小体积工具制品

`scripts/test/ci_clippy_tool.py pack`只复制可执行文件，对副本去除调试信息并以xz低级别压缩；原调试构建保持。归档记录SHA256，接收方先验和，再确认只含唯一普通文件后原子还原可执行程序，拒绝损坏、链接或路径穿越。计划、工具、结果是不同制品；已经压缩的工具归档关闭Actions外层压缩。

剥离副本减少工具自身崩溃的详细调试信息，必要时用同SHA未剥离构建排查。执行器仍使用原Cargo参数与warnings标准，汇总也继续用同一工具核对完整身份和结果，不另写宽松判定。xz解压由Python标准库lzma完成，运行环境必须提供此模块；缺失会明确失败。

### 3.2 失败重跑与批次

准备作业使用`github.run_id`与该作业执行时的`github.run_attempt`生成批次编号，并通过job输出发布批次和实际计划、工具制品名称。后续作业只读取这些输出，不根据自己的重跑编号重新拼接名称。

只重跑失败组时，准备作业的输出保持，继续下载原计划和工具。每组结果按批次与组号命名，上传时覆盖本组旧结果，未重跑组的结果保留。汇总先要求准备及检查作业全部成功，再读取该批次所有分组结果并执行原严格校验。重跑失败、取消或上传失败不能由残留成功结果放行；只重跑汇总也不能绕过作业状态或结果完整性检查。

重跑全部作业时，准备作业生成新批次，旧批次结果不参与新汇总。制品保留7天；过期或缺失时明确失败，应重跑全部作业重新准备，不能自动借用其他批次的结果。

## 4. 触发与维护

PR的workflow路径规则先包含全部路径，再排除两个静态说明树，确保未知路径与未来新增顶层目录也能进入规划；无需另写源码目录白名单。`scripts/test/ci_clippy.py classify`只允许`docs/docs/`和`docs/blog/`已确认静态说明树跳过Clippy。其他代码、README、配置、脚本、依赖及未知输入均完整检查，历史不可用、merge-base等于HEAD或基准不确定也全量；其他OS任务仍由原规划器选取。

计划和结果写target或Actions制品，不加入Git。新增包不维护Clippy白名单；特殊排除必须给出理由。分组只改变排班，不改变功能覆盖；不得通过合并feature配置、忽略warnings或跳过错误制造提速。
