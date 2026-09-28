# LTP hackbench nightly benchmark

这个目录只保存 LTP `hackbench` 的 nightly benchmark 变体：`qemu-x86_64-benchmark.toml`
要求 `--benchmark` 模式，`prebuild.sh`、affinity helper 和 runner 随 benchmark 一起自包含，
不依赖 `apps/starry` 下的 smoke 用例。功能 smoke 仍位于
`apps/starry/qemu/ltp-hackbench`，测量协议和输出 marker 见该目录的 README。

```bash
cargo xtask starry app qemu \
  -t benchmark/qemu/ltp-hackbench \
  --arch x86_64 \
  --qemu-config qemu-x86_64-benchmark.toml
```
