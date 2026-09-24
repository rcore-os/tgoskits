# LTP netstress nightly benchmark

这个目录只保存 LTP `netstress` 的 nightly benchmark 变体：`qemu-x86_64-benchmark.toml`
要求 bench 模式，`prebuild.sh`、runner 和 LTP 构建流程随 benchmark 一起自包含，不依赖
`apps/starry` 下的 smoke 用例。功能 smoke 仍位于
`apps/starry/qemu/ltp-netstress`，负载模型和失败判定见该目录的 README。

```bash
cargo xtask starry app qemu \
  -t benchmark/qemu/ltp-netstress \
  --arch x86_64 \
  --qemu-config qemu-x86_64-benchmark.toml
```
