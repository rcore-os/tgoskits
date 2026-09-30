# compile-sim-bench nightly benchmark

这个目录只保存 Compile Simulation 的 nightly benchmark 变体：`qemu-x86_64-benchmark.toml`
选择 `--benchmark` 模式，`prebuild.sh` 与源码随 benchmark 一起自包含，不依赖
`apps/starry` 下的 smoke 用例。功能 smoke 仍位于
`apps/starry/qemu/compile-sim-bench`，负载模型和结果格式见该目录的 README。

`compile-sim-bench.c`、`compile-sim-bench-run.sh`、`prebuild.sh`、
`linux-compile-sim-init.sh` 和 `build-x86_64-unknown-none.toml` 与
`apps/starry/qemu/compile-sim-bench` 中的同名文件是刻意镜像的共享载荷，必须逐字节一致。
`scripts/test/check_ci_routing.py` 会在 Plan CI 中检查这五个文件，任一缺失或内容分叉都会让 CI
失败；修改共享载荷时必须同步更新两个目录。

通过 xtask 显式选择 nightly 用例：

```bash
cargo xtask starry app qemu \
  -t benchmark/qemu/compile-sim-bench \
  --arch x86_64 \
  --qemu-config qemu-x86_64-benchmark.toml
```
