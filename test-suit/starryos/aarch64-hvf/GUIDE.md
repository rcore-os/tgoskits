# aarch64-hvf 测试组

本组测试针对 aarch64 GICv3 + CNTV 通用定时器路径, 即 Apple HVF
profile 实际运行的运行时环境, 与默认 CI 跑的 GICv2 + CNTP 路径相互独立。

## 运行方式

```bash
cargo xtask starry test qemu --arch aarch64 --test-group aarch64-hvf
cargo xtask starry test qemu --arch aarch64 --test-group aarch64-hvf -c test-aarch64-gicv3-smoke
```

## 为什么单独成组

- 构建配置打开 `gic-v3` 和 `cntv-timer` 两个 feature, 并强制
  `devices.timer-irq=27` (CNTV PPI 11)。
- QEMU 参数固定为 `-machine virt,gic-version=3 -cpu cortex-a76`,
  与默认的 GICv2 + cortex-a53 完全不同。
- 默认 CI 命令 `cargo xtask starry test qemu --arch aarch64` 不带
  `--test-group`, 走 `normal` 分组, 因此本组对默认管线是 opt-in 的,
  不会拖累其他用例。
