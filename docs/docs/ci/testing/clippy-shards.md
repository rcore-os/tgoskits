# Clippy增量与全量分组

CI先使用同一个Rust选择器确定范围：可靠增量在一个作业内串行执行，只有显式全量或原分析回退全量时才使用四组。不是每次提交都完整检查，也不按检查数量自动增加并行。

## 1. 范围选择

`cargo xtask clippy --since <REF>`恢复原Git基线和输入分类。修改包与原反向消费者结果，再与声明的workspace依赖闭包取并集，包含optional、target-specific、build和dev依赖。中间消费者不再被“只保留少数OS根”的过滤排除；这是保守超集，公共包修改可能仍选出较多检查。

std、sync-lint使用的共享输入政策和测试期望保持。原lock混合变化、Markdown及包外输入分类没有在本次收紧，不能把“增量恢复”表述为已经修复所有输入覆盖问题。现有AxVisor定制构建包和chainloader等明确排除项继续输出理由。

用户暂缓了测试目标补齐和依赖它的test-only传播裁剪：本次不新增宿主测试profile、不把测试改动直接判定为独立，也不升级全量计划schema。原有feature、架构和命令保持，仍存在未明确检查所有测试目标的边界。

## 2. 本地命令与选择JSON

```sh
# 已提交差异的增量；未提交修改仍应显式选包
cargo xtask clippy --since origin/dev
cargo xtask clippy --package axbuild

# 本地默认全量仍是单作业
cargo xtask clippy --all

# 只选择，不执行；stdout恰一行JSON，诊断写stderr
cargo xtask clippy --print-selection --since origin/dev
```

`--since`与`--all`、`--package`及保存计划模式互斥。`--print-selection`不与`--package`、`--jobs`及保存计划模式混用。非Git环境的`--print-selection --since`报错；本地显式全量可无Git身份，CI必须有与checkout一致的SHA。

选择JSON的`mode`为full/incremental/empty，记录请求ref、实际diff base、受测SHA、输入指纹、修改包、消费者、实际检查和稳定原因码。`cut`保持空，因为没有启用独立性裁剪。摘要按规范化JSON的SHA256生成，集合按稳定身份排序、workspace路径归一化，Cargo参数/rustflags的顺序保持；执行前重新计算摘要拒绝漂移。

CI在准备作业构建一次工具，生成选择后用同一工具执行。增量传递原请求ref、冻结的`--diff-base <SHA>`和`--expected-selection <DIGEST>`；不重新解释浮动基线，也不允许增量执行意外转全量。没有选择Digest时，本地原增量命令仍允许正常全量回退。

## 3. CI模式与结果

主workflow传入现有since_ref，不改变push/PR去重。准备作业初始checkout深度100；缺基线时尝试单次fetch，无法解析共同祖先则全量。Git对象只属于当前作业，不能把主Plan取得的历史当作已传到Clippy作业。

| mode | 执行及通过条件 |
| --- | --- |
| incremental | 准备作业内按原串行入口执行全部选中检查，作业成功，四组未运行 |
| full | 四组成功，并用原逐项结果验证确认完整性 |
| empty | 原选择政策确定无相关支持检查；准备作业成功，明确记录原因 |
| 未知mode、规划错误、失败、取消、超时 | 失败，不靠skipped或旧结果放行 |

`workflow_call.inputs.incremental_timeout_minutes`默认120，主workflow显式配置；要求1—360的整数。它是初始运维上限，不是所有增量必在此时间内完成的保证。超时失败，不自动增组。手动workflow_dispatch可设置`clippy_full=true`运行完整四组排障；此结果不保证替代PR required checks，也不强制其他OS检查全量。

## 4. 全量计划、工具与批次

```sh
cargo xtask clippy --all --emit-plan target/ci/clippy-plan.json --shards 4
cargo xtask clippy --execute-plan target/ci/clippy-plan.json \
  --shard 1 --result-file target/ci/results/shard-1.json
cargo xtask clippy --verify-results target/ci/clippy-plan.json \
  --results-dir target/ci/results
```

完整目录由Cargo workspace及包内声明展开，不维护四份包清单。按package/target/env成块、最多32项，大块优先放入当前检查数最少组；同一清单分配确定。新包和配置自动纳入，每项恰好一次，缺包、缺项、重复、错身份、失败和NotRun均被拒绝。计划schema仍为1，因为测试目标扩展本轮暂缓。

工具只构建一次，对复制件去调试信息并以xz压缩，保留原调试构建。分发验证SHA256，只恢复唯一普通可执行文件，拒绝损坏、路径穿越与链接。不引入长期target缓存或历史耗时权重。

准备作业输出`run_id-preparation_attempt`批次和实际制品名称。失败分组重跑沿用生产者输出，仅overwrite本组结果；整轮重跑生成新批次。汇总同时检查作业状态和该批次完整结果，不使用残留成功掩盖取消或失败。制品保留7天，过期或缺失必须重新准备，不能借其他批次。

## 5. 回归与边界

原Clippy增量测试已恢复；源码内计划/汇总/包覆盖回归保持，并增加声明依赖超集、选择摘要与路径归一化测试。CI Python验证选择JSON和三种模式的实际状态门禁。外部CLI验收需包含A→B→C中的真实lint错误、optional/条件依赖、空选择、坏历史、输入变化及全量回退，不只比较检查数量。

本轮只恢复增量与中间消费者，不实现测试profile补齐、独立测试输入缩圈、额外输入收紧、push/PR重设计或跨提交结果继承。全量及增量检查仍使用原来的有效feature/架构矩阵，不声称穷举所有组合或覆盖所有特殊测试目标。
