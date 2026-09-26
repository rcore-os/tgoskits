# 最新源码四 crate PGO 筛查（2026-09-25）

源码基点为 `dev@714accd8f636c540b2c3554b0b1e5cb885be42a4` 加现有
AArch64 `memset` 训练修复，实验 HEAD 为
`b292a098bb60ef604e7677c37cd95d926ff08200`。冻结 benchmark SHA256
为 `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`，
Linux RT 数值沿用冻结基线。训练和 full20 均在 OrangePi-5-Plus-1 上进行；
候选与普通镜像均关闭 cpufreq feature。

## 训练与构建

`resume873` 的训练专用 exporter 放在被 profile 的 `starry_kernel` 中。
训练取得 789760 字节计数器和 18127 函数的 LLVM IR profile，但 27 个
训练场景行中 8 行缺样，不能当作 full20 验收；取消 exporter 后的 crate
身份/CFG 也不能直接视为匹配。`resume874` 仅保留空 Cargo feature 的
构建出现 359 个 CFG mismatch、5273 个函数无 profile，`resume875`
改用 `--cfg` 仍使两个热 futex 函数的 CFG hash 不匹配；二者未上板验收。

`resume876` 的**临时实验补丁**见 `source.patch.gz`，原始补丁 SHA256
为 `44866a74273e74d8656e1e5130e0f0997f1f39851c204ec583952e5fbbef1cff`。
它将计数器读取移到顶层 `starryos` binary，并通过训练时注册的 debugfs
provider 导出，不改变训练和候选的 `starry_kernel` 源码及编译 feature。
训练镜像 SHA256 为
`c5f64e52533111e0df0d695a573fb7990b11e9a8076b72050b97b4bfdb8fe517`；
其 789952 字节计数器 SHA256 为
`e6aacda9484f5409ace2d20dc63bd5cc3b35aa8b137e0701eb613156ec47b418`，
profile SHA256 为
`1c42e8712a55bc92e9d19a9b31ed881101b34614b8b4f89f6ed7524c347787e9`。
`compare-elf.json` 记录普通 profile-generate 对照的 16887 个记录全部在
训练 ELF 中找到且 hash/count 无不匹配；profile-use 构建没有函数缺失或
CFG 不匹配警告，候选 ELF 不含 `__llvm_prf_*` section。训练过程有
519981/520000 样本、19 次 `not_parked` 和 8 个无效场景行，只用于取得
profile，不是有效的 full20 验收启动。

实验补丁含训练专用的裸指针计数器读取，不是 PR 的生产运行时改动。
`cargo fmt`、`cargo xtask clippy --package starry-kernel`（72/72）和
`cargo xtask clippy --package starryos`（12/12）在隔离工作树通过。

## 无插桩 full20

同源码普通 release 镜像 SHA256 为
`ad77bbb56abeeafedb90b767ff7f226cf99df757b838e97526b031c34757e747`；
四 crate 选择性 PGO 候选镜像 SHA256 为
`4a872387073a948bf1416479164f7e6269e25d40eec8590ac26eba07cc8467d8`。
执行顺序 A1、F1、F2、A2，每次独立启动。A1、F1、F2 各有 20/20
有效场景、380000/380000 样本和零 `not_parked`；A2 的 OTHER
`thread_futex_same_cpu` 仅 19999/20000 且 `not_parked=1`，整次 A2
作废，不计入同源码回退门槛。F1/F2 的原始 p50 中位数仅 **10/20** 项
达到 Linux RT 的 90%；最差项仍是 OTHER `thread_futex_same_cpu`：
RT 8458 ns，F1/F2 均为 14875 ns，RT/F 为 **56.861%**。

因此这组候选**拒绝**：三次有效候选启动未完成，同源码普通 release 的
全项 p50/p99/p99.9 回退 `<3%` 未证明，生产可复现构建门也未更新。
不能用跨源码历史数据或训练镜像数值宣称优化收益；PR 继续保持 Draft。

运行 `sha256sum -c SHA256SUMS` 和 `python3 check.py` 可核对本目录的
归档哈希、原始 full20 日志、有效性及 90% 比例。镜像、ELF、原始计数器
和 profdata 体积较大，未纳入 Git；这里只保留其 SHA256、配置、构建日志、
原始串口与实验补丁，无法仅靠此目录重算缺席二进制的哈希。
