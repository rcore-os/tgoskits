# resume916：把训练计数器导出纳入源码

PR 本阶段合并 `dev@8af5f366988a9706413ec8a82d72cad2f4df64b8`，再提交 `profile-counter-export` 可选功能。普通 StarryOS 构建不启用该功能；没有修改调度、futex 或唤醒的生产执行逻辑。本目录记录训练设施的构建和板卡冒烟测试，**不是性能优化验收**。

独立源码提交 `ff944460ca0f8c05493fdf1a2ebb23f142c3b36c` 与 PR 中拣入的提交 `6c0e07fe6f`，在 `os/StarryOS/`、`Cargo.lock` 和 `scripts/axbuild/` 的最终树内容一致。该功能在 `main` 注册快照提供者，debugfs 有提供者时才创建 `/sys/kernel/debug/profile_counters`；首次成功读取会缓存快照。训练镜像必须对所有插桩 crate 使用 LLVM 原子计数器更新，且要在训练后首次读取文件（包括可能触发读取的长度查询）。附带的 TOML、wrapper 和脚本是本次实验的原件，含本机绝对路径及旧 profile，**不是当前 PR 可直接运行的生产构建入口**。

验证记录：

- `cargo fmt` 和 `git diff --check` 通过；`cargo xtask clippy --package starry-kernel` 76/76、`cargo xtask clippy --package starryos` 13/13 通过，后者覆盖新 feature。
- `cargo xtask starry build -c generate.toml` 与 `cargo xtask starry build -c use.toml` 均完成。训练 ELF 中 `__llvm_prf_cnts` 为 790272 字节，use ELF 没有 profile counter section。use 构建借用归档 `resume914` profile（SHA256 `445fd8229f286934bebd5f9eee53140fbc117738a04ef5d25b7150d4e15e7de8`），未出现控制流不匹配或缺失 profile 告警；已有 `ax_runtime::run_idle` partial-profile 告警仍存在。这只证明构建匹配，不证明延迟收益。
- 本地板卡服务的固定 `OrangePi-5-Plus-1` 在 816 MHz 寄存器检查后启动训练镜像，8 核启动；会话 `f6f0087c-dc32-4d5e-964f-3566658bbf52` 从 debugfs 读到 790272 字节并释放租约。`serial.log` 和 `result.json` 保留原始证据。前两次启动因宿主解析脚本分别误匹配终端回显、未容忍内核日志插入而被标为无效；串口原件留在本机 `resume916-exporter/board-smoke{,-2}/`，不是性能轮次。

| 文件 | SHA256 |
|---|---|
| `generate.toml` | `0f4a2f381fe8462072bf0c636e09f77559ed21ba8a9d90be539f6c736876d3a5` |
| `use.toml` | `3728f64d52127cf7eecb23f62677067c225de86a2d2cf223842b86f8622e067f` |
| `profile-wrapper.py` | `68bbcf5003e03174c9aa53f1c407cadc474a694767aa8677f4acc0b07c74b0a3` |
| `build-generate.log` | `d2097badeeb99fa7c3b5082aa26699f2b95f9a86fcfc314edc0887b067d4342c` |
| `build-use.log` | `03b64db525b21da0d7792a314ea6b9633c6f2088f9c78fd71ba6ae2835dd9662` |
| `serial.log` | `be643f88c1b9af3264cf7328d2a59480f6da4f14618e1806656eef278b719f49` |
| `result.json` | `9adb5817a7b2b4e3cc92556663d8272e2622e7874bd64262594f80234bd541ed` |

镜像未提交：训练 `generate.bin` SHA256 `979a60e788523e930500bbc3a93c30cf360b9aa7197c315f9c601b6292aaa975`；use `use.bin` SHA256 `e99972a2fea2b9a1f2ddda6c9c33b00fd3aff9d8c2417ca0052866f9d3ce621d`。归档在本机 `issue2308-perf/resume916-exporter/`。

最近一次有效、固定 816 MHz 的 full20 仍为旧源码 `resume914`：五 crate PGO 11/20 项达到 Linux RT p50 90%，最差 OTHER 同核 futex 14875 ns，对 Linux RT 8458 ns 为 56.861%。该结果来自 `363f6f825e` 加临时 exporter 补丁，不能计为此 PR 新 head 的验收。当前源码尚缺精确源码重新训练、同源码普通 release 全尾部分布对照、三次有效候选 full20、唤醒正确性及精确 head CI；PR 保持 Draft。
