# qperf

`apps/qperf/prebuild.sh` 使用本仓库 `tools/qperf` 中的插件和分析器源码，与
`cargo xtask starry perf` 共用同一实现。脚本不下载外部 harness kit；执行命令前
切换到本仓库根目录，因此命令中的 `tools/qperf/...` 相对路径不受调用目录影响。
插件要求 QEMU 11.1.1 的 API v7；不提供旧 API 的兼容构建。

单独构建插件和分析器时使用以下命令。StarryOS 的完整剖析流程使用
`cargo xtask starry perf`，不需要预先执行它们。

```bash
apps/qperf/prebuild.sh cargo build --manifest-path tools/qperf/Cargo.toml --release
apps/qperf/prebuild.sh cargo build --manifest-path tools/qperf/analyzer/Cargo.toml --release --features flamegraph
```

入口回归检查工作目录、参数保留、外部 checkout 隔离及退出状态传播：

```bash
apps/qperf/prebuild.sh python3 tools/qperf/tests/prebuild.py
```

不带参数执行 `apps/qperf/prebuild.sh` 会输出本仓库根目录。
`cargo xtask starry perf` 的报告后处理及 `apps/OScope-harness` 仍使用
`apps/common/prebuild-harness-kit.sh` 提供的固定外部 checkout；本次入口统一不改变它们。
