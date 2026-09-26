# AArch64 同地址空间切换筛查

## 1. 试验范围

本轮在 `b292a098bb` 上临时修改 `axruntime::thread::address_space` 的
`prepare_runtime_address_space_switch()`：同一逻辑用户地址空间的直接切换，
只有在 AArch64 当前硬件根与目标根相等时才进入 `SameUser` 路径；内核 lazy
切换留下保留根的情况仍走原路径。`source.patch` 是完整实验补丁，**没有合入
生产源码**。相邻的 `build-candidate.log`、`build-control.log` 和 `ordinary.toml`
记录了同源码普通 release 构建；`qemu-user-entry.log` 是候选的 AArch64
用户态入口测试，不能替代原生性能验证。

### 1.1 构建身份

普通 A 镜像 SHA-256 为
`31d68c9c739af52722d8596cdafd52796b571ee26d690a8b2a174aeb04bc1ca9`，
候选 B 为
`08bdacb5f6515c169ad59687f7c7945f93b557e78370839aee237bab8bb39497`。
两者使用同一份 `ordinary.toml`，没有 PGO、qperf 或 cpufreq feature。
实验补丁 SHA-256 为
`d8c8b95ed5a82fb082054cc3522496f61f443c722328239d28b995896b17bb94`。
镜像本体不入 Git；哈希只标识当时运行的字节，不能单独证明未来构建复现。

### 1.2 板测顺序

OrangePi-5-Plus-1 使用冻结 benchmark SHA-256
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`，
按 A1、B1、B2、A2 四次独立启动执行无插桩 full20。
`full20/` 保存每次原始日志、串口、会话与解析结果；`board.py` 保存当时的
板卡执行脚本，其中本地路径为实验环境路径，归档后不能直接重放。

## 2. 测量结果

`full20/results.json` 对每轮分别判定有效性。A1、B2、A2 均有 20 个唯一项、
380000/380000 样本且零 `not_parked`、零漏唤醒。B1 的 OTHER
`thread_futex_same_cpu` 只有 19999/20000 样本、`not_parked=1`，因此整个
B1 作废；不能用它的其余 19 项补齐候选数据。

### 2.1 目标项

唯一有效候选 B2 的 OTHER 同核 futex p50 为 26250 ns，A1/A2 p50 中位数
为 27854 ns，探索性差值为 -5.759%。B2 的该项仅为冻结 Linux RT
8458/26250 = **32.221%**；20 项中只有 **10/20** 项达到 90%。这是普通
release 的单次有效候选，不是现有五 crate PGO 的结果，也不能替换 PR 中
已归档的 PGO 11/20 结论。

### 2.2 回退筛查

单次 B2 相对 A1/A2 中位数，在 20 项的 p50、p99、p99.9 共 60 个比较中，
有六项至少回退 3%。其中 FIFO `absolute_timer_same_cpu` 的 p99.9
高 19.766%，FIFO `futex_wait_mismatch` 的 p99 高 16.686%。
这足以拒绝当前候选，但由于 B 只有一次有效启动，不能当作正式的两次
候选回退门禁结果。临时运行时代码已撤回，PR 保持 Draft，90% 最终目标未完成。

## 3. 离线复核

在本目录执行 `python3 check.py` 可从四份原始日志重新核对元数据、样本、
有效性、Linux RT 比率和回退筛查。脚本会明确要求 B1 无效，并校验
`source.patch`、`ordinary.toml` 及原始日志的 SHA-256。该离线检查不等于
实体板复测，也不能证明未入库镜像的生产构建复现。
